//! An agent for running the A2A TCK against the server. It has no model. What
//! it does with a message depends on the prefix of the message's id, as the
//! TCK's `scenarios/core_operations.feature` and `scenarios/streaming.feature`
//! specify. A message that continues an interrupted task completes the task.
//!
//! Two of the TCK's scenarios are not implemented:
//!
//! - `tck-message-response`: an agent responds with a task, never a message.
//! - `tck-reject-task`: none of the TCK's tests send it.

wit_bindgen::generate!({
    path: [
        "../../../../components/agents/wit",
        "../../../../components/agents/agent-support/wit/deps/wasi-config-0.2.0-rc.1",
        "../../../../components/agents/agent-support/wit/deps/wasi-random-0.3.0",
        "../../../../components/models/anthropic-messages-client/wit/deps/wasi-clocks-0.3.0",
        "wit",
    ],
    world: "composable-agents:tck-agent/tck-agent",
    with: {
        "wasi:config/store@0.2.0-rc.1": agent_support::config_store,
        "wasi:random/random@0.3.0": agent_support::random,
        "composable:agents/types@0.1.0": agent_support::types,
        "composable:models/types@0.1.0": agent_support::model_types,
    },
    generate_all,
});

use wit_bindgen::rt::async_support::{FutureReader, spawn_local};

use agent_support::types::{
    AgentError, AgentEvent, AgentInvocation, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, Artifact, Message, Part, PartContent, TaskArtifactUpdateEvent, TaskState,
    TaskStatus,
};
use agent_support::{
    Cancel, Events, INTERNAL_ERROR, TaskIds, TextArtifact, agent_message, agent_metadata, new_id,
    status_event,
};
use wasi::clocks::monotonic_clock::wait_for;

// The TCK's default streaming timeout. The task of `Scenario::Wait` runs for
// twice as long.
const STREAMING_TIMEOUT_NANOS: u64 = 2_000_000_000;

// The name and the media type of a scenario's file.
const FILE_NAME: &str = "output.txt";
const FILE_MEDIA_TYPE: &str = "text/plain";

// What the agent does with a message.
#[derive(Clone, Copy)]
enum Scenario {
    // The task completes, with the text as its status message if given.
    Complete(Option<&'static str>),
    // The task completes with a text artifact.
    Text(&'static str),
    // The task completes with an artifact that has a file's bytes.
    File,
    // The task completes with an artifact that has a file's URL.
    FileUrl,
    // The task completes with an artifact that has structured data.
    Data,
    // The task completes with a text artifact sent in two chunks.
    Chunks,
    // The task is interrupted as `input-required`.
    InputRequired,
    // The task is `working` for twice the streaming timeout, then completes.
    Wait,
}

// The scenario for each message id prefix.
const SCENARIOS: [(&str, Scenario); 14] = [
    (
        "tck-complete-task",
        Scenario::Complete(Some("Hello from TCK")),
    ),
    (
        "tck-artifact-text",
        Scenario::Text("Generated text content"),
    ),
    ("tck-artifact-file", Scenario::File),
    ("tck-artifact-file-url", Scenario::FileUrl),
    ("tck-artifact-data", Scenario::Data),
    ("tck-input-required", Scenario::InputRequired),
    ("tck-stream-001", Scenario::Text("Stream hello from TCK")),
    ("tck-stream-002", Scenario::Complete(None)),
    ("tck-stream-003", Scenario::Text("Stream task lifecycle")),
    ("tck-stream-ordering-001", Scenario::Text("Ordered output")),
    (
        "tck-stream-artifact-text",
        Scenario::Text("Streamed text content"),
    ),
    ("tck-stream-artifact-file", Scenario::File),
    ("tck-stream-artifact-chunked", Scenario::Chunks),
    ("test-resubscribe-message-id", Scenario::Wait),
];

// The scenario of the longest prefix that `message_id` starts with, if any.
fn scenario(message_id: &str) -> Option<Scenario> {
    SCENARIOS
        .iter()
        .filter(|(prefix, _)| message_id.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, scenario)| *scenario)
}

struct Component;

impl exports::composable::agents::agent::Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        agent_metadata()
    }

    async fn invoke(
        request: AgentRequest,
        cancel: FutureReader<()>,
    ) -> Result<AgentInvocation, AgentError> {
        let ids = TaskIds {
            task_id: request.task_id.clone().unwrap_or_else(new_id),
            context_id: request.context_id.clone().unwrap_or_else(new_id),
        };
        let (events_tx, events) = wit_stream::new::<AgentEvent>();
        let (result_tx, result) = wit_future::new::<Result<AgentResponse, AgentError>>(|| {
            Err(AgentError {
                code: INTERNAL_ERROR,
                message: "the agent stopped before the task's outcome".to_string(),
                data: None,
            })
        });
        let invocation = AgentInvocation {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            activated_extensions: Vec::new(),
            response: AgentResponseStream { events, result },
        };
        spawn_local(async move {
            let mut events = Events::new(events_tx);
            let response = run(&request, &ids, &mut events, Cancel::new(cancel)).await;
            drop(events);
            let _ = result_tx.write(Ok(response)).await;
        });
        Ok(invocation)
    }
}

