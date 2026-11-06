//! An agent that answers each message with one call to its model.

wit_bindgen::generate!({
    path: "wit",
    world: "chat-agent",
    generate_all,
});

use futures::StreamExt;
use wit_bindgen::rt::async_support::{StreamWriter, spawn_local};

use composable::agents::model_client::Session;
use composable::agents::types::{
    AgentError, AgentEvent, AgentFinishReason, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, AgentSkill, Artifact, ContentSource, MediaData, Message, ModelDelta,
    ModelExchange, ModelInputItem, ModelOutputItem, ModelRequest, ModelSettings, Part, PartContent,
    Role, TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, TokenUsage,
};

// Config keys used by this agent.
const INSTRUCTIONS_CONFIG: &str = "instructions";
const NAME_CONFIG: &str = "name";
const DESCRIPTION_CONFIG: &str = "description";
const VERSION_CONFIG: &str = "version";

// The media type for text this agent accepts and produces.
const TEXT_PLAIN: &str = "text/plain";

// The default media type for file bytes.
const OCTET_STREAM: &str = "application/octet-stream";

// Part metadata that marks a status message as the model's reasoning.
const REASONING_METADATA: &str = r#"{"reasoning":true}"#;

// JSON-RPC error codes used by this agent.
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

struct Component;

impl exports::composable::agents::agent::Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        let description = required_config(DESCRIPTION_CONFIG)?;
        Ok(AgentMetadata {
            name: required_config(NAME_CONFIG)?,
            description: description.clone(),
            provider: None,
            version: required_config(VERSION_CONFIG)?,
            documentation_url: None,
            default_input_modes: vec![TEXT_PLAIN.to_string()],
            default_output_modes: vec![TEXT_PLAIN.to_string()],
            skills: vec![AgentSkill {
                id: "chat".to_string(),
                name: "chat".to_string(),
                description,
                tags: vec!["chat".to_string()],
                examples: Vec::new(),
                input_modes: Vec::new(),
                output_modes: Vec::new(),
                security_requirements: Vec::new(),
            }],
            icon_url: None,
            extensions: Vec::new(),
        })
    }

    async fn invoke(request: AgentRequest) -> Result<AgentResponseStream, AgentError> {
        // This agent refuses a token budget it cannot enforce.
        if request.token_budget.is_some() {
            return Err(AgentError {
                code: INVALID_PARAMS,
                message: "this agent does not enforce token budgets yet".to_string(),
                data: None,
            });
        }
        let input = model_input(&request.message.parts);
        if input.is_empty() {
            return Err(AgentError {
                code: INVALID_PARAMS,
                message: "a message requires at least one part".to_string(),
                data: None,
            });
        }
        let settings = ModelSettings {
            instructions: get_config(INSTRUCTIONS_CONFIG),
            tools: None,
            tool_choice: None,
            output_schema: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            seed: None,
            stop_sequences: None,
        };
        let session = Session::new(&settings, &request.history).map_err(|e| AgentError {
            code: INVALID_PARAMS,
            message: format!("invalid history: {}", e.message),
            data: None,
        })?;

        let ids = TaskIds {
            task_id: request.task_id.unwrap_or_else(new_id),
            context_id: request.context_id.unwrap_or_else(new_id),
        };
        let (events_tx, events_rx) = wit_stream::new::<AgentEvent>();
        let (outcome_tx, outcome_rx) = wit_future::new::<Result<AgentResponse, AgentError>>(|| {
            Err(AgentError {
                code: INTERNAL_ERROR,
                message: "the agent stopped before its outcome".to_string(),
                data: None,
            })
        });

        let response = AgentResponseStream {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            activated_extensions: Vec::new(),
            events: events_rx,
            result: outcome_rx,
        };
        spawn_local(async move {
            let outcome = run(session, input, &ids, events_tx).await;
            let _ = outcome_tx.write(Ok(outcome)).await;
        });
        Ok(response)
    }
}

// The ids assigned to every event of the task.
struct TaskIds {
    task_id: String,
    context_id: String,
}

