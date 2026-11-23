//! The A2A server span of each request, following the OpenTelemetry GenAI
//! semantic conventions for A2A.

use std::collections::HashMap;
use std::time::SystemTime;

use a2a_json as a2a;
use a2a_json::jsonrpc::{ERROR_INFO_DOMAIN, ERROR_INFO_TYPE, Error};
use axum::http::HeaderMap;
use composable_agents::types::AgentMetadata;
use composable_runtime::PROPAGATED_HEADERS;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{Span as _, SpanKind, Status, Tracer as _};
use opentelemetry::{Array, KeyValue, StringValue, Value};
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, Span};

/// The instrumentation scope of the server's spans.
pub(crate) const SCOPE_NAME: &str = "modulewise.composable.a2a.server";

/// A tracer provider that exports spans to the OTLP `endpoint` with
/// `protocol`, `grpc` or `http/protobuf`, as the service `service_name`.
/// Call it within a tokio runtime.
pub(crate) fn build_tracer_provider(
    endpoint: &str,
    protocol: &str,
    service_name: &str,
) -> anyhow::Result<SdkTracerProvider> {
    let exporter = match protocol {
        "grpc" => SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?,
        "http/protobuf" => SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .build()?,
        other => anyhow::bail!("unsupported OTLP protocol '{other}'"),
    };
    let resource = Resource::builder()
        .with_attribute(KeyValue::new("service.name", service_name.to_string()))
        .build();
    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_span_processor(BatchSpanProcessor::builder(exporter, Tokio).build())
        .build())
}

/// The A2A server span of one request. Without a tracer it records nothing,
/// and its context is the request's inbound trace context.
pub(crate) struct ServerSpan {
    span: Option<Span>,
    context: HashMap<String, String>,
}

impl ServerSpan {
    /// Starts the span of a request for the A2A method `method`, received at
    /// `received_at`. Its parent is the request's `traceparent`, if any.
    pub(crate) fn start(
        tracer: Option<&SdkTracer>,
        method: &str,
        received_at: SystemTime,
        headers: &HeaderMap,
    ) -> Self {
        let mut context: HashMap<String, String> = PROPAGATED_HEADERS
            .iter()
            .filter_map(|name| {
                let value = headers.get(*name)?.to_str().ok()?;
                Some((name.to_string(), value.to_string()))
            })
            .collect();
        let Some(tracer) = tracer else {
            return ServerSpan {
                span: None,
                context,
            };
        };
        let parent = TraceContextPropagator::new().extract(&context);
        let span = tracer
            .span_builder(method.to_string())
            .with_kind(SpanKind::Server)
            .with_start_time(received_at)
            .with_attributes([KeyValue::new("a2a.method.name", method.to_string())])
            .start_with_context(tracer, &parent);
        // The work of the request is traced beneath this span.
        let span_context = span.span_context();
        context.insert(
            "traceparent".to_string(),
            format!(
                "00-{:032x}-{:016x}-{:02x}",
                span_context.trace_id(),
                span_context.span_id(),
                span_context.trace_flags()
            ),
        );
        ServerSpan {
            span: Some(span),
            context,
        }
    }

    pub(crate) fn record(&mut self, attributes: Vec<KeyValue>) {
        if let Some(span) = &mut self.span {
            span.set_attributes(attributes);
        }
    }

    /// Records `error` as the reason the request failed. `error.type` is the
    /// error's A2A reason, or its code when it has none.
    pub(crate) fn fail(&mut self, error: &Error) {
        if let Some(span) = &mut self.span {
            let error_type = a2a_reason(error).unwrap_or_else(|| error.code.to_string());
            span.set_attribute(KeyValue::new("error.type", error_type));
            span.set_status(Status::error(error.message.clone()));
        }
    }

    /// The trace context of the work the request does, with this span's
    /// `traceparent`.
    pub(crate) fn context(&self) -> HashMap<String, String> {
        self.context.clone()
    }

    pub(crate) fn end(mut self) {
        if let Some(span) = &mut self.span {
            span.end();
        }
    }
}

// The reason in the error's `google.rpc.ErrorInfo` with A2A's domain.
fn a2a_reason(error: &Error) -> Option<String> {
    let details = error.data.as_ref()?.as_array()?;
    details.iter().find_map(|detail| {
        let is_a2a = detail["@type"] == ERROR_INFO_TYPE && detail["domain"] == ERROR_INFO_DOMAIN;
        is_a2a
            .then(|| detail["reason"].as_str().map(str::to_string))
            .flatten()
    })
}

