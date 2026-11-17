//! Wraps a `composable:agents/agent` component and emits an `invoke_agent`
//! span for each invocation, following the OpenTelemetry GenAI semantic
//! conventions. The span is open while the agent runs, so any spans started
//! by its model calls are its children.

wit_bindgen::generate!({
    path: "wit",
    world: "agent-otel-interceptor",
    with: {
        "composable:models/types@0.1.0": agent_interceptor_support::model_types,
        "composable:agents/types@0.1.0": agent_interceptor_support::types,
        "composable:agents/agent@0.1.0": agent_interceptor_support::agent,
    },
    generate_all,
});

use agent_interceptor_support::model_types::ModelFinishReason;
use agent_interceptor_support::types::{
    AgentError, AgentEvent, AgentFinishReason, AgentInvocation, AgentMetadata, AgentRequest,
    AgentResponse, PartContent, TaskState, TaskStatus,
};
use agent_interceptor_support::{AgentInterceptor, InvocationInterceptor, Outcome, agent};
use exports::composable::agents::agent::Guest;
use wasi::clocks::system_clock::{self, Instant};
use wasi::otel::{logs, tracing, types};
use wit_bindgen::rt::async_support::{FutureReader, FutureWriter, StreamReader, StreamWriter};

const SCOPE_NAME: &str = "modulewise.composable.agents.agent";
const SCOPE_VERSION: &str = env!("CARGO_PKG_VERSION");

// The severity number of WARN.
const WARN_SEVERITY: u8 = 13;

struct Otel;

impl AgentInterceptor for Otel {
    type Invocation = InvokeSpan;

    async fn new_invocation(request: &mut AgentRequest) -> Result<InvokeSpan, AgentError> {
        let metadata = agent::metadata().await.ok();
        Ok(InvokeSpan::start(
            metadata.as_ref(),
            &request.accepted_output_modes,
        ))
    }

    fn event_stream() -> (StreamWriter<AgentEvent>, StreamReader<AgentEvent>) {
        wit_stream::new()
    }

    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>) {
        wit_future::new(default)
    }

    fn cancel_future(default: fn()) -> (FutureWriter<()>, FutureReader<()>) {
        wit_future::new(default)
    }
}

// An `invoke_agent` span, from the start of an invocation to its outcome.
struct InvokeSpan {
    context: tracing::SpanContext,
    parent_span_id: String,
    name: String,
    start: Instant,
    attributes: Vec<types::KeyValue>,
}

impl InvocationInterceptor for InvokeSpan {
    fn on_invocation(&mut self, invocation: &AgentInvocation) {
        self.add("gen_ai.conversation.id", invocation.context_id.as_str());
    }

    fn on_result(self, outcome: &Outcome) {
        match outcome {
            Ok(response) => self.end_with_response(response),
            Err(error) => self.end_with_error(error),
        }
    }

    fn on_error(self, error: AgentError) -> AgentError {
        self.end_with_error(&error);
        error
    }
}

impl InvokeSpan {
    // Starts a span as a child of the current span, or in a new trace when
    // there is none, with the agent's metadata and the requested output type.
    fn start(metadata: Option<&AgentMetadata>, accepted_output_modes: &[String]) -> Self {
        let start = system_clock::now();
        let parent = tracing::current_span_context();
        let (trace_id, parent_span_id, trace_flags, trace_state) = if is_empty_context(&parent) {
            (
                new_trace_id(),
                String::new(),
                tracing::TraceFlags::SAMPLED,
                Vec::new(),
            )
        } else {
            (
                parent.trace_id,
                parent.span_id,
                parent.trace_flags,
                parent.trace_state,
            )
        };
        let context = tracing::SpanContext {
            trace_id,
            span_id: new_span_id(),
            trace_flags,
            is_remote: false,
            trace_state,
        };
        tracing::on_start(&context);
        let mut span = InvokeSpan {
            context,
            parent_span_id,
            name: "invoke_agent".to_string(),
            start,
            attributes: vec![kv("gen_ai.operation.name", "invoke_agent")],
        };
        if let Some(metadata) = metadata {
            span.name = format!("invoke_agent {}", metadata.name);
            span.add("gen_ai.agent.name", metadata.name.as_str());
            span.add("gen_ai.agent.description", metadata.description.as_str());
            span.add("gen_ai.agent.version", metadata.version.as_str());
        }
        if let [mode] = accepted_output_modes
            && let Some(output_type) = output_type(mode)
        {
            span.add("gen_ai.output.type", output_type);
        }
        span
    }

    // Ends the span with the task's response. A `failed` or `rejected` task
    // is an error, and also emits an exception event.
    fn end_with_response(mut self, response: &AgentResponse) {
        let (input, output) = total_usage(response);
        self.add_some("gen_ai.usage.input_tokens", input);
        self.add_some("gen_ai.usage.output_tokens", output);
        if let Some(reason) = &response.finish_reason {
            self.add(
                "gen_ai.response.finish_reasons",
                vec![raw_finish_reason(reason)],
            );
        }
        let error_type = match response.status.state {
            TaskState::Failed => Some("_OTHER"),
            TaskState::Rejected => Some("rejected"),
            _ => None,
        };
        match error_type {
            Some(error_type) => {
                let description = status_text(&response.status)
                    .unwrap_or_else(|| format!("the task ended {error_type}"));
                self.end_failed(error_type, description);
            }
            None => self.end(tracing::Status::Unset),
        }
    }