// Runs the task as one model call. Streams a `working` status, each run of
// reasoning as a `working` status message, and the text as chunks of one
// artifact, then returns the outcome. A dropped event stream cancels the task.
async fn run(
    session: Session,
    input: Vec<ModelInputItem>,
    ids: &TaskIds,
    mut events: StreamWriter<AgentEvent>,
) -> AgentResponse {
    let mut artifact = TextArtifact::new(new_id());
    if !send(&mut events, status_event(ids, TaskState::Working, None)).await {
        return outcome(
            TaskState::Canceled,
            artifact,
            session.exchanges(),
            unreported(0),
            None,
        );
    }
    let stream = match session
        .call(ModelRequest {
            input,
            max_tokens: None,
        })
        .await
    {
        Ok(stream) => stream,
        Err(e) => return failed(ids, &e.message, artifact, session.exchanges()),
    };

    let mut reasoning = ReasoningRun::default();
    let mut streamed_text = false;
    let mut deltas = stream.deltas.into_stream();
    while let Some(delta) = deltas.next().await {
        // A run of reasoning ends at the next delta of another kind.
        let ready: Vec<AgentEvent> = match delta {
            ModelDelta::Reasoning(chunk) => {
                reasoning.push(chunk);
                Vec::new()
            }
            ModelDelta::Text(chunk) => {
                streamed_text = true;
                let ended = reasoning.finish(ids);
                ended.into_iter().chain(artifact.push(chunk, ids)).collect()
            }
            ModelDelta::ToolCall(_) => reasoning.finish(ids).into_iter().collect(),
        };
        if !send_all(&mut events, ready).await {
            drop(deltas);
            return outcome(
                TaskState::Canceled,
                artifact,
                session.exchanges(),
                unreported(1),
                None,
            );
        }
    }
    // A run of reasoning also ends with the stream.
    if !send_all(&mut events, reasoning.finish(ids).into_iter().collect()).await {
        return outcome(
            TaskState::Canceled,
            artifact,
            session.exchanges(),
            unreported(1),
            None,
        );
    }

    let response = match stream.result.await {
        Ok(response) => response,
        Err(e) => {
            // Any text streamed before a failure remains the task's artifact.
            if let Some(event) = artifact.finish(ids) {
                send(&mut events, event).await;
            }
            return failed(ids, &e.message, artifact, session.exchanges());
        }
    };
    let usage = response.usage;
    let reason = Some(AgentFinishReason::Model(response.finish_reason));
    // If no reasoning deltas were streamed, send each run of reasoning in the
    // output as a status message, before the text.
    if !reasoning.streamed {
        let runs = reasoning_runs(&response.output)
            .into_iter()
            .map(|text| reasoning_event(ids, text))
            .collect();
        if !send_all(&mut events, runs).await {
            return outcome(
                TaskState::Canceled,
                artifact,
                session.exchanges(),
                usage,
                reason,
            );
        }
    }
    // A response that streamed no text deltas sends its whole text as one
    // chunk.
    if !streamed_text
        && let Some(event) = artifact.push(response_text(&response.output), ids)
        && !send(&mut events, event).await
    {
        return outcome(
            TaskState::Canceled,
            artifact,
            session.exchanges(),
            usage,
            reason,
        );
    }
    if let Some(event) = artifact.finish(ids)
        && !send(&mut events, event).await
    {
        return outcome(
            TaskState::Canceled,
            artifact,
            session.exchanges(),
            usage,
            reason,
        );
    }
    outcome(
        TaskState::Completed,
        artifact,
        session.exchanges(),
        usage,
        reason,
    )
}

// Usage with no counts, for `calls` number of model calls that did not report
// any. The `calls` value will be 0 when the task made no call, and 1 when its
// call failed or was canceled before the provider reported usage.
fn unreported(calls: u32) -> TokenUsage {
    TokenUsage {
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        unreported_calls: calls,
    }
}

// The task's artifact with the text sent so far. It holds the latest chunk
// until the next arrives, so that the last chunk sent is marked as such.
struct TextArtifact {
    artifact_id: String,
    // The text of the chunks sent so far.
    sent: String,
    held: Option<String>,
}

impl TextArtifact {
    fn new(artifact_id: String) -> Self {
        TextArtifact {
            artifact_id,
            sent: String::new(),
            held: None,
        }
    }

    // Hold `chunk`, returning the event that sends the chunk it replaces. An
    // empty chunk produces no event.
    fn push(&mut self, chunk: String, ids: &TaskIds) -> Option<AgentEvent> {
        if chunk.is_empty() {
            return None;
        }
        self.held
            .replace(chunk)
            .map(|previous| self.chunk_event(previous, false, ids))
    }

    // The event that sends the held chunk, if any, as the last chunk.
    fn finish(&mut self, ids: &TaskIds) -> Option<AgentEvent> {
        self.held
            .take()
            .map(|last| self.chunk_event(last, true, ids))
    }

