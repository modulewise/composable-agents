//! An agent for the server's integration tests. It has no model. What it does
//! with a message depends on the message's text:
//!
//! - `refuse`: it returns an `agent-error`, so there is no task.
//! - `fail`: the task fails.
//! - `ask`: the task is interrupted as `input-required`.
//! - `wait`: the task is `working` until it is canceled.
//! - `think`: it sends a `working` status with a message, then the task
//!   completes.
//! - `chunks`: it sends an artifact in two chunks, then the task completes.
//! - `no parts`: it sends an artifact with no parts, which A2A doesn't allow.
//! - `other task`: it sends an event with another task's id.
//! - any other text: the task completes with an artifact that has the text
//!   and the number of exchanges in the request's history.
//!
//! A message that continues an interrupted task completes the task, unless its
//! text is `wait`. The agent activates every extension a request asks for.

wit_bindgen::generate!({
    path: ["../../../../components/agents/wit", "wit"],
    world: "composable-agents:test-agent/test-agent",
    generate_all,
});

use composable::agents::types::{
    AgentError, AgentEvent, AgentInvocation, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, AgentSkill, Artifact, Message, Part, PartContent, Role,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use composable::models::types::{
    ModelExchange, ModelInputItem, ModelOutputItem, ModelRequest, TokenUsage,
};
use wit_bindgen::rt::async_support::{FutureReader, StreamWriter, spawn_local};

const CONTENT_TYPE_NOT_SUPPORTED: i32 = -32005;
const INTERNAL_ERROR: i32 = -32603;

struct Component;

// A task's ids.
struct Ids {
    task_id: String,
    context_id: String,
}

impl exports::composable::agents::agent::Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        Ok(AgentMetadata {
            name: "test-agent".to_string(),
            description: "Behaves as its message's text says".to_string(),
            provider: None,
            version: "0.1.0".to_string(),
            documentation_url: None,
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
            skills: vec![AgentSkill {
                id: "test".to_string(),
                name: "Test".to_string(),
                description: "Behaves as its message's text says".to_string(),
                tags: vec!["test".to_string()],
                examples: Vec::new(),
                input_modes: Vec::new(),
                output_modes: Vec::new(),
                security_requirements: Vec::new(),
            }],
            icon_url: None,
            extensions: Vec::new(),
        })
    }

    async fn invoke(
        request: AgentRequest,
        cancel: FutureReader<()>,
    ) -> Result<AgentInvocation, AgentError> {
        let text = text_of(&request.message);
        if text == "refuse" {
            return Err(AgentError {
                code: CONTENT_TYPE_NOT_SUPPORTED,
                message: "the test agent refused the message".to_string(),
                data: Some(r#"[{"@type":"type.googleapis.com/google.rpc.ErrorInfo"}]"#.to_string()),
            });
        }

        // The ids are the request's, or are made from the message's id.
        let message_id = &request.message.message_id;
        let ids = Ids {
            task_id: request
                .task_id
                .clone()
                .unwrap_or_else(|| format!("task-{message_id}")),
            context_id: request
                .context_id
                .clone()
                .unwrap_or_else(|| format!("context-{message_id}")),
        };
        let (mut events_tx, events) = wit_stream::new::<AgentEvent>();
        let (result_tx, result) = wit_future::new::<Result<AgentResponse, AgentError>>(|| {
            Err(AgentError {
                code: INTERNAL_ERROR,
                message: "the test agent stopped before its outcome".to_string(),
                data: None,
            })
        });
        let invocation = AgentInvocation {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            activated_extensions: request.requested_extensions.clone(),
            response: AgentResponseStream { events, result },
        };
        spawn_local(async move {
            let response = run(&request, &text, &ids, &mut events_tx, cancel).await;
            drop(events_tx);
            let _ = result_tx.write(Ok(response)).await;
        });
        Ok(invocation)
    }
}

