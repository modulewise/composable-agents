//! The client's HTTP requests. No redirect is followed.

use wit_bindgen::rt::async_support::{StreamReader, StreamResult, spawn_local};

use crate::componentized::http::client::{self, ErrorCode, HttpResponse, RequestOptions};
use crate::wit_stream;

const NO_REDIRECTS: RequestOptions = RequestOptions {
    connect_timeout_ms: None,
    first_byte_timeout_ms: None,
    between_bytes_timeout_ms: None,
    max_redirects: Some(0),
};

/// Sends a GET request. Fails when no response arrives or the response is a
/// redirect.
pub(crate) async fn get(url: &str, headers: Vec<(String, String)>) -> Result<HttpResponse, String> {
    received(client::get(url.to_string(), headers, Some(NO_REDIRECTS)).await)
}

/// Sends a POST request with `body`. Fails when no response arrives or the
/// response is a redirect.
pub(crate) async fn post(
    url: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<HttpResponse, String> {
    let (mut body_tx, body_rx) = wit_stream::new::<u8>();
    spawn_local(async move {
        let _ = body_tx.write_all(body).await;
    });
    received(client::post(url.to_string(), headers, body_rx, Some(NO_REDIRECTS)).await)
}

// The response, unless it's a redirect.
fn received(sent: Result<HttpResponse, ErrorCode>) -> Result<HttpResponse, String> {
    match sent {
        Ok(response) if (300..400).contains(&response.status) => Err(redirected(response.status)),
        Ok(response) => Ok(response),
        Err(
            ErrorCode::RedirectLimitExceeded((response, _))
            | ErrorCode::RedirectRequiresBody((response, _)),
        ) => Err(redirected(response.status)),
        Err(ErrorCode::Other(reason)) => {
            Err(reason.unwrap_or_else(|| "HTTP request failed (no reason given)".to_string()))
        }
    }
}

fn redirected(status: u16) -> String {
    format!("the agent's server redirected the request (status {status})")
}

/// The value of the first header named `name`, ignoring case.
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Reads `body` to its end.
pub(crate) async fn read(mut body: StreamReader<u8>) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let (result, chunk) = body.read(Vec::with_capacity(8192)).await;
        bytes.extend_from_slice(&chunk);
        if matches!(result, StreamResult::Dropped) {
            return bytes;
        }
    }
}
