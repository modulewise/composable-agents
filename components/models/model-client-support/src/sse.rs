//! The server-sent events of a response body.

use wit_bindgen::rt::async_support::{StreamReader, StreamResult};

pub use ::sse::{Event, Events, Parser};

/// The events of `body`, read as they arrive. Dropping them drops the body,
/// which closes the connection.
pub fn events(mut body: StreamReader<u8>) -> Events<impl AsyncFnMut() -> Option<Vec<u8>>> {
    let mut ended = false;
    Events::new(async move || {
        if ended {
            return None;
        }
        let (result, bytes) = body.read(Vec::with_capacity(8192)).await;
        ended = matches!(result, StreamResult::Dropped);
        Some(bytes)
    })
}
