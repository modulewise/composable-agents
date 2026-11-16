//! Wraps a `composable:models/client` session and emits a `chat` span for each
//! call a client receives, following the OpenTelemetry GenAI semantic
//! conventions. The span ends when the call ends.

wit_bindgen::generate!({
    path: "wit",
    world: "model-client-otel-interceptor",
    with: {
        "composable:models/types@0.1.0": model_client_interceptor_support::types,
        "composable:models/client@0.1.0": model_client_interceptor_support::client,
    },
    generate_all,
});

use exports::composable::models::client::{Guest, GuestSession};
use model_client_interceptor_support::types::{
    ContentDelta, ModelCall, ModelDelta, ModelError, ModelExchange, ModelFinishReason,
    ModelRequest, ModelResponse, ModelSettings, SessionError, TokenUsage,
};
use model_client_interceptor_support::{
    CallInterceptor, Ending, Flow, Outcome, Session, SessionInterceptor,
};
use wasi::clocks::system_clock::{self, Instant};
use wasi::otel::{logs, tracing, types};
use wit_bindgen::rt::async_support::{FutureReader, FutureWriter, StreamReader, StreamWriter};

const SCOPE_NAME: &str = "modulewise.composable.models.client";
const SCOPE_VERSION: &str = env!("CARGO_PKG_VERSION");

// The severity number of WARN.
const WARN_SEVERITY: u8 = 13;

struct Otel;

impl SessionInterceptor for Otel {
    type Call = ChatSpan;

    fn new(
        _settings: &mut ModelSettings,
        _history: &mut Vec<ModelExchange>,
    ) -> Result<Self, SessionError> {
        Ok(Otel)
    }

    fn new_call(&self, _request: &mut ModelRequest) -> Result<ChatSpan, ModelError> {
        Ok(ChatSpan::start())
    }

    fn delta_stream() -> (StreamWriter<ModelDelta>, StreamReader<ModelDelta>) {
        wit_stream::new()
    }

    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>) {
        wit_future::new(default)
    }
}

// A `chat` span, from the start of a call to its end.
struct ChatSpan {
    context: tracing::SpanContext,
    parent_span_id: String,
    name: String,
    start: Instant,
    attributes: Vec<types::KeyValue>,
    // When the first delta with output arrived.
    first_chunk: Option<Instant>,
    // The latest counts from a usage delta.
    usage: Option<TokenUsage>,
}

impl CallInterceptor for ChatSpan {
    // Adds the model, provider, server and parameters of `call`.
    fn on_describe(&mut self, call: &ModelCall) {
        self.name = format!("chat {}", call.request_model);
        self.add("gen_ai.provider.name", call.provider.as_str());
        self.add("gen_ai.request.model", call.request_model.as_str());
        if call.provider == "openai" && call.api == "chat-completions" {
            self.add("openai.api.type", "chat_completions");
        }
        if let Some(server) = &call.server {
            self.add("server.address", server.address.as_str());
            self.add("server.port", server.port);
        }
        if call.response.is_ok() {
            self.add("gen_ai.request.stream", true);
        }
        let parameters = &call.parameters;
        self.add_some("gen_ai.request.max_tokens", parameters.max_tokens);
        self.add_some("gen_ai.request.temperature", parameters.temperature);
        self.add_some("gen_ai.request.top_p", parameters.top_p);
        self.add_some("gen_ai.request.top_k", parameters.top_k);
        self.add_some("gen_ai.request.seed", parameters.seed);
        self.add_some(
            "gen_ai.request.stop_sequences",
            parameters.stop_sequences.clone(),
        );
        self.add_some(
            "gen_ai.request.reasoning.level",
            parameters.reasoning_level.clone(),
        );
        self.add_some("gen_ai.output.type", parameters.output_type.clone());
    }

    // Notes when the first delta with output arrives, and the latest counts.
    fn on_delta(&mut self, delta: &mut ModelDelta) -> Flow {
        match delta {
            ModelDelta::Usage(counts) => self.usage = Some(*counts),
            ModelDelta::Content(content) if self.first_chunk.is_none() && has_output(content) => {
                self.first_chunk = Some(system_clock::now());
            }
            ModelDelta::Content(_) => {}
        }
        Flow::Continue
    }

    fn on_end(self, ending: Ending, outcome: Outcome) -> Outcome {
        self.end(ending, &outcome);
        outcome
    }
}

// Whether a content delta has output, which makes it a chunk for
// `gen_ai.response.time_to_first_chunk`.
fn has_output(content: &ContentDelta) -> bool {
    match content {
        ContentDelta::Text(text) | ContentDelta::Reasoning(text) => !text.is_empty(),
        ContentDelta::ToolCall(call) => call.name.is_some() || !call.arguments.is_empty(),
    }
}

