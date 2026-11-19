//! One agent on an A2A server, as a tool. Exports `composable:tools/tool` and
//! calls the agent through `composable:a2a/client`, so an agent calls another
//! agent as it calls any tool. The server's URL comes from `wasi:config`.

wit_bindgen::generate!({
    path: "wit",
    world: "a2a-tool",
    generate_all,
});

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use wit_bindgen::rt::async_support::StreamReader;

use composable::a2a::client::{
    self, Agent, Error, SendMessageConfiguration, SendMessageRequest, SendMessageResponse,
};
use composable::a2a::types::StreamResponse;
use composable::agents::types::{Message, Part, PartContent, Role, Task, TaskState, TaskStatus};
use composable::tools::types::{
    AudioContent, CallToolResult, ContentBlock, ImageContent, MetaEntry, ResourceLink, TextContent,
    ToolAnnotations, ToolError, ToolMetadata,
};

const SERVER_URL_CONFIG: &str = "server-url";

// The tool's one argument, which is the message for the agent.
const INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"message":{"type":"string","description":"The message to send to the agent."}},"required":["message"]}"#;

// The media types the tool accepts for the parts of a response.
const ACCEPTED_OUTPUT_MODES: [&str; 2] = ["text/plain", "application/json"];

// The JSON-RPC code for invalid params.
const INVALID_PARAMS: i32 = -32602;

// The code the tools contract gives an implementation's failures.
const FAILURE: i32 = -32000;

// The agent, once connected.
struct Remote {
    agent: Agent,
    // Whether the agent's card declares streaming.
    streaming: bool,
}

// The agent's ids that later calls send back.
#[derive(Default)]
struct Conversation {
    // The agent's context, from its first response.
    context_id: Option<String>,
    // The task that is waiting for input, which the next call continues.
    task_id: Option<String>,
}

// The runtime instantiates the calling agent's composition for each of its
// invocations, so this state lasts one invocation of the calling agent.
thread_local! {
    static REMOTE: RefCell<Option<Rc<Remote>>> = const { RefCell::new(None) };
    static CONVERSATION: RefCell<Conversation> = RefCell::new(Conversation::default());
}

struct A2aTool;

impl exports::composable::tools::tool::Guest for A2aTool {
    async fn metadata() -> Result<ToolMetadata, String> {
        let remote = remote().await.map_err(|error| error.message)?;
        let card = remote.agent.card();
        Ok(ToolMetadata {
            name: card.name,
            title: None,
            description: Some(card.description),
            input_schema: INPUT_SCHEMA.to_string(),
            output_schema: None,
            annotations: Some(ToolAnnotations {
                title: None,
                read_only_hint: None,
                destructive_hint: None,
                idempotent_hint: None,
                open_world_hint: Some(true),
            }),
            meta: Vec::new(),
        })
    }

    // The `meta` a caller passes (such as `traceparent`) isn't forwarded,
    // because A2A carries trace context in HTTP headers, which the runtime
    // adds.
    async fn call(arguments: String, _meta: Vec<MetaEntry>) -> Result<CallToolResult, ToolError> {
        let text = message_argument(&arguments)?;
        let remote = remote().await?;
        let request = request(new_id(), text);
        let outcome = if remote.streaming {
            let stream = remote
                .agent
                .send_streaming_message(request)
                .await
                .map_err(tool_error)?;
            let outcome = drain(stream.responses).await;
            stream.result.await.map_err(tool_error)?;
            outcome
        } else {
            match remote
                .agent
                .send_message(request)
                .await
                .map_err(tool_error)?
            {
                SendMessageResponse::Task(task) => Outcome::Task(task),
                SendMessageResponse::Message(message) => Outcome::Message(message),
            }
        };
        remember(&outcome);
        result(outcome)
    }
}

// The agent, connected on first use.
async fn remote() -> Result<Rc<Remote>, ToolError> {
    if let Some(remote) = REMOTE.with_borrow(Clone::clone) {
        return Ok(remote);
    }
    let server_url = wasi::config::store::get(SERVER_URL_CONFIG)
        .ok()
        .flatten()
        .ok_or_else(|| failure(format!("no '{SERVER_URL_CONFIG}' configured")))?;
    let agent = client::connect(server_url, Vec::new())
        .await
        .map_err(tool_error)?;
    let streaming = agent.card().capabilities.streaming == Some(true);
    let remote = Rc::new(Remote { agent, streaming });
    REMOTE.set(Some(remote.clone()));
    Ok(remote)
}

