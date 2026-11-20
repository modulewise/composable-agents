//! The server over HTTP, with its agent card and the JSON-RPC binding, run
//! against the test agent in `tests/test-agent`.

mod common;

use a2a_json::AgentCard;
use composable_a2a_server::{A2aServer, AGENT_CARD_PATH};
use composable_runtime::Runtime;
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::watch;

// A server for the test agent on a local port.
struct Server {
    url: String,
    client: reqwest::Client,
    // Dropping the sender shuts the server down.
    _shutdown: watch::Sender<bool>,
    _runtime: Runtime,
}

impl Server {
    async fn start() -> Self {
        let (handler, runtime) = common::test_agent_handler().await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = A2aServer::new(handler, &url).unwrap();
        let (shutdown, shutdown_rx) = watch::channel(false);
        tokio::spawn(server.run(listener, shutdown_rx));
        Server {
            url,
            client: reqwest::Client::new(),
            _shutdown: shutdown,
            _runtime: runtime,
        }
    }

    // Posts `body` as a request of A2A version 1.0.
    async fn post(&self, body: &Value) -> Response {
        self.client
            .post(&self.url)
            .header("A2A-Version", "1.0")
            .json(body)
            .send()
            .await
            .unwrap()
    }

    // Calls `method` and returns the JSON-RPC response.
    async fn call(&self, method: &str, params: Value) -> Value {
        let response = self.post(&request(method, params)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/json");
        response.json().await.unwrap()
    }
}

fn request(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
}

// The params of a send operation, for a message whose id is `id`. The test
// agent makes the ids `task-<id>` and `context-<id>`.
fn message(id: &str, text: &str) -> Value {
    json!({ "message": {
        "messageId": id,
        "role": "ROLE_USER",
        "parts": [{ "text": text }],
    } })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_card_is_at_the_well_known_path_and_can_be_cached() {
    let server = Server::start().await;
    let url = format!("{}{AGENT_CARD_PATH}", server.url);
    let response = server.client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "max-age=300");
    let etag = response.headers()["etag"].clone();

    let card: AgentCard = response.json().await.unwrap();
    assert_eq!(card.name, "test-agent");
    assert_eq!(card.supported_interfaces[0].url, server.url);
    assert_eq!(card.supported_interfaces[0].protocol_binding, "JSONRPC");
    assert_eq!(card.supported_interfaces[0].protocol_version, "1.0");
    assert_eq!(card.capabilities.streaming, Some(true));
    assert_eq!(card.capabilities.push_notifications, Some(false));
    assert_eq!(card.capabilities.extended_agent_card, Some(false));

    let response = server
        .client
        .get(&url)
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()["etag"], etag);
    assert!(response.bytes().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_has_a_response_with_the_request_id_and_the_task() {
    let server = Server::start().await;
    let response = server.call("SendMessage", message("m1", "hello")).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 7);
    let task = &response["result"]["task"];
    assert_eq!(task["id"], "task-m1");
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");

    let response = server.call("GetTask", json!({ "id": "task-m1" })).await;
    assert_eq!(response["result"]["id"], "task-m1");
    let response = server.call("ListTasks", Value::Null).await;
    assert_eq!(response["result"]["totalSize"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_has_status_200_and_its_code_in_the_response() {
    let server = Server::start().await;
    for (method, params, code) in [
        ("NoSuchMethod", json!({}), -32601),
        ("SendMessage", json!({}), -32602),
        (
            "SendMessage",
            json!({ "message": { "messageId": "m1", "role": "ROLE_USER", "parts": [] } }),
            -32602,
        ),
        (
            "ListTasks",
            json!({ "status": "TASK_STATE_RUNNING" }),
            -32602,
        ),
        ("GetTask", json!({ "id": "task-none" }), -32001),
        ("SubscribeToTask", json!({ "id": "task-none" }), -32001),
        ("SendStreamingMessage", message("m2", "refuse"), -32005),
    ] {
        let response = server.call(method, params).await;
        assert_eq!(response["error"]["code"], code, "{method}: {response}");
        assert_eq!(response["id"], 7);
        assert!(response.get("result").is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_operation_that_needs_an_undeclared_capability_has_its_error() {
    let server = Server::start().await;
    // The capability is checked before the params are read.
    for (method, code) in [
        ("CreateTaskPushNotificationConfig", -32003),
        ("GetTaskPushNotificationConfig", -32003),
        ("ListTaskPushNotificationConfigs", -32003),
        ("DeleteTaskPushNotificationConfig", -32003),
        ("GetExtendedAgentCard", -32004),
    ] {
        let response = server.call(method, json!({})).await;
        assert_eq!(response["error"]["code"], code, "{method}: {response}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_is_not_a_json_rpc_request_has_an_error_with_a_null_id() {
    let server = Server::start().await;
    let response = server
        .client
        .post(&server.url)
        .header("A2A-Version", "1.0")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: Value = response.json().await.unwrap();
    assert_eq!(response["error"]["code"], -32700);
    assert_eq!(response["id"], Value::Null);

    for body in [
        json!({ "jsonrpc": "1.0", "id": 7, "method": "GetTask" }),
        json!({ "jsonrpc": "2.0", "id": {}, "method": "GetTask" }),
        json!({ "jsonrpc": "2.0", "id": 7 }),
        json!([request("GetTask", json!({ "id": "task-none" }))]),
    ] {
        let response: Value = server.post(&body).await.json().await.unwrap();
        assert_eq!(response["error"]["code"], -32600, "{body}");
        assert_eq!(response["id"], Value::Null);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notification_has_no_response() {
    let server = Server::start().await;
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "SendMessage",
        "params": message("m1", "hello"),
    });
    let response = server.post(&notification).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.bytes().await.unwrap().is_empty());

    // The notification was not run.
    let response = server.call("ListTasks", Value::Null).await;
    assert_eq!(response["result"]["totalSize"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_for_another_version_is_version_not_supported() {
    let server = Server::start().await;
    let body = request("ListTasks", json!({}));
    let post = |version: Option<&'static str>, query: &'static str| {
        let mut request = server.client.post(format!("{}{query}", server.url));
        if let Some(version) = version {
            request = request.header("A2A-Version", version);
        }
        let request = request.json(&body);
        async move { request.send().await.unwrap().json::<Value>().await.unwrap() }
    };

    // A request that names no version has version 0.3.
    for (version, query) in [(None, ""), (Some("0.3"), ""), (Some("1.1"), "")] {
        let response = post(version, query).await;
        assert_eq!(response["error"]["code"], -32009, "{version:?}");
        assert_eq!(response["id"], 7);
    }
    for (version, query) in [
        (Some("1.0"), ""),
        (Some("1.0.2"), ""),
        (None, "/?A2A-Version=1.0"),
    ] {
        let response = post(version, query).await;
        assert!(response.get("result").is_some(), "{version:?} {query}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_activated_extensions_are_in_the_extensions_header_of_the_response() {
    let server = Server::start().await;
    // The test agent activates every extension a request asks for.
    let response = server
        .client
        .post(&server.url)
        .header("A2A-Version", "1.0")
        .header(
            "A2A-Extensions",
            "https://example.com/a, https://example.com/b",
        )
        .json(&request("SendMessage", message("m1", "hello")))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["a2a-extensions"],
        "https://example.com/a, https://example.com/b"
    );

    let response = server
        .post(&request("SendMessage", message("m2", "hello")))
        .await;
    assert!(response.headers().get("a2a-extensions").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_message_has_an_event_for_each_response() {
    let server = Server::start().await;
    let response = server
        .post(&request("SendStreamingMessage", message("m1", "chunks")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");

    let body = response.bytes().await.unwrap();
    let events = sse::Parser::default().feed(&body);
    // Each event is a JSON-RPC response with the request's id. The result is
    // a stream response, which has one field for its kind.
    let kinds: Vec<String> = events
        .iter()
        .map(|event| {
            let response: Value = serde_json::from_str(&event.data).unwrap();
            assert_eq!(response["jsonrpc"], "2.0");
            assert_eq!(response["id"], 7);
            let result = response["result"].as_object().unwrap();
            assert_eq!(result.len(), 1);
            result.keys().next().unwrap().clone()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "task",
            "statusUpdate",
            "artifactUpdate",
            "artifactUpdate",
            "statusUpdate"
        ]
    );
    let last: Value = serde_json::from_str(&events[4].data).unwrap();
    assert_eq!(
        last["result"]["statusUpdate"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_has_an_event_stream_that_begins_with_the_task() {
    let server = Server::start().await;
    server.call("SendMessage", message("m1", "ask")).await;
    let response = server
        .post(&request("SubscribeToTask", json!({ "id": "task-m1" })))
        .await;
    assert_eq!(response.headers()["content-type"], "text/event-stream");

    let body = response.bytes().await.unwrap();
    let events = sse::Parser::default().feed(&body);
    assert_eq!(events.len(), 1);
    let response: Value = serde_json::from_str(&events[0].data).unwrap();
    assert_eq!(
        response["result"]["task"]["status"]["state"],
        "TASK_STATE_INPUT_REQUIRED"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_is_not_a_post_is_method_not_allowed() {
    let server = Server::start().await;
    let response = server.client.get(&server.url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "POST");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_over_the_size_limit_is_payload_too_large() {
    let server = Server::start().await;
    let body = vec![b' '; 4 * 1024 * 1024 + 1];
    let response = server
        .client
        .post(&server.url)
        .header("A2A-Version", "1.0")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