impl ChatSpan {
    // Starts a span as a child of the current span, or in a new trace when
    // there is none.
    fn start() -> Self {
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
        ChatSpan {
            context,
            parent_span_id,
            name: "chat".to_string(),
            start,
            attributes: vec![kv("gen_ai.operation.name", "chat")],
            first_chunk: None,
            usage: None,
        }
    }

    // Ends the span with how the call ended and its outcome. A failure also
    // emits an exception event. A canceled call is not a failure.
    fn end(mut self, ending: Ending, outcome: &Outcome) {
        let end = system_clock::now();
        if let Some(first_chunk) = self.first_chunk {
            self.add(
                "gen_ai.response.time_to_first_chunk",
                seconds_between(&self.start, &first_chunk),
            );
        }
        let (status, usage) = match outcome {
            Ok(response) => {
                self.add_response(response);
                (tracing::Status::Unset, Some(response.usage))
            }
            Err(_) if ending == Ending::Canceled => {
                self.add("gen_ai.response.finish_reasons", vec!["error"]);
                self.add("gen_ai.response.status", "cancelled");
                (tracing::Status::Unset, self.usage)
            }
            Err(error) => {
                self.add("gen_ai.response.finish_reasons", vec!["error"]);
                self.add("error.type", error_type(error));
                self.emit_exception(error, end);
                (tracing::Status::Error(error.message.clone()), self.usage)
            }
        };
        if let Some(usage) = usage {
            self.add_usage(&usage);
        }
        tracing::on_end(&tracing::SpanData {
            span_context: self.context,
            parent_span_id: self.parent_span_id,
            span_kind: tracing::SpanKind::Client,
            name: self.name,
            start_time: self.start,
            end_time: end,
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

    fn add_response(&mut self, response: &ModelResponse) {
        self.add_some("gen_ai.response.model", response.model.clone());
        self.add_some("gen_ai.response.id", response.response_id.clone());
        self.add(
            "gen_ai.response.finish_reasons",
            vec![raw_finish_reason(&response.finish_reason)],
        );
    }

    fn add_usage(&mut self, usage: &TokenUsage) {
        self.add_some("gen_ai.usage.input_tokens", usage.input_tokens);
        self.add_some("gen_ai.usage.output_tokens", usage.output_tokens);
        self.add_nonzero(
            "gen_ai.usage.cache_read.input_tokens",
            usage.cache_read_tokens,
        );
        self.add_nonzero(
            "gen_ai.usage.cache_write.input_tokens",
            usage.cache_write_tokens,
        );
        self.add_nonzero(
            "gen_ai.usage.reasoning.output_tokens",
            usage.reasoning_tokens,
        );
    }

    // Emits a `gen_ai.client.inference.exception` event for `error`, in this
    // span.
    fn emit_exception(&self, error: &ModelError, time: Instant) {
        logs::on_emit(&logs::LogRecord {
            timestamp: Some(time),
            observed_timestamp: None,
            severity_text: Some("WARN".to_string()),
            severity_number: Some(WARN_SEVERITY),
            body: None,
            attributes: Some(vec![kv("exception.message", error.message.as_str())]),
            event_name: Some("gen_ai.client.inference.exception".to_string()),
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

    fn add_nonzero(&mut self, key: &str, value: Option<u64>) {
        self.add_some(key, value.filter(|&count| count > 0));
    }
}

// The provider's own finish reason.
fn raw_finish_reason(reason: &ModelFinishReason) -> &str {
    match reason {
        ModelFinishReason::Completed(raw)
        | ModelFinishReason::ToolRequested(raw)
        | ModelFinishReason::Truncated(raw)
        | ModelFinishReason::Stopped(raw)
        | ModelFinishReason::Blocked(raw)
        | ModelFinishReason::Paused(raw)
        | ModelFinishReason::Other(raw) => raw,
    }
}

// The provider's error code, else the status code, else `_OTHER`.
fn error_type(error: &ModelError) -> String {
    match (&error.code, error.status) {
        (Some(code), _) => code.clone(),
        (None, Some(status)) => status.to_string(),
        (None, None) => "_OTHER".to_string(),
    }
}

fn seconds_between(start: &Instant, end: &Instant) -> f64 {
    (end.seconds - start.seconds) as f64
        + (f64::from(end.nanoseconds) - f64::from(start.nanoseconds)) / 1e9
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

struct TracedSession(Session<Otel>);

impl GuestSession for TracedSession {
    fn new(settings: ModelSettings, history: Vec<ModelExchange>) -> Result<Self, SessionError> {
        Session::new(settings, history).map(TracedSession)
    }

    async fn call(&self, request: ModelRequest) -> Result<ModelCall, ModelError> {
        self.0.call(request).await
    }

    fn exchanges(&self) -> Vec<ModelExchange> {
        self.0.exchanges()
    }
}

struct Component;

impl Guest for Component {
    type Session = TracedSession;
}

export!(Component);
