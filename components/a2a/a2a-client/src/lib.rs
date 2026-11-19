//! An A2A client. Implements `composable:a2a/client` with A2A's JSON-RPC
//! binding over HTTP. `connect` fetches an agent's card from its well-known
//! URI and selects the interface that every later request is sent to.

wit_bindgen::generate!({
    path: "wit",
    world: "a2a-client",
    generate_all,
});

mod http;
mod mapping;

use std::cell::RefCell;

use composable::a2a::types::{AgentCard, AgentInterface, StreamResponse};
use composable::agents::types::{Task, TaskState};
use exports::composable::a2a::client::{
    Agent, CancelTaskRequest, Error, GetTaskRequest, Guest, GuestAgent, ListTasksRequest,
    ListTasksResponse, MessageResponseStream, SendMessageRequest, SendMessageResponse,
};
use serde_json::{Value, json};
use wit_bindgen::rt::async_support::{StreamReader, StreamResult, StreamWriter, spawn_local};

use componentized::http::client::HttpResponse;

/// The path of an agent's card under its server's URL.
const WELL_KNOWN_CARD: &str = "/.well-known/agent-card.json";

/// The A2A version the client uses.
const A2A_VERSION: &str = "1.0";

const VERSION_HEADER: &str = "a2a-version";
const EXTENSIONS_HEADER: &str = "a2a-extensions";
const EVENT_STREAM: &str = "text/event-stream";

struct Component;

impl Guest for Component {
    type Agent = RemoteAgent;

    async fn connect(server_url: String, extensions: Vec<String>) -> Result<Agent, Error> {
        let url = format!("{}{WELL_KNOWN_CARD}", server_url.trim_end_matches('/'));
        let response = http::get(&url, vec![version_header()]).await?;
        if response.status >= 400 {
            return Err(format!(
                "the agent's server returned {} for its card",
                response.status
            )
            .into());
        }
        let card = mapping::agent_card(&http::read(response.body).await)?;
        let interface = mapping::select_interface(&card, A2A_VERSION)?;
        if let Some(uri) = mapping::unrequested_required_extension(&card, &extensions) {
            return Err(
                format!("the agent requires the extension {uri}, which was not requested").into(),
            );
        }
        Ok(Agent::new(RemoteAgent {
            card,
            interface,
            requested_extensions: extensions,
            activated_extensions: RefCell::new(Vec::new()),
        }))
    }
}

/// An agent on an A2A server.
struct RemoteAgent {
    card: AgentCard,
    // The interface that requests are sent to.
    interface: AgentInterface,
    // The extensions each request asks the agent to activate.
    requested_extensions: Vec<String>,
    // The extensions the agent's most recent response listed as activated.
    activated_extensions: RefCell<Vec<String>>,
}

impl GuestAgent for RemoteAgent {
    fn card(&self) -> AgentCard {
        self.card.clone()
    }

    fn interface(&self) -> AgentInterface {
        self.interface.clone()
    }

    fn activated_extensions(&self) -> Vec<String> {
        self.activated_extensions.borrow().clone()
    }

    async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, Error> {
        let params = mapping::send_message_params(&request, self.tenant())?;
        let result = self.call("SendMessage", params).await?;
        Ok(mapping::send_message_response(result)?)
    }

    async fn send_streaming_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<MessageResponseStream, Error> {
        let params = mapping::send_message_params(&request, self.tenant())?;
        let response = self
            .send("SendStreamingMessage", params, EVENT_STREAM)
            .await?;
        if is_event_stream(&response.headers) {
            let body = response.body;
            return Ok(response_stream(async move |responses| {
                relay(body, responses).await
            }));
        }
        // An agent may respond with one JSON-RPC response in place of a
        // stream. It has an error, or the one response.
        let result = mapping::rpc_result(json_body(response.body).await?)?;
        let response = mapping::stream_response(result)?;
        Ok(response_stream(async move |responses| {
            let _ = responses.write_one(response).await;
            Ok(())
        }))
    }