// Runs the task, sending a `working` status first, and returns its outcome.
async fn run(
    request: &AgentRequest,
    ids: &TaskIds,
    events: &mut Events,
    cancel: Cancel,
) -> AgentResponse {
    events
        .send(status_event(ids, TaskState::Working, None))
        .await;

    // A request with a task status continues an interrupted task.
    if request.task_status.is_some() {
        return outcome(TaskState::Completed, None, Vec::new());
    }
    let message_id = &request.message.message_id;
    let Some(scenario) = scenario(message_id) else {
        let text = format!("Unhandled messageId prefix: {message_id}");
        return outcome(
            TaskState::Completed,
            Some(agent_message(ids, text)),
            Vec::new(),
        );
    };
    match scenario {
        Scenario::Complete(text) => {
            let message = text.map(|text| agent_message(ids, text.to_string()));
            outcome(TaskState::Completed, message, Vec::new())
        }
        Scenario::Text(text) => text_artifact(ids, events, &[text]).await,
        Scenario::Chunks => text_artifact(ids, events, &["chunk-1 ", "chunk-2"]).await,
        Scenario::File => {
            // The bytes `tck`, base64-encoded.
            let content = PartContent::Raw("dGNr".to_string());
            file_artifact(ids, events, content).await
        }
        Scenario::FileUrl => {
            let content = PartContent::Url(format!("https://example.com/{FILE_NAME}"));
            file_artifact(ids, events, content).await
        }
        Scenario::Data => {
            let part = Part {
                content: PartContent::Data(r#"{"key": "value", "count": 42}"#.to_string()),
                metadata: None,
                filename: None,
                media_type: None,
            };
            artifact(ids, events, part).await
        }
        Scenario::InputRequired => outcome(TaskState::InputRequired, None, Vec::new()),
        Scenario::Wait => {
            let waited = cancel
                .until_canceled(wait_for(2 * STREAMING_TIMEOUT_NANOS))
                .await;
            match waited {
                Some(()) => outcome(TaskState::Completed, None, Vec::new()),
                None => outcome(TaskState::Canceled, None, Vec::new()),
            }
        }
    }
}

// Sends a text artifact as `chunks`, and returns the `completed` outcome with
// the artifact.
async fn text_artifact(ids: &TaskIds, events: &mut Events, chunks: &[&str]) -> AgentResponse {
    let mut artifact = TextArtifact::new(new_id());
    for chunk in chunks {
        if let Some(event) = artifact.push(chunk.to_string(), ids) {
            events.send(event).await;
        }
    }
    if let Some(event) = artifact.finish(ids) {
        events.send(event).await;
    }
    let artifacts = artifact.into_artifact().into_iter().collect();
    outcome(TaskState::Completed, None, artifacts)
}

// Sends an artifact with the scenarios' file as `content`, and returns the
// `completed` outcome with the artifact.
async fn file_artifact(ids: &TaskIds, events: &mut Events, content: PartContent) -> AgentResponse {
    let part = Part {
        content,
        metadata: None,
        filename: Some(FILE_NAME.to_string()),
        media_type: Some(FILE_MEDIA_TYPE.to_string()),
    };
    artifact(ids, events, part).await
}

// Sends an artifact with one part, whole, and returns the `completed` outcome
// with the artifact.
async fn artifact(ids: &TaskIds, events: &mut Events, part: Part) -> AgentResponse {
    let artifact = Artifact {
        artifact_id: new_id(),
        name: None,
        description: None,
        parts: vec![part],
        metadata: None,
        extensions: Vec::new(),
    };
    events
        .send(AgentEvent::Artifact(TaskArtifactUpdateEvent {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            artifact: artifact.clone(),
            append: false,
            last_chunk: true,
            metadata: None,
        }))
        .await;
    outcome(TaskState::Completed, None, vec![artifact])
}

// The task's outcome. The agent calls no model, so it has no exchanges.
fn outcome(state: TaskState, message: Option<Message>, artifacts: Vec<Artifact>) -> AgentResponse {
    AgentResponse {
        status: TaskStatus {
            state,
            message,
            timestamp: None,
        },
        artifacts,
        exchanges: Vec::new(),
        finish_reason: None,
    }
}

export!(Component);
