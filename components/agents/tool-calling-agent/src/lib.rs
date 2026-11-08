//! An agent that calls its model in a loop, running the tools the model
//! requests from its toolset and sending their results back, until the model
//! finishes or the agent reaches its turn limit. The text of every model call
//! streams as chunks of one artifact.

wit_bindgen::generate!({
    path: "wit",
    world: "tool-calling-agent",
    with: {
        "wasi:config/store@0.2.0-rc.1": agent_support::config_store,
        "wasi:random/random@0.3.0": agent_support::random,
        "composable:agents/types@0.1.0": agent_support::types,
    },
    generate_all,
});

use std::collections::VecDeque;
use std::num::NonZeroU32;

use futures::StreamExt;
use wit_bindgen::rt::async_support::{StreamWriter, spawn_local};

use agent_support::types::{
    AgentError, AgentEvent, AgentFinishReason, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, ContentDelta, ContentSource, MediaData, ModelDelta, ModelExchange,
    ModelFinishReason, ModelInputItem, ModelOutputItem, ModelRequest, ModelSettings, ResultContent,
    TaskState, TaskStatus, TokenUsage, ToolCall, ToolChoice, ToolDefinition, ToolResult,
};
use agent_support::{
    INSTRUCTIONS_CONFIG, INTERNAL_ERROR, INVALID_PARAMS, OCTET_STREAM, ReasoningRun, TaskIds,
    TextArtifact, agent_message, agent_metadata, get_config, model_input, new_id, parse_config,
    reasoning_event, reasoning_runs, response_text, send, send_all, status_event,
};
use composable::agents::model_client::Session;
use composable::tools::toolset;
use composable::tools::types::{CallToolRequest, ContentBlock, ResourceContents, ToolMetadata};

const MAX_TURNS_CONFIG: &str = "max-turns";
const REPORT_TOOL_CALLS_CONFIG: &str = "report-tool-calls";

// The maximum number of model calls per invocation when `max-turns` isn't set.
const DEFAULT_MAX_TURNS: NonZeroU32 = NonZeroU32::new(8).unwrap();

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
        let options = Options::from_config().map_err(|message| AgentError {
            code: INTERNAL_ERROR,
            message,
            data: None,
        })?;
        let tools: Vec<ToolDefinition> = toolset::list()
            .await
            .map_err(|e| AgentError {
                code: INTERNAL_ERROR,
                message: format!("failed to list tools: {e}"),
                data: None,
            })?
            .iter()
            .map(tool_definition)
            .collect();
        let settings = ModelSettings {
            instructions: get_config(INSTRUCTIONS_CONFIG),
            tool_choice: (!tools.is_empty()).then_some(ToolChoice::Auto),
            tools: (!tools.is_empty()).then_some(tools),
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
            let outcome = run(session, input, &ids, &options, events_tx).await;
            let _ = outcome_tx.write(Ok(outcome)).await;
        });
        Ok(response)
    }
}

// The agent's config.
struct Options {
    max_turns: NonZeroU32,
    // Whether to send a status naming each tool before calling it.
    report_tool_calls: bool,
}

impl Options {
    fn from_config() -> Result<Self, String> {
        Ok(Options {
            max_turns: parse_config(
                MAX_TURNS_CONFIG,
                get_config(MAX_TURNS_CONFIG),
                DEFAULT_MAX_TURNS,
                "a whole number greater than 0",
            )?,
            report_tool_calls: parse_config(
                REPORT_TOOL_CALLS_CONFIG,
                get_config(REPORT_TOOL_CALLS_CONFIG),
                false,
                "true or false",
            )?,
        })
    }
}