// A request to send `text` as the message `message_id`, in the agent's
// context, to the task that is waiting for input if there is one.
fn request(message_id: String, text: String) -> SendMessageRequest {
    let (context_id, task_id) = CONVERSATION.with_borrow(|conversation| {
        (
            conversation.context_id.clone(),
            conversation.task_id.clone(),
        )
    });
    SendMessageRequest {
        message: Message {
            message_id,
            context_id,
            task_id,
            role: Role::User,
            parts: vec![Part {
                content: PartContent::Text(text),
                metadata: None,
                filename: None,
                media_type: None,
            }],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        },
        configuration: Some(SendMessageConfiguration {
            accepted_output_modes: ACCEPTED_OUTPUT_MODES.map(str::to_string).to_vec(),
            history_length: None,
            return_immediately: false,
        }),
        metadata: None,
    }
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

// The agent's response once all of it has arrived.
enum Outcome {
    // A message, with no task.
    Message(Message),
    // The task, with every update applied.
    Task(Task),
    // No response.
    Nothing,
}

async fn drain(mut responses: StreamReader<StreamResponse>) -> Outcome {
    let mut outcome = Outcome::Nothing;
    while let Some(response) = responses.next().await {
        outcome = apply(outcome, response);
    }
    outcome
}

// The outcome after one more response.
fn apply(outcome: Outcome, response: StreamResponse) -> Outcome {
    match response {
        StreamResponse::Message(message) => Outcome::Message(message),
        StreamResponse::Task(task) => Outcome::Task(task),
        StreamResponse::StatusUpdate(update) => {
            let mut task = task_of(outcome, &update.task_id, &update.context_id);
            task.status = update.status;
            Outcome::Task(task)
        }
        StreamResponse::ArtifactUpdate(update) => {
            let mut task = task_of(outcome, &update.task_id, &update.context_id);
            let artifact = update.artifact;
            let existing = task
                .artifacts
                .iter_mut()
                .find(|existing| existing.artifact_id == artifact.artifact_id);
            match existing {
                Some(existing) if update.append => existing.parts.extend(artifact.parts),
                Some(existing) => *existing = artifact,
                None => task.artifacts.push(artifact),
            }
            Outcome::Task(task)
        }
    }
}

// The task an update belongs to, which is the one received so far or, if the
// update came first, a task with the update's ids.
fn task_of(outcome: Outcome, task_id: &str, context_id: &str) -> Task {
    match outcome {
        Outcome::Task(task) => task,
        Outcome::Message(_) | Outcome::Nothing => Task {
            id: task_id.to_string(),
            context_id: Some(context_id.to_string()),
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: None,
            },
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: None,
        },
    }
}

// Keeps the ids later calls send back, which are the agent's context and a
// task that is waiting for input.
fn remember(outcome: &Outcome) {
    let (context_id, task_id) = match outcome {
        Outcome::Message(message) => (message.context_id.clone(), None),
        Outcome::Task(task) => (
            task.context_id.clone(),
            matches!(task.status.state, TaskState::InputRequired).then(|| task.id.clone()),
        ),
        Outcome::Nothing => return,
    };
    CONVERSATION.with_borrow_mut(|conversation| {
        if context_id.is_some() {
            conversation.context_id = context_id;
        }
        conversation.task_id = task_id;
    });
}