    fn chunk_event(&mut self, chunk: String, last_chunk: bool, ids: &TaskIds) -> AgentEvent {
        let append = !self.sent.is_empty();
        self.sent.push_str(&chunk);
        AgentEvent::Artifact(TaskArtifactUpdateEvent {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            artifact: self.with_text(chunk),
            append: Some(append),
            last_chunk: Some(last_chunk),
            metadata: None,
        })
    }

    // The artifact as sent, or none if no chunk was sent.
    fn into_artifact(mut self) -> Option<Artifact> {
        if self.sent.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.sent);
        Some(self.with_text(text))
    }

    fn with_text(&self, text: String) -> Artifact {
        Artifact {
            artifact_id: self.artifact_id.clone(),
            name: None,
            description: None,
            parts: vec![text_part(text)],
            metadata: None,
            extensions: Vec::new(),
        }
    }
}

// Reasoning deltas collected since the last reasoning message was sent.
#[derive(Default)]
struct ReasoningRun {
    text: String,
    // Whether any reasoning delta arrived.
    streamed: bool,
}

impl ReasoningRun {
    fn push(&mut self, chunk: String) {
        self.streamed = true;
        self.text.push_str(&chunk);
    }

    // Takes the collected text, if any.
    fn take(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.text);
        (!text.is_empty()).then_some(text)
    }

    // A status event with the collected text, if any.
    fn finish(&mut self, ids: &TaskIds) -> Option<AgentEvent> {
        self.take().map(|text| reasoning_event(ids, text))
    }
}

// The joined text of each run of consecutive reasoning items, skipping runs
// without text (such as redacted reasoning).
fn reasoning_runs(output: &[ModelOutputItem]) -> Vec<String> {
    let mut runs = vec![String::new()];
    for item in output {
        match item {
            ModelOutputItem::Reasoning(reasoning) => {
                runs.last_mut().unwrap().push_str(&reasoning.text);
            }
            _ => runs.push(String::new()),
        }
    }
    runs.retain(|text| !text.is_empty());
    runs
}

// A `working` status with the model's reasoning as its message.
fn reasoning_event(ids: &TaskIds, text: String) -> AgentEvent {
    let message = Message {
        parts: vec![reasoning_part(text)],
        ..agent_message(ids, String::new())
    };
    status_event(ids, TaskState::Working, Some(message))
}

// The model's reasoning as a text part marked by `REASONING_METADATA`.
fn reasoning_part(text: String) -> Part {
    Part {
        metadata: Some(REASONING_METADATA.to_string()),
        ..text_part(text)
    }
}

// The outcome of a task that ended in `state`. Includes its artifact as sent,
// the exchanges its model call made, the usage it reported, and the finish
// reason if the call produced a response.
fn outcome(
    state: TaskState,
    artifact: TextArtifact,
    added_history: Vec<ModelExchange>,
    usage: TokenUsage,
    finish_reason: Option<AgentFinishReason>,
) -> AgentResponse {
    AgentResponse {
        status: TaskStatus {
            state,
            message: None,
            timestamp: None,
        },
        artifacts: artifact.into_artifact().into_iter().collect(),
        added_history,
        usage,
        finish_reason,
    }
}

// The outcome of a task whose model call failed, with the failure as the
// status message. The call reported no usage.
fn failed(
    ids: &TaskIds,
    error: &str,
    artifact: TextArtifact,
    added_history: Vec<ModelExchange>,
) -> AgentResponse {
    let mut response = outcome(
        TaskState::Failed,
        artifact,
        added_history,
        unreported(1),
        None,
    );
    response.status.message = Some(agent_message(
        ids,
        format!("the model call failed: {error}"),
    ));
    response
}

fn status_event(ids: &TaskIds, state: TaskState, message: Option<Message>) -> AgentEvent {
    AgentEvent::Status(TaskStatusUpdateEvent {
        task_id: ids.task_id.clone(),
        context_id: ids.context_id.clone(),
        status: TaskStatus {
            state,
            message,
            timestamp: None,
        },
        metadata: None,
    })
}

// Send an event, returning whether the caller still reads the stream.
async fn send(events: &mut StreamWriter<AgentEvent>, event: AgentEvent) -> bool {
    events.write_one(event).await.is_none()
}

// Send events in order, returning whether the caller still reads the stream.
async fn send_all(events: &mut StreamWriter<AgentEvent>, ready: Vec<AgentEvent>) -> bool {
    for event in ready {
        if !send(events, event).await {
            return false;
        }
    }
    true
}

