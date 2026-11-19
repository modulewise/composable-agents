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
use wit_bindgen::rt::async_support::{FutureReader, spawn_local};

use agent_support::model_types::{
    ContentDelta, ModelDelta, ModelExchange, ModelInputItem, ModelRequest, ModelSettings,
};
use agent_support::types::{
    AgentError, AgentEvent, AgentFinishReason, AgentInvocation, AgentMetadata, AgentRequest,
    AgentResponse, AgentResponseStream, TaskState, TaskStatus,
};
use agent_support::{
    Cancel, Events, INSTRUCTIONS_CONFIG, INTERNAL_ERROR, INVALID_PARAMS, REPORT_REASONING_CONFIG,
    ReasoningRun, TaskIds, TextArtifact, agent_message, agent_metadata, get_config, model_input,
    new_id, parse_config, response_text, session_error, status_event,
};
use composable::models::client::Session;

struct Component;

impl exports::composable::agents::agent::Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        agent_metadata()
    }

    async fn invoke(
        request: AgentRequest,
        cancel: FutureReader<()>,
    ) -> Result<AgentInvocation, AgentError> {
        let input = model_input(&request.message.parts);
        if input.is_empty() {
            return Err(AgentError {
                code: INVALID_PARAMS,
                message: "a message requires at least one part".to_string(),
                data: None,
            });
        }
        let options = Options::from_config().map_err(|message| AgentError {
            code: INTERNAL_ERROR,
            message,
            data: None,
        })?;
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
        let session = Session::new(&settings, &request.history).map_err(session_error)?;

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

        let invocation = AgentInvocation {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            activated_extensions: Vec::new(),
            response: AgentResponseStream {
                events: events_rx,
                result: outcome_rx,
            },
        };
        let cancel = Cancel::new(cancel);
        spawn_local(async move {
            let outcome = run(
                session,
                input,
                &ids,
                &options,
                Events::new(events_tx),
                &cancel,
            )
            .await;
            let _ = outcome_tx.write(Ok(outcome)).await;
        });
        Ok(invocation)
    }
}

// The agent's config.
struct Options {
    // Whether to send the model's reasoning as status messages.
    report_reasoning: bool,
}

impl Options {
    fn from_config() -> Result<Self, String> {
        Ok(Options {
            report_reasoning: parse_config(
                REPORT_REASONING_CONFIG,
                get_config(REPORT_REASONING_CONFIG),
                false,
                "true or false",
            )?,
        })
    }
}

// Runs the task as one model call. Streams a `working` status, each run of
// reasoning as a `working` status message when the agent reports reasoning,
// and the text as chunks of one artifact, then returns the outcome. A cancel
// stops the model call.
async fn run(
    session: Session,
    input: Vec<ModelInputItem>,
    ids: &TaskIds,
    options: &Options,
    mut events: Events,
    cancel: &Cancel,
) -> AgentResponse {
    let mut artifact = TextArtifact::new(new_id());
    events
        .send(status_event(ids, TaskState::Working, None))
        .await;
    if cancel.is_canceled() {
        return outcome(TaskState::Canceled, artifact, session.exchanges(), None);
    }
    let stream = match session
        .call(ModelRequest {
            input,
            max_tokens: None,
        })
        .await
        .and_then(|call| call.response)
    {
        Ok(stream) => stream,
        Err(e) => return failed(ids, &e.message, artifact, session.exchanges()),
    };

    let mut reasoning = ReasoningRun::new(options.report_reasoning);
    let mut streamed_text = false;
    let mut deltas = stream.deltas.into_stream();
    let canceled = loop {
        let delta = match cancel.until_canceled(deltas.next()).await {
            None => break true,
            Some(None) => break false,
            Some(Some(delta)) => delta,
        };
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
        events.send_all(ready).await;
    };
    if canceled {
        // Dropping the deltas stops the model call. Its exchange is complete
        // once its result resolves.
        drop(deltas);
        let _ = stream.result.await;
        return outcome(TaskState::Canceled, artifact, session.exchanges(), None);
    }
    // A run of reasoning also ends with the stream.
    events
        .send_all(reasoning.finish(ids).into_iter().collect())
        .await;

    let response = match stream.result.await {
        Ok(response) => response,
        Err(e) => {
            // Any text streamed before a failure remains the task's artifact.
            if let Some(event) = artifact.finish(ids) {
                events.send(event).await;
            }
            return failed(ids, &e.message, artifact, session.exchanges());
        }
    };
    let reason = Some(AgentFinishReason::Model(response.finish_reason));
    // If no reasoning deltas were streamed, send each run of reasoning in the
    // output as a status message, before the text.
    events
        .send_all(reasoning.unstreamed(ids, &response.output))
        .await;
    // A response that streamed no text deltas sends its whole text as one
    // chunk.
    if !streamed_text && let Some(event) = artifact.push(response_text(&response.output), ids) {
        events.send(event).await;
    }
    if let Some(event) = artifact.finish(ids) {
        events.send(event).await;
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
