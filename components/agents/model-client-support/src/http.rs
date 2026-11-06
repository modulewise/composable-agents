//! Sends a call's HTTP request, retrying it after a recoverable failure.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;
use wit_bindgen::rt::async_support::{StreamReader, StreamResult, spawn_local};

use crate::componentized::http::client::{self, ErrorCode, HttpResponse, RequestOptions};
use crate::composable::agents::types::ModelError;
use crate::wit_stream;

// Backoff: 0.5s, doubled on each retry, at most 8s.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

/// POSTs the body and returns the streaming response. An error status
/// returns a `ModelError` with the status and the provider's error message.
///
/// A request that fails before its response starts is retried up to
/// `max_retries` times (see `should_retry`), waiting with `sleep` between
/// attempts.
pub async fn send_request<S, F>(
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
    max_retries: u32,
    sleep: S,
) -> Result<HttpResponse, ModelError>
where
    S: Fn(Duration) -> F,
    F: Future<Output = ()>,
{
    let mut retries = 0;
    loop {
        let sent = client::post(
            url.to_string(),
            headers.to_vec(),
            body_stream(body.clone()),
            Some(NO_REDIRECTS),
        )
        .await;
        let wait = match sent {
            Ok(response) if response.status < 300 => return Ok(response),
            Ok(response) if response.status < 400 => return Err(redirected(response.status)),
            Ok(response) => {
                let retry = should_retry(response.status, &response.headers);
                let wait = retry_delay(&response.headers, retries);
                let error = error_of(response).await;
                if !retry || retries >= max_retries {
                    return Err(error);
                }
                wait
            }
            // The request failed before a response arrived.
            Err(ErrorCode::Other(_)) if retries < max_retries => backoff(retries),
            Err(ErrorCode::Other(reason)) => {
                return Err(failure(reason.unwrap_or_else(|| {
                    "HTTP request failed (no reason given)".to_string()
                })));
            }
            Err(
                ErrorCode::RedirectLimitExceeded((response, _))
                | ErrorCode::RedirectRequiresBody((response, _)),
            ) => return Err(redirected(response.status)),
        };
        sleep(wait).await;
        retries += 1;
    }
}

const NO_REDIRECTS: RequestOptions = RequestOptions {
    connect_timeout_ms: None,
    first_byte_timeout_ms: None,
    between_bytes_timeout_ms: None,
    max_redirects: Some(0),
};

fn redirected(status: u16) -> ModelError {
    ModelError {
        message: format!("the provider redirected the request (status {status})"),
        status: Some(status as u32),
    }
}

// The request body as a stream.
fn body_stream(body: Vec<u8>) -> StreamReader<u8> {
    let (mut body_tx, body_rx) = wit_stream::new::<u8>();
    spawn_local(async move {
        let _ = body_tx.write_all(body).await;
    });
    body_rx
}

// The error for an error response: its status, and the provider's error
// message if the body includes one. Drains the body.
async fn error_of(response: HttpResponse) -> ModelError {
    let status = response.status;
    let mut err_body = Vec::new();
    let mut stream = response.body;
    loop {
        let (result, chunk) = stream.read(Vec::with_capacity(8192)).await;
        err_body.extend_from_slice(&chunk);
        if matches!(result, StreamResult::Dropped) {
            break;
        }
    }
    let message = match provider_error(&err_body) {
        Some((Some(kind), message)) => format!("provider returned {status} ({kind}): {message}"),
        Some((None, message)) => format!("provider returned {status}: {message}"),
        None => format!("provider returned {status}"),
    };
    ModelError {
        message,
        status: Some(status as u32),
    }
}

// The provider's error type and message from an error body: `error.type` and
// `error.message`, or `error` as a string. Nothing else is read.
fn provider_error(body: &[u8]) -> Option<(Option<String>, String)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    match body.get("error")? {
        Value::String(message) => Some((None, message.clone())),
        error => {
            let message = error.get("message")?.as_str()?.to_string();
            let kind = error
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some((kind, message))
        }
    }
}

fn failure(message: String) -> ModelError {
    ModelError {
        message,
        status: None,
    }
}

// A response header's value, matched by name case-insensitively.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

// Retries on 5xx and on 429 with a `retry-after` header. If present, the
// `x-should-retry` header takes precedence.
fn should_retry(status: u16, headers: &[(String, String)]) -> bool {
    let retry = status >= 500 || (status == 429 && header(headers, "retry-after").is_some());
    match header(headers, "x-should-retry") {
        Some("true") => true,
        Some("false") => false,
        _ => retry,
    }
}

// Waits `retry-after` seconds if present, otherwise backs off exponentially.
fn retry_delay(headers: &[(String, String)], retries: u32) -> Duration {
    header(headers, "retry-after")
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| backoff(retries))
}

// 0.5 seconds, doubled for each earlier retry, at most 8 seconds.
fn backoff(retries: u32) -> Duration {
    INITIAL_RETRY_DELAY
        .saturating_mul(2u32.saturating_pow(retries))
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn an_error_names_the_providers_error_and_nothing_else_of_the_body() {
        assert_eq!(
            provider_error(
                br#"{"type":"error","error":{"type":"invalid_request_error","message":"temperature: Extra inputs are not permitted"},"request_id":"req_1"}"#
            ),
            Some((
                Some("invalid_request_error".to_string()),
                "temperature: Extra inputs are not permitted".to_string()
            ))
        );
        assert_eq!(
            provider_error(br#"{"error":{"message":"model not found","code":null}}"#),
            Some((None, "model not found".to_string()))
        );
        assert_eq!(
            provider_error(br#"{"error":"model 'x' not found"}"#),
            Some((None, "model 'x' not found".to_string()))
        );
        // A body without an error message isn't shown.
        assert_eq!(provider_error(b"<html>Bad Gateway</html>"), None);
        assert_eq!(provider_error(br#"{"detail":"secret"}"#), None);
    }

    #[test]
    fn server_errors_and_429s_with_retry_after_are_retried() {
        for status in [500, 502, 529] {
            assert!(should_retry(status, &[]), "{status}");
        }
        assert!(should_retry(429, &headers(&[("retry-after", "2")])));
        // A 429 without `retry-after`, such as a reached spend cap.
        assert!(!should_retry(429, &[]));
        for status in [400, 401, 403, 404, 408, 409, 413] {
            assert!(!should_retry(status, &[]), "{status}");
        }
    }

    #[test]
    fn x_should_retry_replaces_the_status_result() {
        assert!(should_retry(400, &headers(&[("x-should-retry", "true")])));
        assert!(!should_retry(529, &headers(&[("X-Should-Retry", "false")])));
    }

    #[test]
    fn the_wait_is_retry_after_or_a_backoff() {
        assert_eq!(
            retry_delay(&headers(&[("retry-after", "3")]), 0),
            Duration::from_secs(3)
        );
        let backoffs: Vec<Duration> = (0..6).map(|retries| retry_delay(&[], retries)).collect();
        assert_eq!(
            backoffs,
            [500, 1000, 2000, 4000, 8000, 8000].map(Duration::from_millis)
        );
    }
}