fn agent_message(ids: &TaskIds, text: String) -> Message {
    Message {
        message_id: new_id(),
        context_id: Some(ids.context_id.clone()),
        task_id: Some(ids.task_id.clone()),
        role: Role::Agent,
        parts: vec![text_part(text)],
        metadata: None,
        extensions: Vec::new(),
        reference_task_ids: Vec::new(),
    }
}

fn text_part(text: String) -> Part {
    Part {
        content: PartContent::Text(text),
        metadata: None,
        filename: None,
        media_type: None,
    }
}

// A message's parts as model input. Includes text and structured data as text,
// and a file as media data with its specified media type, or
// `application/octet-stream` if not specified.
fn model_input(parts: &[Part]) -> Vec<ModelInputItem> {
    parts
        .iter()
        .map(|part| match &part.content {
            PartContent::Text(text) => ModelInputItem::Text(text.clone()),
            PartContent::Data(json) => ModelInputItem::Text(json.clone()),
            PartContent::Raw(data) => file_input(
                part.media_type.as_deref(),
                ContentSource::Data(MediaData {
                    mime_type: part
                        .media_type
                        .clone()
                        .unwrap_or_else(|| OCTET_STREAM.to_string()),
                    data: data.clone(),
                }),
            ),
            PartContent::Url(url) => {
                file_input(part.media_type.as_deref(), ContentSource::Url(url.clone()))
            }
        })
        .collect()
}

// A file as an image, audio, or video item based on its media type, otherwise
// a document.
fn file_input(media_type: Option<&str>, source: ContentSource) -> ModelInputItem {
    match media_type.and_then(|media_type| media_type.split('/').next()) {
        Some("image") => ModelInputItem::Image(source),
        Some("audio") => ModelInputItem::Audio(source),
        Some("video") => ModelInputItem::Video(source),
        _ => ModelInputItem::Document(source),
    }
}