// Calls the model in a loop and runs the tools each response requests,
// streaming a `working` status before each call, reasoning as status messages
// and all text as one artifact. A dropped event stream cancels the task.
async fn run(
    session: Session,
    input: Vec<ModelInputItem>,
    ids: &TaskIds,
    options: &Options,
    mut events: StreamWriter<AgentEvent>,
) -> AgentResponse {
    let mut task = Task {
        session,
        ids,
        artifact: TextArtifact::new(new_id()),
        pending_calls: VecDeque::new(),
        results: Vec::new(),
    };
    if !send(&mut events, status_event(ids, TaskState::Working, None)).await {
        return task.end(TaskState::Canceled, None, None);
    }

    let mut input = input;
    let mut turn: u32 = 1;
    loop {
        let stream = match task
            .session
            .call(ModelRequest {
                input,
                max_tokens: None,
            })
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                let message = format!("the model call failed: {}", e.message);
                return task
                    .finish(&mut events, TaskState::Failed, None, Some(message))
                    .await;
            }
        };

        let mut reasoning = ReasoningRun::default();
        let mut streamed_text = false;
        let mut deltas = stream.deltas.into_stream();
        while let Some(delta) = deltas.next().await {
            // A run of reasoning ends at the next content delta of another
            // kind.
            let ready: Vec<AgentEvent> = match delta {
                ModelDelta::Content(ContentDelta::Reasoning(chunk)) => {
                    reasoning.push(chunk);
                    Vec::new()
                }
                ModelDelta::Content(ContentDelta::Text(chunk)) => {
                    streamed_text = true;
                    let ended = reasoning.finish(ids);
                    ended
                        .into_iter()
                        .chain(task.artifact.push(chunk, ids))
                        .collect()
                }
                ModelDelta::Content(ContentDelta::ToolCall(_)) => {
                    reasoning.finish(ids).into_iter().collect()
                }
                // The session records the counts in the call's exchange.
                ModelDelta::Usage(_) => Vec::new(),
            };
            if !send_all(&mut events, ready).await {
                drop(deltas);
                return task.end(TaskState::Canceled, None, None);
            }
        }
        // A run of reasoning also ends with the stream.
        if !send_all(&mut events, reasoning.finish(ids).into_iter().collect()).await {
            return task.end(TaskState::Canceled, None, None);
        }

        let response = match stream.result.await {
            Ok(response) => response,
            Err(e) => {
                let message = format!("the model call failed: {}", e.message);
                return task
                    .finish(&mut events, TaskState::Failed, None, Some(message))
                    .await;
            }
        };
        task.pending_calls = tool_calls(&response.output);
        // If no reasoning deltas were streamed, send each run of reasoning in
        // the output as a status message, before the text.
        if !reasoning.streamed {
            let runs = reasoning_runs(&response.output)
                .into_iter()
                .map(|text| reasoning_event(ids, text))
                .collect();
            if !send_all(&mut events, runs).await {
                return task.end(TaskState::Canceled, None, None);
            }
        }
        // A response that streamed no text deltas sends its whole text as one
        // chunk.
        if !streamed_text
            && let Some(event) = task.artifact.push(response_text(&response.output), ids)
            && !send(&mut events, event).await
        {
            return task.end(TaskState::Canceled, None, None);
        }

        match response.finish_reason {
            reason @ ModelFinishReason::ToolRequested(_) if task.pending_calls.is_empty() => {
                let message = "the model requested tools but named none".to_string();
                let reason = Some(AgentFinishReason::Model(reason));
                return task
                    .finish(&mut events, TaskState::Failed, reason, Some(message))
                    .await;
            }
            ModelFinishReason::ToolRequested(_) => {}
            reason => {
                let reason = Some(AgentFinishReason::Model(reason));
                return task
                    .finish(&mut events, TaskState::Completed, reason, None)
                    .await;
            }
        }
        while let Some(call) = task.pending_calls.pop_front() {
            if options.report_tool_calls
                && !send(&mut events, tool_call_event(ids, &call.name)).await
            {
                task.pending_calls.push_front(call);
                return task.end(TaskState::Canceled, None, None);
            }
            task.results.push(call_tool(&call).await);
        }
        // At the turn limit, the results go in the closing exchange instead of
        // another model call.
        if turn == options.max_turns.get() {
            let message = format!("the agent reached its limit of {turn} model calls");
            let reason = Some(AgentFinishReason::MaxTurns);
            return task
                .finish(&mut events, TaskState::Failed, reason, Some(message))
                .await;
        }
        if !send(&mut events, status_event(ids, TaskState::Working, None)).await {
            return task.end(TaskState::Canceled, None, None);
        }
        input = std::mem::take(&mut task.results);
        turn += 1;
    }
}

// The task's progress, reported in its outcome.
struct Task<'a> {
    session: Session,
    ids: &'a TaskIds,
    artifact: TextArtifact,
    // The last response's tool calls that haven't run.
    pending_calls: VecDeque<ToolCall>,
    // The results of the tools that ran, not yet sent to the model.
    results: Vec<ModelInputItem>,
}

