//! An agent that answers each message with one call to its model.

wit_bindgen::generate!({
    path: "wit",
    world: "chat-agent",
    with: {
        "wasi:config/store@0.2.0-rc.1": agent_support::config_store,
        "wasi:random/random@0.3.0": agent_support::random,
        "composable:agents/types@0.1.0": agent_support::types,
        "composable:models/types@0.1.0": agent_support::model_types,
    },
    generate_all,
});

use futures::StreamExt;
use wit_bindgen::rt::async_support::{StreamWriter, spawn_local};

use agent_support::model_types::{
    ContentDelta, ModelDelta, ModelExchange, ModelInputItem, ModelRequest, ModelSettings,
};
use agent_support::types::{
    AgentError, AgentEvent, AgentFinishReason, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, TaskState, TaskStatus,
};
use agent_support::{
    INSTRUCTIONS_CONFIG, INTERNAL_ERROR, INVALID_PARAMS, ReasoningRun, TaskIds, TextArtifact,
    agent_message, agent_metadata, get_config, model_input, new_id, reasoning_event,
    reasoning_runs, response_text, send, send_all, status_event,
};
use composable::models::client::Session;

struct Component;

impl exports::composable::agents::agent::Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        agent_metadata()
    }

    async fn invoke(request: AgentRequest) -> Result<AgentResponseStream, AgentError> {
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
        return outcome(TaskState::Canceled, artifact, session.exchanges(), None);
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
        // A run of reasoning ends at the next content delta of another kind.
        let ready: Vec<AgentEvent> = match delta {
            ModelDelta::Content(ContentDelta::Reasoning(chunk)) => {
                reasoning.push(chunk);
                Vec::new()
            }
            ModelDelta::Content(ContentDelta::Text(chunk)) => {
                streamed_text = true;
                let ended = reasoning.finish(ids);
                ended.into_iter().chain(artifact.push(chunk, ids)).collect()
            }
            ModelDelta::Content(ContentDelta::ToolCall(_)) => {
                reasoning.finish(ids).into_iter().collect()
            }
            // The session records the counts in the call's exchange.
            ModelDelta::Usage(_) => Vec::new(),
        };
        if !send_all(&mut events, ready).await {
            drop(deltas);
            return outcome(TaskState::Canceled, artifact, session.exchanges(), None);
        }
    }
    // A run of reasoning also ends with the stream.
    if !send_all(&mut events, reasoning.finish(ids).into_iter().collect()).await {
        return outcome(TaskState::Canceled, artifact, session.exchanges(), None);
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
    let reason = Some(AgentFinishReason::Model(response.finish_reason));
    // If no reasoning deltas were streamed, send each run of reasoning in the
    // output as a status message, before the text.
    if !reasoning.streamed {
        let runs = reasoning_runs(&response.output)
            .into_iter()
            .map(|text| reasoning_event(ids, text))
            .collect();
        if !send_all(&mut events, runs).await {
            return outcome(TaskState::Canceled, artifact, session.exchanges(), reason);
        }
    }
    // A response that streamed no text deltas sends its whole text as one
    // chunk.
    if !streamed_text
        && let Some(event) = artifact.push(response_text(&response.output), ids)
        && !send(&mut events, event).await
    {
        return outcome(TaskState::Canceled, artifact, session.exchanges(), reason);
    }
    if let Some(event) = artifact.finish(ids)
        && !send(&mut events, event).await
    {
        return outcome(TaskState::Canceled, artifact, session.exchanges(), reason);
    }
    outcome(TaskState::Completed, artifact, session.exchanges(), reason)
}

// The outcome of a task that ended in `state`. Includes its artifact as sent,
// the exchanges its model call made, and the finish reason if the call
// produced a response.
fn outcome(
    state: TaskState,
    artifact: TextArtifact,
    exchanges: Vec<ModelExchange>,
    finish_reason: Option<AgentFinishReason>,
) -> AgentResponse {
    AgentResponse {
        status: TaskStatus {
            state,
            message: None,
            timestamp: None,
        },
        artifacts: artifact.into_artifact().into_iter().collect(),
        exchanges,
        finish_reason,
    }
}

// The outcome of a task whose model call failed, with the failure as the
// status message.
fn failed(
    ids: &TaskIds,
    error: &str,
    artifact: TextArtifact,
    exchanges: Vec<ModelExchange>,
) -> AgentResponse {
    let mut response = outcome(TaskState::Failed, artifact, exchanges, None);
    response.status.message = Some(agent_message(
        ids,
        format!("the model call failed: {error}"),
    ));
    response
}

export!(Component);