    async fn get_task(&self, request: GetTaskRequest) -> Result<Task, Error> {
        let params = mapping::get_task_params(&request, self.tenant())?;
        Ok(mapping::task(self.call("GetTask", params).await?)?)
    }

    async fn list_tasks(&self, request: ListTasksRequest) -> Result<ListTasksResponse, Error> {
        let params = mapping::list_tasks_params(&request, self.tenant())?;
        let result = self.call("ListTasks", params).await?;
        Ok(mapping::list_tasks_response(result)?)
    }

    async fn cancel_task(&self, request: CancelTaskRequest) -> Result<Task, Error> {
        let params = mapping::cancel_task_params(&request, self.tenant())?;
        Ok(mapping::task(self.call("CancelTask", params).await?)?)
    }
}

impl RemoteAgent {
    fn tenant(&self) -> Option<&str> {
        self.interface.tenant.as_deref()
    }

    // Calls `method` and returns its result.
    async fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        let response = self.send(method, params, "application/json").await?;
        mapping::rpc_result(json_body(response.body).await?)
    }

    // Sends a JSON-RPC request for `method`, accepting a response of the
    // `accept` media type, and returns the HTTP response. Keeps the extensions
    // the response lists as activated. An error status fails with the
    // JSON-RPC error in its body, if it has one.
    async fn send(&self, method: &str, params: Value, accept: &str) -> Result<HttpResponse, Error> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("accept".to_string(), accept.to_string()),
            version_header(),
        ];
        if !self.requested_extensions.is_empty() {
            headers.push((
                EXTENSIONS_HEADER.to_string(),
                self.requested_extensions.join(","),
            ));
        }
        let response =
            http::post(&self.interface.url, headers, body.to_string().into_bytes()).await?;
        *self.activated_extensions.borrow_mut() = extensions_listed(&response.headers);
        if response.status >= 400 {
            let status = response.status;
            let error = match json_body(response.body).await.map(mapping::rpc_result) {
                Ok(Err(error)) => error,
                _ => format!("the agent's server returned {status}").into(),
            };
            return Err(error);
        }
        Ok(response)
    }
}

// A stream of the responses that `write` writes, with a result that resolves
// to the value `write` returns. The responses end before the result resolves.
fn response_stream(
    write: impl AsyncFnOnce(&mut StreamWriter<StreamResponse>) -> Result<(), Error> + 'static,
) -> MessageResponseStream {
    let (mut responses_tx, responses) = wit_stream::new::<StreamResponse>();
    let (result_tx, result) =
        wit_future::new::<Result<(), Error>>(|| Err("the response was dropped".to_string().into()));
    spawn_local(async move {
        let ended = write(&mut responses_tx).await;
        drop(responses_tx);
        let _ = result_tx.write(ended).await;
    });
    MessageResponseStream { responses, result }
}

// A failure other than a JSON-RPC error from the agent, which has only a
// message.
impl From<String> for Error {
    fn from(message: String) -> Self {
        Error {
            message,
            code: None,
            data: None,
        }
    }
}

// Relays each event of the SSE `body` as a response, until the body ends or
// the caller stops reading the responses. Fails on an event that isn't a
// JSON-RPC response with a `StreamResponse` result, on a JSON-RPC error, and
// when the body ends before a message arrives or the task reaches a terminal
// or interrupted state.
async fn relay(
    mut body: StreamReader<u8>,
    responses: &mut StreamWriter<StreamResponse>,
) -> Result<(), Error> {
    let mut ended = false;
    let mut events = sse::Events::new(async move || {
        if ended {
            return None;
        }
        let (result, bytes) = body.read(Vec::with_capacity(8192)).await;
        ended = matches!(result, StreamResult::Dropped);
        Some(bytes)
    });
    let mut complete = false;
    while let Some(event) = events.next().await {
        let response: Value = serde_json::from_str(&event.data)
            .map_err(|e| format!("the agent sent an event that is not valid JSON: {e}"))?;
        let response = mapping::stream_response(mapping::rpc_result(response)?)?;
        complete = is_last(&response);
        if responses.write_one(response).await.is_some() {
            return Ok(());
        }
    }
    if complete {
        Ok(())
    } else {
        Err(
            "the agent's responses ended before its task reached a terminal or interrupted state"
                .to_string()
                .into(),
        )
    }
}