impl Task<'_> {
    // Ends the task, sending the artifact's held chunk as its last. A task
    // that would complete is canceled if the caller stopped reading.
    async fn finish(
        mut self,
        events: &mut StreamWriter<AgentEvent>,
        state: TaskState,
        finish_reason: Option<AgentFinishReason>,
        message: Option<String>,
    ) -> AgentResponse {
        let delivered = match self.artifact.finish(self.ids) {
            Some(event) => send(events, event).await,
            None => true,
        };
        let state = match state {
            TaskState::Completed if !delivered => TaskState::Canceled,
            state => state,
        };
        self.end(state, finish_reason, message)
    }

    // The outcome of a task that ended in `state`, with `message` as its
    // status message. Includes its artifact as sent and the exchanges its
    // model calls made, along with any closing exchange.
    fn end(
        self,
        state: TaskState,
        finish_reason: Option<AgentFinishReason>,
        message: Option<String>,
    ) -> AgentResponse {
        let mut exchanges = self.session.exchanges();
        exchanges.extend(closing_exchange(
            self.results,
            &self.pending_calls,
            not_run_reason(state),
        ));
        AgentResponse {
            status: TaskStatus {
                state,
                message: message.map(|text| agent_message(self.ids, text)),
                timestamp: None,
            },
            artifacts: self.artifact.into_artifact().into_iter().collect(),
            exchanges,
            finish_reason,
        }
    }
}

// Why the pending tool calls of a task that ended in `state` didn't run.
fn not_run_reason(state: TaskState) -> &'static str {
    match state {
        TaskState::Canceled => "the task was canceled",
        _ => "the model's response ended without requesting tools",
    }
}

// The results of the tools that ran and an error result for each pending
// call, as an exchange that answers the last response's tool calls and is not
// sent as a call. `None` if there are neither.
fn closing_exchange(
    mut results: Vec<ModelInputItem>,
    pending_calls: &VecDeque<ToolCall>,
    reason: &str,
) -> Option<ModelExchange> {
    results.extend(pending_calls.iter().map(|call| {
        ModelInputItem::ToolResult(ToolResult {
            tool_call_id: call.id.clone(),
            name: call.name.clone(),
            content: vec![ResultContent::Text(format!("Not run: {reason}."))],
            is_error: true,
        })
    }));
    (!results.is_empty()).then_some(ModelExchange {
        request: ModelRequest {
            input: results,
            max_tokens: None,
        },
        output: None,
        opaque_content: None,
        provider: None,
        request_model: None,
        response_model: None,
        usage: TokenUsage::default(),
    })
}