    // Ends the span with the agent's error.
    fn end_with_error(self, error: &AgentError) {
        self.end_failed(&error.code.to_string(), error.message.clone());
    }

    fn end_failed(mut self, error_type: &str, description: String) {
        self.add("error.type", error_type);
        self.emit_exception(&description);
        self.end(tracing::Status::Error(description));
    }

    fn end(self, status: tracing::Status) {
        tracing::on_end(&tracing::SpanData {
            span_context: self.context,
            parent_span_id: self.parent_span_id,
            span_kind: tracing::SpanKind::Internal,
            name: self.name,
            start_time: self.start,
            end_time: system_clock::now(),
            attributes: self.attributes,
            events: Vec::new(),
            links: Vec::new(),
            status,
            instrumentation_scope: scope(),
            dropped_attributes: 0,
            dropped_events: 0,
            dropped_links: 0,
        });
    }

    // Emits a `gen_ai.invoke_agent.exception` event, in this span.
    fn emit_exception(&self, message: &str) {
        logs::on_emit(&logs::LogRecord {
            timestamp: Some(system_clock::now()),
            observed_timestamp: None,
            severity_text: Some("WARN".to_string()),
            severity_number: Some(WARN_SEVERITY),
            body: None,
            attributes: Some(vec![kv("exception.message", message)]),
            event_name: Some("gen_ai.invoke_agent.exception".to_string()),
            resource: None,
            instrumentation_scope: Some(scope()),
            trace_id: Some(self.context.trace_id.clone()),
            span_id: Some(self.context.span_id.clone()),
            trace_flags: Some(self.context.trace_flags),
        });
    }

    fn add(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.attributes.push(kv(key, value));
    }

    fn add_some(&mut self, key: &str, value: Option<impl Into<serde_json::Value>>) {
        if let Some(value) = value {
            self.add(key, value);
        }
    }
}

// The conventions' output type for an accepted output mode, if one applies.
fn output_type(mode: &str) -> Option<&'static str> {
    match mode {
        "text/plain" => Some("text"),
        "application/json" => Some("json"),
        mode if mode.starts_with("image/") => Some("image"),
        _ => None,
    }
}

// The input and output token counts of the response's exchanges, summed over
// the exchanges that reported them.
fn total_usage(response: &AgentResponse) -> (Option<u64>, Option<u64>) {
    let sum = |count: fn(&agent_interceptor_support::model_types::TokenUsage) -> Option<u64>| {
        response
            .exchanges
            .iter()
            .filter_map(|exchange| count(&exchange.usage))
            .reduce(|total, count| total + count)
    };
    (
        sum(|usage| usage.input_tokens),
        sum(|usage| usage.output_tokens),
    )
}

// The model's own finish reason, or `max_turns`.
fn raw_finish_reason(reason: &AgentFinishReason) -> &str {
    match reason {
        AgentFinishReason::Model(
            ModelFinishReason::Completed(raw)
            | ModelFinishReason::ToolRequested(raw)
            | ModelFinishReason::Truncated(raw)
            | ModelFinishReason::Stopped(raw)
            | ModelFinishReason::Blocked(raw)
            | ModelFinishReason::Paused(raw)
            | ModelFinishReason::Other(raw),
        ) => raw,
        AgentFinishReason::MaxTurns => "max_turns",
    }
}

// The text of a status's message, if it has any.
fn status_text(status: &TaskStatus) -> Option<String> {
    let text: String = status
        .message
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match &part.content {
            PartContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then_some(text)
}

// An attribute, with its value as JSON.
fn kv(key: &str, value: impl Into<serde_json::Value>) -> types::KeyValue {
    types::KeyValue {
        key: key.to_string(),
        value: value.into().to_string(),
    }
}

fn scope() -> types::InstrumentationScope {
    types::InstrumentationScope {
        name: SCOPE_NAME.to_string(),
        version: Some(SCOPE_VERSION.to_string()),
        schema_url: None,
        attributes: Vec::new(),
    }
}

fn new_span_id() -> String {
    format!("{:016x}", wasi::random::random::get_random_u64())
}

fn new_trace_id() -> String {
    format!(
        "{:016x}{:016x}",
        wasi::random::random::get_random_u64(),
        wasi::random::random::get_random_u64()
    )
}

// The host returns all-zero trace and span ids when no span is open and no
// trace was propagated.
fn is_empty_context(context: &tracing::SpanContext) -> bool {
    context.trace_id.chars().all(|c| c == '0') && context.span_id.chars().all(|c| c == '0')
}

struct Component;

impl Guest for Component {
    async fn metadata() -> Result<AgentMetadata, String> {
        agent_interceptor_support::metadata::<Otel>().await
    }

    async fn invoke(
        request: AgentRequest,
        cancel: FutureReader<()>,
    ) -> Result<AgentInvocation, AgentError> {
        agent_interceptor_support::invoke::<Otel>(request, cancel).await
    }
}

export!(Component);
