//! The request handler's operations, run against the test agent in
//! `tests/test-agent`.

mod common;

use std::time::Duration;

use a2a_json::jsonrpc::{Error, code};
use a2a_json::{
    CancelTaskRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, Message, PartContent,
    SendMessageConfiguration, SendMessageRequest, SendMessageResponse, StreamResponse,
    SubscribeToTaskRequest, Task, TaskState,
};
use composable_a2a_server::CallContext;
use composable_runtime::Runtime;
use serde_json::json;
use tokio::sync::mpsc;

// A request handler for the test agent, with the runtime that hosts the
// agent.
struct Server {
    handler: common::Handler,
    _runtime: Runtime,
}

impl Server {
    async fn start() -> Self {
        let (handler, runtime) = common::test_agent_handler().await;
        Server {
            handler,
            _runtime: runtime,
        }
    }

    async fn send_message(&self, request: SendMessageRequest) -> Result<Task, Error> {
        let response = self
            .handler
            .send_message(request, &mut CallContext::default())
            .await?;
        match response {
            SendMessageResponse::Task(task) => Ok(task),
            SendMessageResponse::Message(_) => panic!("expected a task"),
        }
    }

    // Sends a message and returns its task.
    async fn send(&self, message: Message) -> Task {
        self.send_message(request(message)).await.unwrap()
    }

    async fn get_task(&self, id: &str, history_length: Option<i32>) -> Result<Task, Error> {
        let request = GetTaskRequest {
            tenant: None,
            id: id.to_string(),
            history_length,
        };
        self.handler
            .get_task(request, &CallContext::default())
            .await
    }

    async fn cancel_task(&self, id: &str) -> Result<Task, Error> {
        let request = CancelTaskRequest {
            tenant: None,
            id: id.to_string(),
            metadata: None,
        };
        self.handler
            .cancel_task(request, &CallContext::default())
            .await
    }

    async fn list_tasks(&self, request: ListTasksRequest) -> Result<ListTasksResponse, Error> {
        self.handler
            .list_tasks(request, &CallContext::default())
            .await
    }
}

// A message whose id is `id`. The test agent makes the ids `task-<id>` and
// `context-<id>` for a message that has none.
fn message(id: &str, text: &str) -> Message {
    serde_json::from_value(json!({
        "messageId": id,
        "role": "ROLE_USER",
        "parts": [{ "text": text }],
    }))
    .unwrap()
}

// `message` for the task with `task_id`.
fn for_task(mut message: Message, task_id: &str) -> Message {
    message.task_id = Some(task_id.to_string());
    message
}

// `message` in the context with `context_id`.
fn in_context(mut message: Message, context_id: &str) -> Message {
    message.context_id = Some(context_id.to_string());
    message
}

fn request(message: Message) -> SendMessageRequest {
    SendMessageRequest {
        tenant: None,
        message,
        configuration: None,
        metadata: None,
    }
}

fn configured(message: Message, configuration: SendMessageConfiguration) -> SendMessageRequest {
    SendMessageRequest {
        configuration: Some(configuration),
        ..request(message)
    }
}

fn return_immediately() -> SendMessageConfiguration {
    SendMessageConfiguration {
        return_immediately: true,
        ..SendMessageConfiguration::default()
    }
}

// The text of a message's or an artifact's first part.
fn text(parts: &[a2a_json::Part]) -> &str {
    match &parts[0].content {
        PartContent::Text(text) => text,
        _ => panic!("expected text"),
    }
}

fn status_text(task: &Task) -> &str {
    text(&task.status.message.as_ref().unwrap().parts)
}

