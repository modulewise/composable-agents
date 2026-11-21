//! A2A's JSON-RPC binding over HTTP. It reads a JSON-RPC request from an HTTP
//! request, calls the operation the request specifies, and writes the HTTP
//! response. The response to a streaming operation is an event stream of
//! JSON-RPC responses.

use std::convert::Infallible;
use std::time::SystemTime;

use a2a_json as a2a;
use a2a_json::jsonrpc::{Error, Request, Response, VERSION, code};
use a2a_json::method;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use composable_runtime::{PROPAGATION_CONTEXT, PropagationContext};
use http_body_util::LengthLimitError;
use opentelemetry::KeyValue;
use opentelemetry_sdk::trace::SdkTracer;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::handler::{CallContext, RequestHandler};
use crate::span::{ServerSpan, SpanAttributes, agent_attributes};
use crate::store::{ExchangeStore, TaskStore};

// The largest request body that is read.
const MAX_BODY: usize = 4 * 1024 * 1024;

// The service parameters, as HTTP header names.
const VERSION_PARAMETER: &str = "a2a-version";
const EXTENSIONS_PARAMETER: &str = "a2a-extensions";

// The version of a request that names none.
const UNSPECIFIED_VERSION: &str = "0.3";

/// Handles an HTTP request to an agent's JSON-RPC endpoint.
pub(crate) async fn handle<T: TaskStore, E: ExchangeStore>(
    handler: &RequestHandler<T, E>,
    tracer: Option<&SdkTracer>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let received_at = SystemTime::now();
    let (parts, body) = request.into_parts();
    if !has_json_content_type(&parts.headers) {
        return status_response(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let body = match to_bytes(body, MAX_BODY).await {
        Ok(body) => body,
        Err(error) => {
            let too_large = error.into_inner().is::<LengthLimitError>();
            return status_response(if too_large {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            });
        }
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        let error = Error::new(code::PARSE_ERROR, "the request is not valid JSON");
        return json_response(Response::error(Value::Null, error), &[]);
    };
    let request = match serde_json::from_value::<Request>(value) {
        Ok(request) if request.jsonrpc == VERSION && valid_id(request.id.as_ref()) => request,
        _ => {
            let error = Error::new(
                code::INVALID_REQUEST,
                format!("the request is not a JSON-RPC {VERSION} request"),
            );
            return json_response(Response::error(Value::Null, error), &[]);
        }
    };
    // A request with no id is a notification, which has no response.
    let Some(id) = request.id else {
        return status_response(StatusCode::NO_CONTENT);
    };
    if !method::ALL.contains(&request.method.as_str()) {
        let error = Error::new(
            code::METHOD_NOT_FOUND,
            format!("method not found: {}", request.method),
        );
        return json_response(Response::error(id, error), &[]);
    }

    // The span starts at the time the request was received.
    let mut span = ServerSpan::start(tracer, &request.method, received_at, &parts.headers);
    let version = requested_version(&parts.headers, parts.uri.query());
    span.record(agent_attributes(handler.metadata()));
    span.record(vec![KeyValue::new("a2a.protocol.version", version.clone())]);
    if let Some(request_id) = request_id(&id) {
        span.record(vec![KeyValue::new("jsonrpc.request.id", request_id)]);
    }

    if !supports_version(&version) {
        let error = Error::new(
            code::VERSION_NOT_SUPPORTED,
            format!(
                "A2A version {version} is not supported, only {}",
                a2a::PROTOCOL_VERSION
            ),
        );
        return failed(id, error, span);
    }
    let mut context = CallContext {
        requested_extensions: requested_extensions(&parts.headers),
        activated_extensions: Vec::new(),
    };
    if let Err(error) = handler.validate(&request.method, &context) {
        return failed(id, error, span);
    }

    // The work of the operation is traced beneath the span.
    let propagation = PropagationContext {
        entries: span.context(),
    };
    let params = request.params.unwrap_or(Value::Null);
    match request.method.as_str() {
        method::SEND_STREAMING_MESSAGE | method::SUBSCRIBE_TO_TASK => {
            let streamed = stream(handler, &request.method, params, &mut context, &mut span);
            match PROPAGATION_CONTEXT.scope(Some(propagation), streamed).await {
                Ok(responses) => event_stream(id, responses, &context.activated_extensions, span),
                // A streaming operation that fails before it has a stream has
                // one response.
                Err(error) => failed(id, error, span),
            }
        }
        _ => {
            let called = call(handler, &request.method, params, &mut context, &mut span);
            let result = PROPAGATION_CONTEXT.scope(Some(propagation), called).await;
            if let Err(error) = &result {
                span.fail(error);
            }
            span.end();
            json_response(Response { id, result }, &context.activated_extensions)
        }
    }
}

// The response with `error`, after it is recorded on the span.
fn failed(id: Value, error: Error, mut span: ServerSpan) -> axum::response::Response {
    span.fail(&error);
    span.end();
    json_response(Response::error(id, error), &[])
}

// The request's id as a string, or `None` for a null id.
fn request_id(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

// Calls the streaming operation named `method`, and returns its stream of
// responses.
async fn stream<T: TaskStore, E: ExchangeStore>(
    handler: &RequestHandler<T, E>,
    method: &str,
    params: Value,
    context: &mut CallContext,
    span: &mut ServerSpan,
) -> Result<mpsc::Receiver<a2a::StreamResponse>, Error> {
    if method == method::SEND_STREAMING_MESSAGE {
        let request = parsed(params, span)?;
        handler.send_streaming_message(request, context).await
    } else {
        let request = parsed(params, span)?;
        handler.subscribe_to_task(request, context).await
    }
}

// Calls the operation named `method` that has one response, and returns its
// result as JSON. Records the attributes of the request and the response on
// `span`.
async fn call<T: TaskStore, E: ExchangeStore>(
    handler: &RequestHandler<T, E>,
    method: &str,
    params: Value,
    context: &mut CallContext,
    span: &mut ServerSpan,
) -> Result<Value, Error> {
    match method {
        method::SEND_MESSAGE => {
            let request = parsed(params, span)?;
            recorded(handler.send_message(request, context).await?, span)
        }
        method::GET_TASK => {
            let request = parsed(params, span)?;
            recorded(handler.get_task(request, context).await?, span)
        }
        method::LIST_TASKS => {
            let request = parsed_or_default(params, span)?;
            result(handler.list_tasks(request, context).await?)
        }
        method::CANCEL_TASK => {
            let request = parsed(params, span)?;
            recorded(handler.cancel_task(request, context).await?, span)
        }
        method::CREATE_TASK_PUSH_NOTIFICATION_CONFIG => {
            let request = parsed(params, span)?;
            let config = handler
                .create_task_push_notification_config(request, context)
                .await?;
            recorded(config, span)
        }
        method::GET_TASK_PUSH_NOTIFICATION_CONFIG => {
            let request = parsed(params, span)?;
            let config = handler
                .get_task_push_notification_config(request, context)
                .await?;
            recorded(config, span)
        }
        method::LIST_TASK_PUSH_NOTIFICATION_CONFIGS => {
            let request = parsed(params, span)?;
            result(
                handler
                    .list_task_push_notification_configs(request, context)
                    .await?,
            )
        }
        method::DELETE_TASK_PUSH_NOTIFICATION_CONFIG => {
            let request = parsed(params, span)?;
            handler
                .delete_task_push_notification_config(request, context)
                .await?;
            Ok(json!({}))
        }
        method::GET_EXTENDED_AGENT_CARD => {
            let request = parsed_or_default(params, span)?;
            result(handler.get_extended_agent_card(request, context).await?)
        }
        other => Err(Error::new(
            code::METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

// `params` as the request of an operation, with its attributes recorded on
// `span`.
fn parsed<P: DeserializeOwned + SpanAttributes>(
    params: Value,
    span: &mut ServerSpan,
) -> Result<P, Error> {
    let request: P = serde_json::from_value(params).map_err(|e| {
        Error::new(
            code::INVALID_PARAMS,
            format!("the params are not valid: {e}"),
        )
    })?;
    span.record(request.attributes());
    Ok(request)
}

// `params` as the request of an operation whose fields are all optional, so
// that a request with no params is valid.
fn parsed_or_default<P: DeserializeOwned + Default + SpanAttributes>(
    params: Value,
    span: &mut ServerSpan,
) -> Result<P, Error> {
    if params.is_null() {
        let request = P::default();
        span.record(request.attributes());
        return Ok(request);
    }
    parsed(params, span)
}

// The result of an operation as JSON, with its attributes recorded on `span`.
fn recorded(
    response: impl Serialize + SpanAttributes,
    span: &mut ServerSpan,
) -> Result<Value, Error> {
    span.record(response.attributes());
    result(response)
}

// The result of an operation as JSON.
fn result(result: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(result).map_err(|e| {
        Error::new(
            code::INTERNAL_ERROR,
            format!("the result is not valid: {e}"),
        )
    })
}

// Whether `id` is valid as a request's id, which is a string, a number or
// null. A request with no id is a notification.
fn valid_id(id: Option<&Value>) -> bool {
    id.is_none_or(|id| id.is_string() || id.is_number() || id.is_null())
}

// Whether the request's content type is `application/json`, with or without
// parameters.
fn has_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

// The A2A version a request names in its header, else in its query, else the
// version of a request that names none.
fn requested_version(headers: &HeaderMap, query: Option<&str>) -> String {
    let from_header = headers
        .get(VERSION_PARAMETER)
        .and_then(|value| value.to_str().ok());
    let from_query = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            key.eq_ignore_ascii_case(VERSION_PARAMETER).then_some(value)
        });
    from_header
        .or(from_query)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .unwrap_or(UNSPECIFIED_VERSION)
        .to_string()
}

// Whether `version` has the major and minor numbers of the version the
// server supports. Any patch number is ignored.
fn supports_version(version: &str) -> bool {
    fn major_minor(version: &str) -> Option<(&str, &str)> {
        let mut numbers = version.split('.');
        Some((numbers.next()?, numbers.next()?))
    }
    major_minor(version).is_some() && major_minor(version) == major_minor(a2a::PROTOCOL_VERSION)
}

// The extension URIs a request lists in its extensions headers.
fn requested_extensions(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(EXTENSIONS_PARAMETER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
        .map(str::to_string)
        .collect()
}

// A response with `status` and no body.
fn status_response(status: StatusCode) -> axum::response::Response {
    let mut response = axum::response::Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

// A JSON-RPC response as a JSON body. An error response also has status 200.
fn json_response(response: Response, activated_extensions: &[String]) -> axum::response::Response {
    let body = serde_json::to_vec(&response).unwrap_or_default();
    with_headers(Body::from(body), "application/json", activated_extensions)
}

// An event stream with a JSON-RPC response for each of `responses`, each with
// the request's `id`. The span ends when the stream ends (when it is dropped).
fn event_stream(
    id: Value,
    responses: mpsc::Receiver<a2a::StreamResponse>,
    activated_extensions: &[String],
    mut span: ServerSpan,
) -> axum::response::Response {
    let events = ReceiverStream::new(responses).map(move |response| {
        // A stream transitions through task states, so the span has none.
        if let a2a::StreamResponse::Task(task) = &response {
            let attributes = task.attributes();
            span.record(
                attributes
                    .into_iter()
                    .filter(|attribute| attribute.key.as_str() != "a2a.task.state")
                    .collect(),
            );
        }
        let response = Response::result(id.clone(), json!(response));
        let event = sse::Event {
            event_type: None,
            data: serde_json::to_string(&response).unwrap_or_default(),
        };
        Ok::<_, Infallible>(Bytes::from(event.encode()))
    });
    let mut response = with_headers(
        Body::from_stream(events),
        "text/event-stream",
        activated_extensions,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

// A response with `body`, its content type, and the activated extensions if
// there are any.
fn with_headers(
    body: Body,
    content_type: &'static str,
    activated_extensions: &[String],
) -> axum::response::Response {
    let mut response = axum::response::Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if !activated_extensions.is_empty()
        && let Ok(value) = HeaderValue::from_str(&activated_extensions.join(", "))
    {
        headers.insert(EXTENSIONS_PARAMETER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn a_request_names_its_version_in_a_header_or_in_its_query() {
        let header = headers(&[("a2a-version", "1.0")]);
        assert_eq!(requested_version(&header, None), "1.0");
        assert_eq!(
            requested_version(&HeaderMap::new(), Some("a=b&A2A-Version=1.0")),
            "1.0"
        );
        // The header is used when both are present.
        assert_eq!(requested_version(&header, Some("A2A-Version=0.3")), "1.0");
    }

    #[test]
    fn a_request_that_names_no_version_has_version_0_3() {
        assert_eq!(requested_version(&HeaderMap::new(), None), "0.3");
        assert_eq!(
            requested_version(&headers(&[("a2a-version", " ")]), Some("a=b")),
            "0.3"
        );
    }

    #[test]
    fn a_version_is_supported_by_its_major_and_minor_numbers() {
        assert!(supports_version("1.0"));
        assert!(supports_version("1.0.3"));
        assert!(!supports_version("0.3"));
        assert!(!supports_version("1.1"));
        assert!(!supports_version("1"));
        assert!(!supports_version("latest"));
    }

    #[test]
    fn the_requested_extensions_are_the_uris_of_every_extensions_header() {
        let headers = headers(&[
            (
                "a2a-extensions",
                "https://example.com/a, https://example.com/b",
            ),
            ("a2a-extensions", "https://example.com/c,"),
        ]);
        assert_eq!(
            requested_extensions(&headers),
            [
                "https://example.com/a",
                "https://example.com/b",
                "https://example.com/c"
            ]
        );
        assert!(requested_extensions(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn a_request_id_is_a_string_a_number_or_null() {
        assert!(valid_id(None));
        assert!(valid_id(Some(&json!("a"))));
        assert!(valid_id(Some(&json!(1))));
        assert!(valid_id(Some(&Value::Null)));
        assert!(!valid_id(Some(&json!({}))));
        assert!(!valid_id(Some(&json!([1]))));
    }
}