// The tool calls in a response's output, in order.
fn tool_calls(output: &[ModelOutputItem]) -> VecDeque<ToolCall> {
    output
        .iter()
        .filter_map(|item| match item {
            ModelOutputItem::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

// Calls a tool and returns its result for the model. A failed call returns its
// error as an error result.
async fn call_tool(call: &ToolCall) -> ModelInputItem {
    let request = CallToolRequest {
        name: call.name.clone(),
        arguments: call.arguments.clone(),
        meta: Vec::new(),
    };
    let (content, is_error) = match toolset::call(request).await {
        Ok(result) => (
            result.content.iter().map(result_content).collect(),
            result.is_error,
        ),
        Err(e) => (vec![ResultContent::Text(e.message)], true),
    };
    ModelInputItem::ToolResult(ToolResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content,
        is_error,
    })
}

// One block of a tool's result as content for the model. A blob resource is
// media data with its media type, or `application/octet-stream` as the default.
fn result_content(block: &ContentBlock) -> ResultContent {
    match block {
        ContentBlock::Text(t) => ResultContent::Text(t.text.clone()),
        ContentBlock::Image(i) => ResultContent::Media(ContentSource::Data(MediaData {
            mime_type: i.mime_type.clone(),
            data: i.data.clone(),
        })),
        ContentBlock::Audio(a) => ResultContent::Media(ContentSource::Data(MediaData {
            mime_type: a.mime_type.clone(),
            data: a.data.clone(),
        })),
        ContentBlock::ResourceLink(r) => ResultContent::Media(ContentSource::Url(r.uri.clone())),
        ContentBlock::Resource(r) => match &r.resource_data {
            ResourceContents::Text(t) => ResultContent::Text(t.text.clone()),
            ResourceContents::Blob(b) => ResultContent::Media(ContentSource::Data(MediaData {
                mime_type: b
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| OCTET_STREAM.to_string()),
                data: b.blob.clone(),
            })),
        },
    }
}

fn tool_definition(tool: &ToolMetadata) -> ToolDefinition {
    ToolDefinition {
        name: tool.name.clone(),
        description: tool.description.clone().unwrap_or_default(),
        input_schema: tool.input_schema.clone(),
    }
}

// A `working` status naming the tool, without the call's arguments.
fn tool_call_event(ids: &TaskIds, tool: &str) -> AgentEvent {
    let message = agent_message(ids, format!("Calling tool `{tool}`"));
    status_event(ids, TaskState::Working, Some(message))
}

export!(Component);

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: "calc.add".to_string(),
            arguments: r#"{"a":1,"b":2}"#.to_string(),
        }
    }

    #[test]
    fn the_closing_exchange_answers_every_pending_call() {
        let ran = ModelInputItem::ToolResult(ToolResult {
            tool_call_id: "1".to_string(),
            name: "calc.add".to_string(),
            content: vec![ResultContent::Text("3".to_string())],
            is_error: false,
        });
        let pending = VecDeque::from([call("2")]);

        let exchange =
            closing_exchange(vec![ran.clone()], &pending, "the task was canceled").unwrap();

        assert!(exchange.output.is_none());
        assert!(exchange.opaque_content.is_none());
        // It is not sent as a call.
        assert!(exchange.provider.is_none());
        assert!(exchange.request_model.is_none());
        let expected = vec![
            ran,
            ModelInputItem::ToolResult(ToolResult {
                tool_call_id: "2".to_string(),
                name: "calc.add".to_string(),
                content: vec![ResultContent::Text(
                    "Not run: the task was canceled.".to_string(),
                )],
                is_error: true,
            }),
        ];
        assert_eq!(
            format!("{:?}", exchange.request.input),
            format!("{expected:?}")
        );
    }

    #[test]
    fn no_closing_exchange_is_necessary_when_nothing_left_to_answer() {
        assert!(closing_exchange(Vec::new(), &VecDeque::new(), "unused").is_none());
    }

    #[test]
    fn pending_calls_reason_for_not_running_matches_the_reason_the_task_ended() {
        assert_eq!(not_run_reason(TaskState::Canceled), "the task was canceled");
        assert_eq!(
            not_run_reason(TaskState::Completed),
            "the model's response ended without requesting tools"
        );
    }

    #[test]
    fn tool_calls_ordered_in_output() {
        let output = [
            ModelOutputItem::Text("Adding.".to_string()),
            ModelOutputItem::ToolCall(call("1")),
            ModelOutputItem::ToolCall(call("2")),
        ];
        let ids: Vec<String> = tool_calls(&output)
            .into_iter()
            .map(|call| call.id)
            .collect();
        assert_eq!(ids, ["1", "2"]);
    }

    #[test]
    fn the_turn_limit_is_a_whole_number_greater_than_0() {
        let parse = |value: &str| {
            parse_config(
                MAX_TURNS_CONFIG,
                Some(value.to_string()),
                DEFAULT_MAX_TURNS,
                "a whole number greater than 0",
            )
        };
        assert_eq!(parse("3").unwrap().get(), 3);
        assert_eq!(
            parse("0").unwrap_err(),
            "'max-turns' must be a whole number greater than 0, got '0'"
        );
    }

    #[test]
    fn a_tool_result_keeps_media_including_a_blob_resource() {
        use composable::tools::types::{
            BlobResourceContents, EmbeddedResource, ImageContent, ResourceLink, TextContent,
        };

        let blocks = [
            ContentBlock::Text(TextContent {
                text: "3".to_string(),
                annotations: None,
                meta: Vec::new(),
            }),
            ContentBlock::Image(ImageContent {
                data: "iVBORw0KGgo=".to_string(),
                mime_type: "image/png".to_string(),
                annotations: None,
                meta: Vec::new(),
            }),
            ContentBlock::ResourceLink(ResourceLink {
                uri: "file:///a.txt".to_string(),
                name: None,
                description: None,
                mime_type: None,
                annotations: None,
                meta: Vec::new(),
            }),
            ContentBlock::Resource(EmbeddedResource {
                resource_data: ResourceContents::Blob(BlobResourceContents {
                    uri: "file:///a.bin".to_string(),
                    mime_type: None,
                    blob: "AAAA".to_string(),
                    meta: Vec::new(),
                }),
                annotations: None,
                meta: Vec::new(),
            }),
        ];
        let content: Vec<ResultContent> = blocks.iter().map(result_content).collect();
        let expected = vec![
            ResultContent::Text("3".to_string()),
            ResultContent::Media(ContentSource::Data(MediaData {
                mime_type: "image/png".to_string(),
                data: "iVBORw0KGgo=".to_string(),
            })),
            ResultContent::Media(ContentSource::Url("file:///a.txt".to_string())),
            ResultContent::Media(ContentSource::Data(MediaData {
                mime_type: "application/octet-stream".to_string(),
                data: "AAAA".to_string(),
            })),
        ];
        assert_eq!(format!("{content:?}"), format!("{expected:?}"));
    }
}