fn ids(tasks: &[Task]) -> Vec<&str> {
    tasks.iter().map(|task| task.id.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_runs_a_task_to_a_terminal_state() {
    let server = Server::start().await;
    let task = server.send(message("m1", "hello")).await;

    assert_eq!(task.id, "task-m1");
    assert_eq!(task.context_id.as_deref(), Some("context-m1"));
    assert_eq!(task.status.state, TaskState::Completed);
    assert!(task.status.timestamp.is_some());
    assert_eq!(task.artifacts.len(), 1);
    assert_eq!(task.artifacts[0].artifact_id, "reply");
    assert_eq!(text(&task.artifacts[0].parts), "hello (history: 0)");
    assert_eq!(task.history, [message("m1", "hello")]);

    assert_eq!(server.get_task("task-m1", None).await, Ok(task));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_in_a_context_has_the_exchanges_of_the_tasks_before_it() {
    let server = Server::start().await;
    server.send(message("m1", "one")).await;
    let task = server
        .send(in_context(message("m2", "two"), "context-m1"))
        .await;

    assert_eq!(task.id, "task-m2");
    assert_eq!(task.context_id.as_deref(), Some("context-m1"));
    assert_eq!(text(&task.artifacts[0].parts), "two (history: 1)");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_that_names_an_unknown_task_is_task_not_found() {
    let server = Server::start().await;
    let named = for_task(message("m1", "hello"), "task-none");
    let error = server.send_message(request(named)).await.unwrap_err();
    assert_eq!(error.code, code::TASK_NOT_FOUND);
    assert_eq!(
        error.data,
        Some(json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "TASK_NOT_FOUND",
            "domain": "a2a-protocol.org",
        }]))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_message_of_a_working_status_is_not_added_to_the_history() {
    let server = Server::start().await;
    let task = server.send(message("m1", "think")).await;
    assert_eq!(task.status.state, TaskState::Completed);
    assert_eq!(task.history, [message("m1", "think")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_continues_the_interrupted_task_it_names() {
    let server = Server::start().await;
    let task = server.send(message("m1", "ask")).await;
    assert_eq!(task.status.state, TaskState::InputRequired);
    assert_eq!(status_text(&task), "Which one?");

    // A context id that is not the task's is rejected.
    let other_context = in_context(for_task(message("m2", "blue"), "task-m1"), "context-other");
    let error = server
        .send_message(request(other_context))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::INVALID_PARAMS);

    let reply = for_task(message("m3", "blue"), "task-m1");
    let task = server.send(reply.clone()).await;
    assert_eq!(task.id, "task-m1");
    assert_eq!(task.context_id.as_deref(), Some("context-m1"));
    assert_eq!(task.status.state, TaskState::Completed);
    assert_eq!(text(&task.artifacts[0].parts), "continued: blue");
    // The history has both of the client's messages, with the agent's
    // question between them.
    assert_eq!(task.history.len(), 3);
    assert_eq!(task.history[0], message("m1", "ask"));
    assert_eq!(text(&task.history[1].parts), "Which one?");
    assert_eq!(task.history[2], reply);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_is_not_interrupted_accepts_no_messages() {
    let server = Server::start().await;
    server.send(message("m1", "hello")).await;
    let to_terminal = for_task(message("m2", "more"), "task-m1");
    let error = server.send_message(request(to_terminal)).await.unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED_OPERATION);

    let waiting = configured(message("m3", "wait"), return_immediately());
    server.send_message(waiting).await.unwrap();
    let to_working = for_task(message("m4", "more"), "task-m3");
    let error = server.send_message(request(to_working)).await.unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED_OPERATION);
    server.cancel_task("task-m3").await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_has_no_more_history_than_the_request_allows() {
    let server = Server::start().await;
    let configuration = SendMessageConfiguration {
        history_length: Some(1),
        ..SendMessageConfiguration::default()
    };
    let task = server
        .send_message(configured(message("m1", "ask"), configuration))
        .await
        .unwrap();
    // The most recent message is the agent's question.
    assert_eq!(task.history.len(), 1);
    assert_eq!(text(&task.history[0].parts), "Which one?");

    let stored = server.get_task("task-m1", Some(0)).await.unwrap();
    assert!(stored.history.is_empty());
    let stored = server.get_task("task-m1", None).await.unwrap();
    assert_eq!(stored.history.len(), 2);

    let error = server.get_task("task-m1", Some(-1)).await.unwrap_err();
    assert_eq!(error.code, code::INVALID_PARAMS);
    let error = server.get_task("task-none", None).await.unwrap_err();
    assert_eq!(error.code, code::TASK_NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_returned_immediately_is_still_running_and_can_be_canceled() {
    let server = Server::start().await;
    let task = server
        .send_message(configured(message("m1", "wait"), return_immediately()))
        .await
        .unwrap();
    assert!(
        [TaskState::Submitted, TaskState::Working].contains(&task.status.state),
        "{:?}",
        task.status.state
    );

    let canceled = server.cancel_task("task-m1").await.unwrap();
    assert_eq!(canceled.status.state, TaskState::Canceled);

    let error = server.cancel_task("task-m1").await.unwrap_err();
    assert_eq!(error.code, code::TASK_NOT_CANCELABLE);
    let error = server.cancel_task("task-none").await.unwrap_err();
    assert_eq!(error.code, code::TASK_NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_task_can_be_canceled() {
    let server = Server::start().await;
    server.send(message("m1", "ask")).await;
    let canceled = server.cancel_task("task-m1").await.unwrap();
    assert_eq!(canceled.status.state, TaskState::Canceled);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_from_the_agent_is_the_error_of_the_request_and_no_task_is_stored() {
    let server = Server::start().await;
    let error = server
        .send_message(request(message("m1", "refuse")))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::CONTENT_TYPE_NOT_SUPPORTED);
    assert_eq!(error.message, "the test agent refused the message");
    assert_eq!(
        error.data,
        Some(json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo" }]))
    );

    let listed = server
        .list_tasks(ListTasksRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.total_size, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_fails_has_the_reason_as_its_status_message() {
    let server = Server::start().await;
    let task = server.send(message("m1", "fail")).await;
    assert_eq!(task.status.state, TaskState::Failed);
    assert_eq!(status_text(&task), "the test agent failed");

    // An artifact with no parts is not valid in A2A, so the task fails.
    let task = server.send(message("m2", "no parts")).await;
    assert_eq!(task.status.state, TaskState::Failed);
    assert_eq!(
        status_text(&task),
        "the agent sent an artifact with no parts"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_extensions_the_agent_activated_are_set_on_the_call_context() {
    let server = Server::start().await;
    // The test agent activates every extension a request asks for.
    let requested = vec!["https://example.com/ext/a".to_string()];
    let mut context = CallContext {
        requested_extensions: requested.clone(),
        activated_extensions: Vec::new(),
    };
    server
        .handler
        .send_message(request(message("m1", "hello")), &mut context)
        .await
        .unwrap();
    assert_eq!(context.activated_extensions, requested);

    let mut context = CallContext {
        requested_extensions: requested.clone(),
        activated_extensions: Vec::new(),
    };
    server
        .handler
        .send_streaming_message(request(message("m2", "hello")), &mut context)
        .await
        .unwrap();
    assert_eq!(context.activated_extensions, requested);
}

// Each response of a stream until it ends, described in a few words.
async fn responses(mut stream: mpsc::Receiver<StreamResponse>) -> Vec<String> {
    let mut described = Vec::new();
    while let Some(response) = stream.recv().await {
        described.push(match response {
            StreamResponse::Task(task) => format!("task {:?}", task.status.state),
            StreamResponse::Message(_) => "message".to_string(),
            StreamResponse::StatusUpdate(update) => format!("status {:?}", update.status.state),
            StreamResponse::ArtifactUpdate(update) => format!(
                "artifact {:?} append={} last={}",
                text(&update.artifact.parts),
                update.append,
                update.last_chunk
            ),
        });
    }
    described
}

impl Server {
    async fn stream(
        &self,
        request: SendMessageRequest,
    ) -> Result<mpsc::Receiver<StreamResponse>, Error> {
        self.handler
            .send_streaming_message(request, &mut CallContext::default())
            .await
    }

    async fn subscribe(&self, id: &str) -> Result<mpsc::Receiver<StreamResponse>, Error> {
        let request = SubscribeToTaskRequest {
            tenant: None,
            id: id.to_string(),
        };
        self.handler
            .subscribe_to_task(request, &CallContext::default())
            .await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_message_has_the_task_then_each_update_then_the_final_status() {
    let server = Server::start().await;
    let stream = server.stream(request(message("m1", "chunks"))).await;
    assert_eq!(
        responses(stream.unwrap()).await,
        [
            "task Submitted",
            "status Working",
            "artifact \"one \" append=false last=false",
            "artifact \"two\" append=true last=true",
            "status Completed",
        ]
    );

    // The stored task has the artifact as the agent assembled it.
    let task = server.get_task("task-m1", None).await.unwrap();
    assert_eq!(task.status.state, TaskState::Completed);
    assert_eq!(task.artifacts.len(), 1);
    assert_eq!(task.artifacts[0].parts.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_task_of_a_stream_has_no_more_history_than_the_request_allows() {
    let server = Server::start().await;
    let configuration = SendMessageConfiguration {
        history_length: Some(0),
        ..SendMessageConfiguration::default()
    };
    let mut stream = server
        .stream(configured(message("m1", "hello"), configuration))
        .await
        .unwrap();
    let Some(StreamResponse::Task(task)) = stream.recv().await else {
        panic!("expected the task first");
    };
    assert!(task.history.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_message_the_agent_refuses_has_an_error_and_no_stream() {
    let server = Server::start().await;
    let error = server
        .stream(request(message("m1", "refuse")))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::CONTENT_TYPE_NOT_SUPPORTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn each_subscriber_of_a_running_task_has_the_task_then_its_later_updates() {
    let server = Server::start().await;
    let first = server.stream(request(message("m1", "wait"))).await.unwrap();
    let second = server.subscribe("task-m1").await.unwrap();
    server.cancel_task("task-m1").await.unwrap();

    assert_eq!(
        responses(first).await,
        ["task Submitted", "status Working", "status Canceled"]
    );
    // The second stream begins with the task as it was when it subscribed,
    // so the `working` status is in that task or is its own response.
    let second = responses(second).await;
    assert!(
        second == ["task Working", "status Canceled"]
            || second == ["task Submitted", "status Working", "status Canceled"],
        "{second:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_to_an_interrupted_task_has_the_task_and_ends() {
    let server = Server::start().await;
    server.send(message("m1", "ask")).await;
    let stream = server.subscribe("task-m1").await.unwrap();
    assert_eq!(responses(stream).await, ["task InputRequired"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_to_an_unknown_or_a_terminal_task_is_an_error() {
    let server = Server::start().await;
    let error = server.subscribe("task-none").await.unwrap_err();
    assert_eq!(error.code, code::TASK_NOT_FOUND);

    server.send(message("m1", "hello")).await;
    let error = server.subscribe("task-m1").await.unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED_OPERATION);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_for_another_task_fails_the_task() {
    let server = Server::start().await;
    let stream = server.stream(request(message("m1", "other task"))).await;
    // The agent's own final status is not sent after the failure.
    assert_eq!(
        responses(stream.unwrap()).await,
        ["task Submitted", "status Working", "status Failed"]
    );
    let task = server.get_task("task-m1", None).await.unwrap();
    assert_eq!(task.status.state, TaskState::Failed);
    assert_eq!(
        status_text(&task),
        "the agent sent an event for another task"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_runs_to_its_end_after_its_stream_is_closed() {
    let server = Server::start().await;
    let stream = server.stream(request(message("m1", "chunks"))).await;
    drop(stream.unwrap());

    let mut state = TaskState::Submitted;
    for _ in 0..200 {
        state = server.get_task("task-m1", None).await.unwrap().status.state;
        if state == TaskState::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(state, TaskState::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn each_operation_that_is_not_supported_has_its_error() {
    let server = Server::start().await;
    let context = CallContext::default();
    let handler = &server.handler;
    let config: a2a_json::TaskPushNotificationConfig =
        serde_json::from_value(json!({ "taskId": "t1", "url": "https://example.com" })).unwrap();
    let named = |id: &str| json!({ "taskId": "t1", "id": id });

    let errors = [
        handler
            .create_task_push_notification_config(config.clone(), &context)
            .await
            .unwrap_err(),
        handler
            .get_task_push_notification_config(
                serde_json::from_value(named("c1")).unwrap(),
                &context,
            )
            .await
            .unwrap_err(),
        handler
            .list_task_push_notification_configs(
                serde_json::from_value(json!({ "taskId": "t1" })).unwrap(),
                &context,
            )
            .await
            .unwrap_err(),
        handler
            .delete_task_push_notification_config(
                serde_json::from_value(named("c1")).unwrap(),
                &context,
            )
            .await
            .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.code, code::PUSH_NOTIFICATION_NOT_SUPPORTED);
    }

    // A message with a push notification config is also rejected.
    let configuration = SendMessageConfiguration {
        task_push_notification_config: Some(config),
        ..SendMessageConfiguration::default()
    };
    let error = server
        .send_message(configured(message("m1", "hello"), configuration))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::PUSH_NOTIFICATION_NOT_SUPPORTED);

    let error = handler
        .get_extended_agent_card(a2a_json::GetExtendedAgentCardRequest::default(), &context)
        .await
        .unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED_OPERATION);
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_are_listed_by_status_time_in_pages_and_without_artifacts() {
    let server = Server::start().await;
    for (id, text) in [("m1", "one"), ("m2", "ask"), ("m3", "three")] {
        server.send(message(id, text)).await;
        // Each task's status has a later time than the one before.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let list = async |request: ListTasksRequest| server.list_tasks(request).await.unwrap();

    let listed = list(ListTasksRequest::default()).await;
    assert_eq!(ids(&listed.tasks), ["task-m3", "task-m2", "task-m1"]);
    assert_eq!(listed.total_size, 3);
    assert_eq!(listed.page_size, 50);
    assert_eq!(listed.next_page_token, "");
    // Artifacts are left out unless the request asks for them.
    assert!(listed.tasks[0].artifacts.is_empty());
    let listed = list(ListTasksRequest {
        include_artifacts: Some(true),
        history_length: Some(0),
        ..ListTasksRequest::default()
    })
    .await;
    assert_eq!(
        text(&listed.tasks[0].artifacts[0].parts),
        "three (history: 0)"
    );
    assert!(listed.tasks[0].history.is_empty());

    let first = list(ListTasksRequest {
        page_size: Some(2),
        ..ListTasksRequest::default()
    })
    .await;
    assert_eq!(ids(&first.tasks), ["task-m3", "task-m2"]);
    assert_eq!(first.total_size, 3);
    let second = list(ListTasksRequest {
        page_size: Some(2),
        page_token: Some(first.next_page_token),
        ..ListTasksRequest::default()
    })
    .await;
    assert_eq!(ids(&second.tasks), ["task-m1"]);
    assert_eq!(second.next_page_token, "");

    let interrupted = list(ListTasksRequest {
        status: Some(TaskState::InputRequired),
        ..ListTasksRequest::default()
    })
    .await;
    assert_eq!(ids(&interrupted.tasks), ["task-m2"]);
    let in_context = list(ListTasksRequest {
        context_id: Some("context-m1".to_string()),
        ..ListTasksRequest::default()
    })
    .await;
    assert_eq!(ids(&in_context.tasks), ["task-m1"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_requests_that_are_not_valid_are_invalid_params() {
    let server = Server::start().await;
    for request in [
        ListTasksRequest {
            page_size: Some(0),
            ..ListTasksRequest::default()
        },
        ListTasksRequest {
            page_size: Some(101),
            ..ListTasksRequest::default()
        },
        ListTasksRequest {
            page_token: Some("not a token".to_string()),
            ..ListTasksRequest::default()
        },
        ListTasksRequest {
            status_timestamp_after: Some("yesterday".to_string()),
            ..ListTasksRequest::default()
        },
        ListTasksRequest {
            history_length: Some(-1),
            ..ListTasksRequest::default()
        },
    ] {
        let error = server.list_tasks(request.clone()).await.unwrap_err();
        assert_eq!(error.code, code::INVALID_PARAMS, "{request:?}");
    }
}