// Whether a stream may end after `response`. Returns `true` for any message,
// or a task that is in a terminal or interrupted state.
fn is_last(response: &StreamResponse) -> bool {
    let state = match response {
        StreamResponse::Message(_) => return true,
        StreamResponse::Task(task) => task.status.state,
        StreamResponse::StatusUpdate(update) => update.status.state,
        StreamResponse::ArtifactUpdate(_) => return false,
    };
    matches!(
        state,
        TaskState::Completed
            | TaskState::Failed
            | TaskState::Canceled
            | TaskState::Rejected
            | TaskState::InputRequired
            | TaskState::AuthRequired
    )
}

// The JSON in `body`.
async fn json_body(body: StreamReader<u8>) -> Result<Value, String> {
    serde_json::from_slice(&http::read(body).await)
        .map_err(|e| format!("the agent's response is not valid JSON: {e}"))
}

fn version_header() -> (String, String) {
    (VERSION_HEADER.to_string(), A2A_VERSION.to_string())
}

// The extensions a response's `A2A-Extensions` header lists.
fn extensions_listed(headers: &[(String, String)]) -> Vec<String> {
    http::header(headers, EXTENSIONS_HEADER)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|uri| !uri.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// Whether a response's content type is an SSE event stream.
fn is_event_stream(headers: &[(String, String)]) -> bool {
    http::header(headers, "content-type")
        .is_some_and(|value| value.trim_start().starts_with(EVENT_STREAM))
}

export!(Component);

#[cfg(test)]
mod tests {
    use super::*;
    use composable::agents::types::{
        Artifact, TaskArtifactUpdateEvent, TaskStatus, TaskStatusUpdateEvent,
    };

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn an_event_stream_is_recognized_by_its_content_type() {
        assert!(is_event_stream(&headers(&[(
            "Content-Type",
            "text/event-stream; charset=utf-8"
        )])));
        assert!(!is_event_stream(&headers(&[(
            "content-type",
            "application/json"
        )])));
        assert!(!is_event_stream(&headers(&[])));
    }

    #[test]
    fn the_activated_extensions_are_the_uris_in_the_header() {
        assert_eq!(
            extensions_listed(&headers(&[(
                "A2A-Extensions",
                "https://example.com/ext/a, https://example.com/ext/b,"
            )])),
            vec![
                "https://example.com/ext/a".to_string(),
                "https://example.com/ext/b".to_string()
            ]
        );
        assert!(extensions_listed(&headers(&[("content-type", "application/json")])).is_empty());
    }

    fn status_update(state: TaskState) -> StreamResponse {
        StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: "ctx-1".to_string(),
            status: TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            metadata: None,
        })
    }

    #[test]
    fn a_stream_may_end_after_a_terminal_or_interrupted_state() {
        for state in [
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::Rejected,
            TaskState::InputRequired,
            TaskState::AuthRequired,
        ] {
            assert!(is_last(&status_update(state)), "{state:?}");
        }
        for state in [
            TaskState::Submitted,
            TaskState::Working,
            TaskState::Unspecified,
        ] {
            assert!(!is_last(&status_update(state)), "{state:?}");
        }
        assert!(!is_last(&StreamResponse::ArtifactUpdate(
            TaskArtifactUpdateEvent {
                task_id: "task-1".to_string(),
                context_id: "ctx-1".to_string(),
                artifact: Artifact {
                    artifact_id: "a-1".to_string(),
                    name: None,
                    description: None,
                    parts: Vec::new(),
                    metadata: None,
                    extensions: Vec::new(),
                },
                append: false,
                last_chunk: true,
                metadata: None,
            }
        )));
    }
}