// The tool result for the agent's response.
fn result(outcome: Outcome) -> Result<CallToolResult, ToolError> {
    let (content, is_error) = match outcome {
        Outcome::Message(message) => (content_of(&message.parts), false),
        Outcome::Task(task) => {
            let status = task
                .status
                .message
                .as_ref()
                .map(|message| content_of(&message.parts))
                .unwrap_or_default();
            match task.status.state {
                TaskState::Completed => {
                    let artifacts: Vec<ContentBlock> = task
                        .artifacts
                        .iter()
                        .flat_map(|artifact| content_of(&artifact.parts))
                        .collect();
                    if artifacts.is_empty() {
                        (status, false)
                    } else {
                        (artifacts, false)
                    }
                }
                TaskState::InputRequired => (status, false),
                // Until auth exists, a task that requires it can't continue.
                TaskState::AuthRequired => {
                    (or_text(status, "the agent requires authorization"), true)
                }
                TaskState::Failed => (or_text(status, "failed"), true),
                TaskState::Rejected => (or_text(status, "rejected"), true),
                TaskState::Canceled => (or_text(status, "canceled"), true),
                // A2A's `SendMessage` waits for a terminal or interrupted
                // state unless `return-immediately` is set, which the tool
                // never sets.
                TaskState::Unspecified | TaskState::Submitted | TaskState::Working => {
                    return Err(failure(format!(
                        "the agent's response ended before task {} reached a terminal or interrupted state",
                        task.id
                    )));
                }
            }
        }
        Outcome::Nothing => {
            return Err(failure(
                "the agent's response ended without a task or a message".to_string(),
            ));
        }
    };
    Ok(CallToolResult {
        content,
        is_error,
        structured_content: None,
        meta: Vec::new(),
    })
}

fn content_of(parts: &[Part]) -> Vec<ContentBlock> {
    parts.iter().map(part_content).collect()
}

// One A2A part as a content block. A model can't read arbitrary file bytes, so
// a file that is neither an image nor audio is named, not included.
fn part_content(part: &Part) -> ContentBlock {
    match &part.content {
        PartContent::Text(text) | PartContent::Data(text) => text_block(text.clone()),
        PartContent::Url(url) => ContentBlock::ResourceLink(ResourceLink {
            uri: url.clone(),
            name: part.filename.clone(),
            description: None,
            mime_type: part.media_type.clone(),
            annotations: None,
            meta: Vec::new(),
        }),
        PartContent::Raw(data) => match part.media_type.clone() {
            Some(mime_type) if mime_type.starts_with("image/") => {
                ContentBlock::Image(ImageContent {
                    data: data.clone(),
                    mime_type,
                    annotations: None,
                    meta: Vec::new(),
                })
            }
            Some(mime_type) if mime_type.starts_with("audio/") => {
                ContentBlock::Audio(AudioContent {
                    data: data.clone(),
                    mime_type,
                    annotations: None,
                    meta: Vec::new(),
                })
            }
            mime_type => text_block(format!(
                "[file {} ({})]",
                part.filename.as_deref().unwrap_or("unnamed"),
                mime_type.as_deref().unwrap_or("unknown type")
            )),
        },
    }
}

fn text_block(text: String) -> ContentBlock {
    ContentBlock::Text(TextContent {
        text,
        annotations: None,
        meta: Vec::new(),
    })
}

// `content`, or `fallback` as text when the agent sent none.
fn or_text(content: Vec<ContentBlock>, fallback: &str) -> Vec<ContentBlock> {
    if content.is_empty() {
        vec![text_block(fallback.to_string())]
    } else {
        content
    }
}

// The `message` argument, or a `tool-error` when it is missing or not a
// string.
fn message_argument(arguments: &str) -> Result<String, ToolError> {
    let arguments: Value = serde_json::from_str(arguments).map_err(|e| ToolError {
        code: INVALID_PARAMS,
        message: format!("arguments are not JSON: {e}"),
        data: None,
    })?;
    arguments
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError {
            code: INVALID_PARAMS,
            message: "'message' must be a string".to_string(),
            data: None,
        })
}

// A failure to reach the agent, or a JSON-RPC error it returned. The agent's
// own code goes in `data`, since A2A's codes are in the range the tools
// contract reserves for implementations.
fn tool_error(error: Error) -> ToolError {
    let data = error.code.map(|code| {
        let details = error
            .data
            .as_deref()
            .and_then(|data| serde_json::from_str::<Value>(data).ok());
        json!({ "code": code, "message": &error.message, "data": details }).to_string()
    });
    ToolError {
        code: FAILURE,
        message: error.message,
        data,
    }
}

fn failure(message: String) -> ToolError {
    ToolError {
        code: FAILURE,
        message,
        data: None,
    }
}

export!(A2aTool);

#[cfg(test)]
mod tests {
    use super::*;
    use composable::agents::types::{Artifact, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};

    fn text_part(text: &str) -> Part {
        Part {
            content: PartContent::Text(text.to_string()),
            metadata: None,
            filename: None,
            media_type: None,
        }
    }