/// The span attributes of the agent, from its metadata.
pub(crate) fn agent_attributes(metadata: &AgentMetadata) -> Vec<KeyValue> {
    vec![
        KeyValue::new("gen_ai.agent.name", metadata.name.clone()),
        KeyValue::new("gen_ai.agent.description", metadata.description.clone()),
        KeyValue::new("gen_ai.agent.version", metadata.version.clone()),
    ]
}

/// The span attributes of an A2A request or response.
pub(crate) trait SpanAttributes {
    fn attributes(&self) -> Vec<KeyValue>;
}

impl SpanAttributes for a2a::SendMessageRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        let message = &self.message;
        let mut attributes = tenant(&self.tenant);
        attributes.push(KeyValue::new("a2a.message.id", message.message_id.clone()));
        attributes.extend(task_id(&message.task_id));
        attributes.extend(conversation_id(&message.context_id));
        if !message.reference_task_ids.is_empty() {
            let ids = message
                .reference_task_ids
                .iter()
                .cloned()
                .map(StringValue::from)
                .collect();
            attributes.push(KeyValue::new(
                "a2a.message.reference_task_ids",
                Value::Array(Array::String(ids)),
            ));
        }
        attributes
    }
}

impl SpanAttributes for a2a::GetTaskRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.id)
    }
}

impl SpanAttributes for a2a::CancelTaskRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.id)
    }
}

impl SpanAttributes for a2a::SubscribeToTaskRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.id)
    }
}

impl SpanAttributes for a2a::ListTasksRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = tenant(&self.tenant);
        attributes.extend(conversation_id(&self.context_id));
        attributes
    }
}

impl SpanAttributes for a2a::GetExtendedAgentCardRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        tenant(&self.tenant)
    }
}

impl SpanAttributes for a2a::TaskPushNotificationConfig {
    fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = tenant(&self.tenant);
        attributes.extend(task_id(&self.task_id));
        attributes
    }
}

impl SpanAttributes for a2a::GetTaskPushNotificationConfigRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.task_id)
    }
}

impl SpanAttributes for a2a::ListTaskPushNotificationConfigsRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.task_id)
    }
}

impl SpanAttributes for a2a::DeleteTaskPushNotificationConfigRequest {
    fn attributes(&self) -> Vec<KeyValue> {
        task_request(&self.tenant, &self.task_id)
    }
}

impl SpanAttributes for a2a::Task {
    fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = vec![KeyValue::new("a2a.task.id", self.id.clone())];
        attributes.extend(task_state(self.status.state));
        attributes.extend(conversation_id(&self.context_id));
        attributes
    }
}

impl SpanAttributes for a2a::Message {
    fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = task_id(&self.task_id);
        attributes.extend(conversation_id(&self.context_id));
        attributes
    }
}

impl SpanAttributes for a2a::SendMessageResponse {
    fn attributes(&self) -> Vec<KeyValue> {
        match self {
            a2a::SendMessageResponse::Task(task) => task.attributes(),
            a2a::SendMessageResponse::Message(message) => message.attributes(),
        }
    }
}

// The attributes of a request for one task.
fn task_request(tenant_name: &Option<String>, id: &str) -> Vec<KeyValue> {
    let mut attributes = tenant(tenant_name);
    attributes.push(KeyValue::new("a2a.task.id", id.to_string()));
    attributes
}

fn tenant(tenant: &Option<String>) -> Vec<KeyValue> {
    optional("a2a.tenant", tenant)
}

fn task_id(task_id: &Option<String>) -> Vec<KeyValue> {
    optional("a2a.task.id", task_id)
}

fn conversation_id(context_id: &Option<String>) -> Vec<KeyValue> {
    optional("gen_ai.conversation.id", context_id)
}

fn task_state(state: a2a::TaskState) -> Option<KeyValue> {
    let state = serde_json::to_value(state).ok()?;
    Some(KeyValue::new("a2a.task.state", state.as_str()?.to_string()))
}

fn optional(key: &'static str, value: &Option<String>) -> Vec<KeyValue> {
    value
        .iter()
        .map(|value| KeyValue::new(key, value.clone()))
        .collect()
}