// The text of a response's output.
fn response_text(output: &[ModelOutputItem]) -> String {
    output
        .iter()
        .filter_map(|item| match item {
            ModelOutputItem::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// A new id as a random (version 4) UUID. Two `get-random-u64` calls provide
// the 16 bytes, since `get-random-bytes` may return fewer than requested.
fn new_id() -> String {
    use wasi::random::random::get_random_u64;

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&get_random_u64().to_le_bytes());
    bytes[8..].copy_from_slice(&get_random_u64().to_le_bytes());
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

fn get_config(key: &str) -> Option<String> {
    wasi::config::store::get(key).ok().flatten()
}

// The config value `key`, which is required by this agent.
fn required_config(key: &str) -> Result<String, String> {
    get_config(key).ok_or_else(|| format!("no '{key}' configured"))
}

export!(Component);

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> TaskIds {
        TaskIds {
            task_id: "task-1".to_string(),
            context_id: "context-1".to_string(),
        }
    }

    // An artifact event's chunk text, `append`, and `last-chunk`.
    fn chunk(event: AgentEvent) -> (String, Option<bool>, Option<bool>) {
        let AgentEvent::Artifact(update) = event else {
            panic!("expected an artifact event");
        };
        assert_eq!(update.task_id, "task-1");
        assert_eq!(update.context_id, "context-1");
        assert_eq!(update.artifact.artifact_id, "artifact-1");
        let [
            Part {
                content: PartContent::Text(text),
                ..
            },
        ] = update.artifact.parts.as_slice()
        else {
            panic!("expected one text part");
        };
        (text.clone(), update.append, update.last_chunk)
    }

    fn text(artifact: Artifact) -> String {
        let [
            Part {
                content: PartContent::Text(text),
                ..
            },
        ] = artifact.parts.as_slice()
        else {
            panic!("expected one text part");
        };
        text.clone()
    }

    #[test]
    fn chunks_are_pushed_one_behind_with_the_last_chunk_marked() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());

        assert!(artifact.push("Hel".to_string(), &ids).is_none());
        assert!(artifact.push(String::new(), &ids).is_none());
        let first = artifact.push("lo".to_string(), &ids).unwrap();
        assert_eq!(chunk(first), ("Hel".to_string(), Some(false), Some(false)));
        let second = artifact.push("!".to_string(), &ids).unwrap();
        assert_eq!(chunk(second), ("lo".to_string(), Some(true), Some(false)));
        let last = artifact.finish(&ids).unwrap();
        assert_eq!(chunk(last), ("!".to_string(), Some(true), Some(true)));
        assert!(artifact.finish(&ids).is_none());

        assert_eq!(text(artifact.into_artifact().unwrap()), "Hello!");
    }

    #[test]
    fn a_single_chunk_is_the_first_and_the_last() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());

        assert!(artifact.push("Hi".to_string(), &ids).is_none());
        let last = artifact.finish(&ids).unwrap();
        assert_eq!(chunk(last), ("Hi".to_string(), Some(false), Some(true)));
    }

    #[test]
    fn an_artifact_with_nothing_sent_is_none() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());
        assert!(artifact.push("held".to_string(), &ids).is_none());

        // A held chunk that was never sent is not part of the artifact.
        assert!(artifact.into_artifact().is_none());
    }

    #[test]
    fn parts_become_model_input_by_media_type() {
        let part = |content: PartContent, media_type: Option<&str>| Part {
            content,
            metadata: None,
            filename: None,
            media_type: media_type.map(str::to_string),
        };
        let input = model_input(&[
            part(PartContent::Text("Describe this.".to_string()), None),
            part(PartContent::Data(r#"{"a":1}"#.to_string()), None),
            part(
                PartContent::Raw("iVBORw0KGgo=".to_string()),
                Some("image/png"),
            ),
            part(PartContent::Raw("ABC".to_string()), None),
            part(
                PartContent::Url("https://example.com/a.mp3".to_string()),
                Some("audio/mpeg"),
            ),
            part(
                PartContent::Url("https://example.com/a.mp4".to_string()),
                Some("video/mp4"),
            ),
            part(
                PartContent::Url("https://example.com/a.pdf".to_string()),
                None,
            ),
        ]);

        let expected = vec![
            ModelInputItem::Text("Describe this.".to_string()),
            ModelInputItem::Text(r#"{"a":1}"#.to_string()),
            ModelInputItem::Image(ContentSource::Data(MediaData {
                mime_type: "image/png".to_string(),
                data: "iVBORw0KGgo=".to_string(),
            })),
            ModelInputItem::Document(ContentSource::Data(MediaData {
                mime_type: OCTET_STREAM.to_string(),
                data: "ABC".to_string(),
            })),
            ModelInputItem::Audio(ContentSource::Url("https://example.com/a.mp3".to_string())),
            ModelInputItem::Video(ContentSource::Url("https://example.com/a.mp4".to_string())),
            ModelInputItem::Document(ContentSource::Url("https://example.com/a.pdf".to_string())),
        ];
        assert_eq!(format!("{input:?}"), format!("{expected:?}"));
    }

    fn reasoning(text: &str, redacted: bool) -> ModelOutputItem {
        ModelOutputItem::Reasoning(composable::agents::types::Reasoning {
            text: text.to_string(),
            redacted,
        })
    }

    #[test]
    fn a_run_of_reasoning_joins_its_deltas_until_it_ends() {
        let mut run = ReasoningRun::default();
        assert!(run.take().is_none());
        assert!(!run.streamed);

        run.push("Add ".to_string());
        run.push("them.".to_string());
        assert_eq!(run.take().as_deref(), Some("Add them."));

        // The next run starts empty, and reasoning still counts as streamed.
        assert!(run.take().is_none());
        assert!(run.streamed);
    }

    #[test]
    fn reasoning_is_a_text_part_marked_in_its_metadata() {
        let part = reasoning_part("Add them.".to_string());
        assert!(matches!(&part.content, PartContent::Text(text) if text == "Add them."));
        assert_eq!(part.metadata.as_deref(), Some(r#"{"reasoning":true}"#));
    }

    #[test]
    fn reasoning_runs_join_consecutive_items_and_skip_those_without_text() {
        let output = [
            reasoning("First ", false),
            reasoning("thought.", false),
            ModelOutputItem::Text("Hi".to_string()),
            reasoning("", true),
            ModelOutputItem::Text("!".to_string()),
            reasoning("Second.", false),
        ];
        assert_eq!(
            reasoning_runs(&output),
            ["First thought.".to_string(), "Second.".to_string()]
        );
        assert!(reasoning_runs(&[ModelOutputItem::Text("Hi".to_string())]).is_empty());
    }

    #[test]
    fn response_text_joins_the_text_items() {
        let output = [
            ModelOutputItem::Reasoning(composable::agents::types::Reasoning {
                text: "Think.".to_string(),
                redacted: false,
            }),
            ModelOutputItem::Text("Hello".to_string()),
            ModelOutputItem::Text(", World".to_string()),
        ];
        assert_eq!(response_text(&output), "Hello, World");
    }
}