    fn message(text: &str) -> Message {
        Message {
            message_id: "m-1".to_string(),
            context_id: Some("ctx-1".to_string()),
            task_id: None,
            role: Role::Agent,
            parts: vec![text_part(text)],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }
    }

    fn status(state: TaskState, text: Option<&str>) -> TaskStatus {
        TaskStatus {
            state,
            message: text.map(message),
            timestamp: None,
        }
    }

    fn task(state: TaskState, text: Option<&str>) -> Task {
        Task {
            id: "task-1".to_string(),
            context_id: Some("ctx-1".to_string()),
            status: status(state, text),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: None,
        }
    }

    fn status_update(state: TaskState, text: Option<&str>) -> StreamResponse {
        StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: "ctx-1".to_string(),
            status: status(state, text),
            metadata: None,
        })
    }

    fn artifact_update(artifact_id: &str, text: &str, append: bool) -> StreamResponse {
        StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: "ctx-1".to_string(),
            artifact: Artifact {
                artifact_id: artifact_id.to_string(),
                name: None,
                description: None,
                parts: vec![text_part(text)],
                metadata: None,
                extensions: Vec::new(),
            },
            append,
            last_chunk: false,
            metadata: None,
        })
    }

    // The text of each text block.
    fn texts(content: &[ContentBlock]) -> Vec<String> {
        content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => text.text.clone(),
                _ => panic!("expected a text block"),
            })
            .collect()
    }

    fn outcome_of(responses: Vec<StreamResponse>) -> Outcome {
        responses.into_iter().fold(Outcome::Nothing, apply)
    }

    #[test]
    fn updates_are_applied_to_the_task() {
        let outcome = outcome_of(vec![
            StreamResponse::Task(task(TaskState::Submitted, None)),
            status_update(TaskState::Working, None),
            artifact_update("a-1", "Hel", false),
            artifact_update("a-1", "lo", true),
            artifact_update("a-2", "first", false),
            artifact_update("a-2", "second", false),
            status_update(TaskState::Completed, Some("done")),
        ]);
        let Outcome::Task(task) = outcome else {
            panic!("expected a task");
        };
        assert!(matches!(task.status.state, TaskState::Completed));
        // An appended chunk adds its parts, and any other replaces the artifact.
        assert_eq!(task.artifacts.len(), 2);
        assert_eq!(task.artifacts[0].parts.len(), 2);
        assert_eq!(task.artifacts[1].parts.len(), 1);
        let result = result(Outcome::Task(task)).unwrap();
        assert!(!result.is_error);
        assert_eq!(texts(&result.content), ["Hel", "lo", "second"]);
    }

    #[test]
    fn an_update_that_arrives_first_starts_the_task() {
        let Outcome::Task(task) =
            outcome_of(vec![status_update(TaskState::Completed, Some("done"))])
        else {
            panic!("expected a task");
        };
        assert_eq!(task.id, "task-1");
        assert_eq!(task.context_id.as_deref(), Some("ctx-1"));
    }

    #[test]
    fn a_message_is_the_result() {
        let result = result(outcome_of(vec![StreamResponse::Message(message("hi"))])).unwrap();
        assert!(!result.is_error);
        assert_eq!(texts(&result.content), ["hi"]);
    }

    #[test]
    fn a_completed_task_without_artifacts_gives_its_status_message() {
        let result = result(Outcome::Task(task(TaskState::Completed, Some("done")))).unwrap();
        assert!(!result.is_error);
        assert_eq!(texts(&result.content), ["done"]);
    }

    #[test]
    fn each_final_state_maps_to_a_result() {
        for (state, text, is_error, expected) in [
            (
                TaskState::InputRequired,
                Some("Which city?"),
                false,
                "Which city?",
            ),
            (TaskState::Failed, Some("it broke"), true, "it broke"),
            (TaskState::Failed, None, true, "failed"),
            (TaskState::Rejected, None, true, "rejected"),
            (TaskState::Canceled, None, true, "canceled"),
            (
                TaskState::AuthRequired,
                None,
                true,
                "the agent requires authorization",
            ),
        ] {
            let result = result(Outcome::Task(task(state, text))).unwrap();
            assert_eq!(result.is_error, is_error, "{state:?}");
            assert_eq!(texts(&result.content), [expected], "{state:?}");
        }
    }

    #[test]
    fn a_response_that_did_not_finish_is_a_tool_error() {
        for state in [
            TaskState::Submitted,
            TaskState::Working,
            TaskState::Unspecified,
        ] {
            let error = result(Outcome::Task(task(state, None))).err().unwrap();
            assert_eq!(error.code, FAILURE, "{state:?}");
            assert_eq!(
                error.message,
                "the agent's response ended before task task-1 reached a terminal or interrupted state"
            );
        }
        let error = result(Outcome::Nothing).err().unwrap();
        assert_eq!(
            error.message,
            "the agent's response ended without a task or a message"
        );
    }

    #[test]
    fn the_context_and_a_task_waiting_for_input_are_kept() {
        remember(&Outcome::Task(task(
            TaskState::InputRequired,
            Some("Which city?"),
        )));
        let continued = request("m-2".to_string(), "Paris".to_string());
        assert_eq!(continued.message.message_id, "m-2");
        assert_eq!(continued.message.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(continued.message.task_id.as_deref(), Some("task-1"));

        // A task that finished is not continued, and the context stays.
        remember(&Outcome::Task(task(TaskState::Completed, None)));
        let next = request("m-3".to_string(), "Thanks".to_string());
        assert_eq!(next.message.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(next.message.task_id, None);
    }

    #[test]
    fn parts_map_to_content_blocks() {
        let part = |content, filename: Option<&str>, media_type: Option<&str>| Part {
            content,
            metadata: None,
            filename: filename.map(str::to_string),
            media_type: media_type.map(str::to_string),
        };
        let data = part(PartContent::Data(r#"{"n":1}"#.to_string()), None, None);
        assert_eq!(texts(&[part_content(&data)]), [r#"{"n":1}"#]);

        let url = part(
            PartContent::Url("https://example.com/a.pdf".to_string()),
            Some("a.pdf"),
            Some("application/pdf"),
        );
        let ContentBlock::ResourceLink(link) = part_content(&url) else {
            panic!("expected a resource link");
        };
        assert_eq!(link.uri, "https://example.com/a.pdf");
        assert_eq!(link.name.as_deref(), Some("a.pdf"));
        assert_eq!(link.mime_type.as_deref(), Some("application/pdf"));

        let image = part(
            PartContent::Raw("aW1n".to_string()),
            None,
            Some("image/png"),
        );
        assert!(matches!(part_content(&image), ContentBlock::Image(image) if image.data == "aW1n"));
        let audio = part(
            PartContent::Raw("YXVk".to_string()),
            None,
            Some("audio/wav"),
        );
        assert!(matches!(part_content(&audio), ContentBlock::Audio(_)));

        let file = part(
            PartContent::Raw("emlw".to_string()),
            Some("a.zip"),
            Some("application/zip"),
        );
        assert_eq!(
            texts(&[part_content(&file)]),
            ["[file a.zip (application/zip)]"]
        );
        let unnamed = part(PartContent::Raw("Pz8/".to_string()), None, None);
        assert_eq!(
            texts(&[part_content(&unnamed)]),
            ["[file unnamed (unknown type)]"]
        );
    }

    #[test]
    fn the_message_argument_must_be_a_string() {
        assert_eq!(
            message_argument(r#"{"message":"hi"}"#).ok().as_deref(),
            Some("hi")
        );
        for arguments in [r#"{}"#, r#"{"message":1}"#, "not json"] {
            let error = message_argument(arguments).err().unwrap();
            assert_eq!(error.code, INVALID_PARAMS, "{arguments}");
        }
    }

    #[test]
    fn an_agents_error_keeps_its_code_and_data_in_the_tool_errors_data() {
        let error = tool_error(Error {
            message: "Task not found".to_string(),
            code: Some(-32001),
            data: Some(r#"[{"reason":"TASK_NOT_FOUND"}]"#.to_string()),
        });
        assert_eq!(error.code, FAILURE);
        assert_eq!(error.message, "Task not found");
        assert_eq!(
            error.data.as_deref(),
            Some(
                r#"{"code":-32001,"message":"Task not found","data":[{"reason":"TASK_NOT_FOUND"}]}"#
            )
        );

        // A failure that isn't a JSON-RPC error has only its message.
        let error = tool_error(Error {
            message: "HTTP request failed".to_string(),
            code: None,
            data: None,
        });
        assert_eq!(error.code, FAILURE);
        assert_eq!(error.data, None);
    }
}