// Runs the task, sending a `working` status first, and returns its outcome.
async fn run(
    request: &AgentRequest,
    text: &str,
    ids: &Ids,
    events: &mut StreamWriter<AgentEvent>,
    cancel: FutureReader<()>,
) -> AgentResponse {
    let _ = events
        .write_one(status_event(ids, TaskState::Working, None))
        .await;

    // A request with a task status continues an interrupted task.
    if request.task_status.is_some() && text != "wait" {
        return completed(ids, events, text, format!("continued: {text}")).await;
    }
    match text {
        "think" => {
            let progress = status_event(ids, TaskState::Working, Some("Working on it."));
            let _ = events.write_one(progress).await;
            completed(ids, events, text, "done".to_string()).await
        }
        "fail" => outcome(
            ids,
            TaskState::Failed,
            Some("the test agent failed"),
            Vec::new(),
            text,
        ),
        "ask" => outcome(
            ids,
            TaskState::InputRequired,
            Some("Which one?"),
            Vec::new(),
            text,
        ),
        "wait" => {
            cancel.await;
            outcome(ids, TaskState::Canceled, None, Vec::new(), text)
        }
        "chunks" => {
            let first = text_artifact("one ".to_string());
            let second = text_artifact("two".to_string());
            let whole = Artifact {
                parts: vec![text_part("one ".to_string()), text_part("two".to_string())],
                ..first.clone()
            };
            let _ = events
                .write_one(chunk_event(ids, first, false, false))
                .await;
            let _ = events.write_one(chunk_event(ids, second, true, true)).await;
            outcome(ids, TaskState::Completed, None, vec![whole], text)
        }
        "no parts" => {
            let artifact = Artifact {
                parts: Vec::new(),
                ..text_artifact(String::new())
            };
            let _ = events.write_one(artifact_event(ids, artifact)).await;
            outcome(ids, TaskState::Completed, None, Vec::new(), text)
        }
        "other task" => {
            let other = Ids {
                task_id: "task-other".to_string(),
                context_id: ids.context_id.clone(),
            };
            let _ = events
                .write_one(status_event(&other, TaskState::Working, None))
                .await;
            outcome(ids, TaskState::Completed, None, Vec::new(), text)
        }
        _ => {
            let reply = format!("{text} (history: {})", request.history.len());
            completed(ids, events, text, reply).await
        }
    }
}

// Sends `reply` as the task's artifact and returns the `completed` outcome.
async fn completed(
    ids: &Ids,
    events: &mut StreamWriter<AgentEvent>,
    text: &str,
    reply: String,
) -> AgentResponse {
    let artifact = text_artifact(reply);
    let _ = events
        .write_one(artifact_event(ids, artifact.clone()))
        .await;
    outcome(ids, TaskState::Completed, None, vec![artifact], text)
}

// The task's outcome, with one exchange for the invocation whose input is the
// message's text.
fn outcome(
    ids: &Ids,
    state: TaskState,
    status_text: Option<&str>,
    artifacts: Vec<Artifact>,
    text: &str,
) -> AgentResponse {
    AgentResponse {
        status: status(ids, state, status_text),
        artifacts,
        exchanges: vec![ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::Text(text.to_string())],
                max_tokens: None,
            },
            output: Some(vec![ModelOutputItem::Text("ok".to_string())]),
            opaque_content: None,
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage {
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        }],
        finish_reason: None,
    }
}

fn status_event(ids: &Ids, state: TaskState, text: Option<&str>) -> AgentEvent {
    AgentEvent::Status(TaskStatusUpdateEvent {
        task_id: ids.task_id.clone(),
        context_id: ids.context_id.clone(),
        status: status(ids, state, text),
        metadata: None,
    })
}

// A status with no timestamp, and with `text` as its message if given.
fn status(ids: &Ids, state: TaskState, text: Option<&str>) -> TaskStatus {
    TaskStatus {
        state,
        message: text.map(|text| Message {
            message_id: format!("{}-status", ids.task_id),
            context_id: Some(ids.context_id.clone()),
            task_id: Some(ids.task_id.clone()),
            role: Role::Agent,
            parts: vec![text_part(text.to_string())],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }),
        timestamp: None,
    }
}

// An artifact sent whole, as its only chunk.
fn artifact_event(ids: &Ids, artifact: Artifact) -> AgentEvent {
    chunk_event(ids, artifact, false, true)
}

// A chunk of an artifact.
fn chunk_event(ids: &Ids, artifact: Artifact, append: bool, last_chunk: bool) -> AgentEvent {
    AgentEvent::Artifact(TaskArtifactUpdateEvent {
        task_id: ids.task_id.clone(),
        context_id: ids.context_id.clone(),
        artifact,
        append,
        last_chunk,
        metadata: None,
    })
}

fn text_artifact(text: String) -> Artifact {
    Artifact {
        artifact_id: "reply".to_string(),
        name: None,
        description: None,
        parts: vec![text_part(text)],
        metadata: None,
        extensions: Vec::new(),
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

// The text of the message's first part, or empty if that part isn't text.
fn text_of(message: &Message) -> String {
    match message.parts.first().map(|part| &part.content) {
        Some(PartContent::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

export!(Component);
