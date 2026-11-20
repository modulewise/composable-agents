//! The messages of the push notification config operations.

use serde::{Deserialize, Serialize};

/// A push notification config for a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPushNotificationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The config's id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The id of the config's task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The URL that notifications are sent to.
    pub url: String,
    /// A token for the task or session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<AuthenticationInfo>,
}

/// How to authenticate when sending a notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthenticationInfo {
    /// The HTTP authentication scheme, e.g. `Bearer`.
    pub scheme: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskPushNotificationConfigRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub task_id: String,
    /// The config's id.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsResponse {
    #[serde(default)]
    pub configs: Vec<TaskPushNotificationConfig>,
    /// Empty when there are no more configs.
    #[serde(default)]
    pub next_page_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTaskPushNotificationConfigRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub task_id: String,
    /// The config's id.
    pub id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_config_is_read_and_written_without_its_unset_fields() {
        let json = json!({
            "taskId": "t1",
            "url": "https://example.com/notifications",
            "authentication": { "scheme": "Bearer" },
        });
        let config: TaskPushNotificationConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(config.task_id.as_deref(), Some("t1"));
        assert_eq!(config.authentication.as_ref().unwrap().scheme, "Bearer");
        assert_eq!(json!(config), json);
    }

    #[test]
    fn a_config_without_a_url_is_not_read() {
        let error = serde_json::from_value::<TaskPushNotificationConfig>(json!({})).unwrap_err();
        assert!(error.to_string().contains("missing field `url`"), "{error}");
    }
}
