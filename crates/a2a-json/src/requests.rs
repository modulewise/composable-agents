//! The request messages of A2A's operations, and `ListTasksResponse`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Message, Struct, Task, TaskState, is_false};

/// The name of each A2A operation, as the JSON-RPC binding's `method`.
pub mod method {
    pub const SEND_MESSAGE: &str = "SendMessage";
    pub const SEND_STREAMING_MESSAGE: &str = "SendStreamingMessage";
    pub const GET_TASK: &str = "GetTask";
    pub const LIST_TASKS: &str = "ListTasks";
    pub const CANCEL_TASK: &str = "CancelTask";
    pub const SUBSCRIBE_TO_TASK: &str = "SubscribeToTask";
    pub const CREATE_TASK_PUSH_NOTIFICATION_CONFIG: &str = "CreateTaskPushNotificationConfig";
    pub const GET_TASK_PUSH_NOTIFICATION_CONFIG: &str = "GetTaskPushNotificationConfig";
    pub const LIST_TASK_PUSH_NOTIFICATION_CONFIGS: &str = "ListTaskPushNotificationConfigs";
    pub const DELETE_TASK_PUSH_NOTIFICATION_CONFIG: &str = "DeleteTaskPushNotificationConfig";
    pub const GET_EXTENDED_AGENT_CARD: &str = "GetExtendedAgentCard";
}

/// The request of `SendMessage` and `SendStreamingMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMessageRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<SendMessageConfiguration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageConfiguration {
    /// The media types the client accepts for response parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_output_modes: Vec<String>,
    /// A `TaskPushNotificationConfig`, as sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_push_notification_config: Option<Value>,
    /// The most messages of the task's history to return. `None` is no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
    /// Whether to return once the task is created, before it ends.
    #[serde(default, skip_serializing_if = "is_false")]
    pub return_immediately: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub id: String,
    /// The most messages of the task's history to return. `None` is no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// The most messages of each task's history to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
    /// An RFC 3339 timestamp. Only tasks whose status is as recent are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_timestamp_after: Option<String>,
    /// Whether each task includes its artifacts. `None` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_artifacts: Option<bool>,
}

/// A page of tasks. The page fields may be missing when read, because
/// ProtoJSON can leave out an empty string or a 0. They are always written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResponse {
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// Empty when there are no more tasks.
    #[serde(default)]
    pub next_page_token: String,
    #[serde(default)]
    pub page_size: i32,
    /// The number of tasks the request matches, in all pages.
    #[serde(default)]
    pub total_size: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeToTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_send_message_request_is_read_with_its_configuration() {
        let request: SendMessageRequest = serde_json::from_value(json!({
            "message": {
                "messageId": "m1",
                "role": "ROLE_USER",
                "parts": [{ "text": "Hi" }],
            },
            "configuration": {
                "acceptedOutputModes": ["text/plain"],
                "historyLength": 0,
                "returnImmediately": true,
            },
            "metadata": { "k": "v" },
        }))
        .unwrap();
        let configuration = request.configuration.unwrap();
        assert_eq!(configuration.accepted_output_modes, ["text/plain"]);
        assert_eq!(configuration.history_length, Some(0));
        assert!(configuration.return_immediately);
        assert_eq!(request.metadata.unwrap()["k"], json!("v"));
    }

    #[test]
    fn a_request_without_a_required_field_is_not_read() {
        let error = serde_json::from_value::<SendMessageRequest>(json!({})).unwrap_err();
        assert!(
            error.to_string().contains("missing field `message`"),
            "{error}"
        );
        let error = serde_json::from_value::<GetTaskRequest>(json!({})).unwrap_err();
        assert!(error.to_string().contains("missing field `id`"), "{error}");
    }

    #[test]
    fn a_list_tasks_request_is_written_with_only_what_is_set() {
        assert_eq!(json!(ListTasksRequest::default()), json!({}));
        let request = ListTasksRequest {
            context_id: Some("c1".to_string()),
            status: Some(TaskState::Working),
            page_size: Some(10),
            include_artifacts: Some(true),
            ..ListTasksRequest::default()
        };
        assert_eq!(
            json!(request),
            json!({
                "contextId": "c1",
                "status": "TASK_STATE_WORKING",
                "pageSize": 10,
                "includeArtifacts": true,
            })
        );
    }

    #[test]
    fn a_list_of_tasks_may_leave_out_its_page_fields_and_is_written_with_them() {
        let response: ListTasksResponse = serde_json::from_value(json!({})).unwrap();
        assert_eq!(response, ListTasksResponse::default());
        assert_eq!(
            json!(response),
            json!({ "tasks": [], "nextPageToken": "", "pageSize": 0, "totalSize": 0 })
        );
    }
}
