//! A2A's messages as JSON, in the form the JSON-RPC and HTTP+JSON bindings
//! use. That form is ProtoJSON: field names are camelCase, an enum value is
//! its proto name (e.g. `TASK_STATE_WORKING`), and a `oneof` is whichever one
//! of its fields is present. The types follow `a2a.proto`.
//!
//! Reading fails on a missing required field, on a `oneof` with no field or
//! with several, and on an empty list where A2A requires at least one
//! element. Writing leaves out an optional field that has no value.

mod card;
mod requests;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use card::{
    AgentCapabilities, AgentCard, AgentCardSignature, AgentExtension, AgentInterface,
    AgentProvider, AgentSkill, ApiKeySecurityScheme, AuthorizationCodeOauthFlow,
    ClientCredentialsOauthFlow, DeviceCodeOauthFlow, HttpAuthSecurityScheme, ImplicitOauthFlow,
    MutualTlsSecurityScheme, Oauth2SecurityScheme, OauthFlows, OpenIdConnectSecurityScheme,
    PasswordOauthFlow, SecurityRequirement, SecurityScheme, StringList,
};
pub use requests::{
    CancelTaskRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse,
    SendMessageConfiguration, SendMessageRequest, SubscribeToTaskRequest, method,
};

/// The A2A protocol version these types follow.
pub const PROTOCOL_VERSION: &str = "1.0";

/// A `google.protobuf.Struct`, which is a JSON object.
pub type Struct = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "ROLE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "ROLE_USER")]
    User,
    #[serde(rename = "ROLE_AGENT")]
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "TASK_STATE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
}

/// A section of a message's or an artifact's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PartFields", into = "PartFields")]
pub struct Part {
    pub content: PartContent,
    pub metadata: Option<Struct>,
    pub filename: Option<String>,
    pub media_type: Option<String>,
}

/// A part's `content`, which is exactly one of these.
#[derive(Debug, Clone, PartialEq)]
pub enum PartContent {
    Text(String),
    /// A file's bytes, base64-encoded.
    Raw(String),
    /// A URL of a file's content.
    Url(String),
    /// Any JSON value, including `null`.
    Data(Value),
}

// A part as its JSON fields.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Struct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

impl TryFrom<PartFields> for Part {
    type Error = String;

    fn try_from(fields: PartFields) -> Result<Self, String> {
        let content = one_of(
            [
                fields.text.map(PartContent::Text),
                fields.raw.map(PartContent::Raw),
                fields.url.map(PartContent::Url),
                fields.data.map(PartContent::Data),
            ],
            "a part",
        )?;
        Ok(Part {
            content,
            metadata: fields.metadata,
            filename: fields.filename,
            media_type: fields.media_type,
        })
    }
}

impl From<Part> for PartFields {
    fn from(part: Part) -> Self {
        let mut fields = PartFields {
            text: None,
            raw: None,
            url: None,
            data: None,
            metadata: part.metadata,
            filename: part.filename,
            media_type: part.media_type,
        };
        match part.content {
            PartContent::Text(text) => fields.text = Some(text),
            PartContent::Raw(raw) => fields.raw = Some(raw),
            PartContent::Url(url) => fields.url = Some(url),
            PartContent::Data(data) => fields.data = Some(data),
        }
        fields
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: Role,
    /// At least one.
    #[serde(deserialize_with = "message_parts")]
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// When the status was recorded, as an RFC 3339 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// At least one.
    #[serde(deserialize_with = "artifact_parts")]
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    pub task_id: String,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    pub task_id: String,
    pub context_id: String,
    pub artifact: Artifact,
    /// Whether the artifact's parts are added to those already sent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub append: bool,
    /// Whether this is the artifact's last chunk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_chunk: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Struct>,
}

/// The response to `SendMessage`, which is exactly one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    try_from = "SendMessageResponseFields",
    into = "SendMessageResponseFields"
)]
pub enum SendMessageResponse {
    Task(Task),
    Message(Message),
}

// A `SendMessage` response as its JSON fields.
#[derive(Default, Serialize, Deserialize)]
struct SendMessageResponseFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<Message>,
}

impl TryFrom<SendMessageResponseFields> for SendMessageResponse {
    type Error = String;

    fn try_from(fields: SendMessageResponseFields) -> Result<Self, String> {
        one_of(
            [
                fields.task.map(SendMessageResponse::Task),
                fields.message.map(SendMessageResponse::Message),
            ],
            "a response",
        )
    }
}

impl From<SendMessageResponse> for SendMessageResponseFields {
    fn from(response: SendMessageResponse) -> Self {
        let mut fields = SendMessageResponseFields::default();
        match response {
            SendMessageResponse::Task(task) => fields.task = Some(task),
            SendMessageResponse::Message(message) => fields.message = Some(message),
        }
        fields
    }
}

/// One response in the stream of `SendStreamingMessage` or `SubscribeToTask`,
/// which is exactly one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StreamResponseFields", into = "StreamResponseFields")]
pub enum StreamResponse {
    Task(Task),
    Message(Message),
    StatusUpdate(TaskStatusUpdateEvent),
    ArtifactUpdate(TaskArtifactUpdateEvent),
}

// A stream response as its JSON fields.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamResponseFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_update: Option<TaskStatusUpdateEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_update: Option<TaskArtifactUpdateEvent>,
}

impl TryFrom<StreamResponseFields> for StreamResponse {
    type Error = String;

    fn try_from(fields: StreamResponseFields) -> Result<Self, String> {
        one_of(
            [
                fields.task.map(StreamResponse::Task),
                fields.message.map(StreamResponse::Message),
                fields.status_update.map(StreamResponse::StatusUpdate),
                fields.artifact_update.map(StreamResponse::ArtifactUpdate),
            ],
            "a response",
        )
    }
}

impl From<StreamResponse> for StreamResponseFields {
    fn from(response: StreamResponse) -> Self {
        let mut fields = StreamResponseFields::default();
        match response {
            StreamResponse::Task(task) => fields.task = Some(task),
            StreamResponse::Message(message) => fields.message = Some(message),
            StreamResponse::StatusUpdate(update) => fields.status_update = Some(update),
            StreamResponse::ArtifactUpdate(update) => fields.artifact_update = Some(update),
        }
        fields
    }
}

// The one value that is set among a `oneof`'s fields. Fails when none or
// several are. `name` identifies the `oneof` in the error.
fn one_of<T>(fields: impl IntoIterator<Item = Option<T>>, name: &str) -> Result<T, String> {
    let mut set = fields.into_iter().flatten();
    match (set.next(), set.next()) {
        (Some(value), None) => Ok(value),
        (None, _) => Err(format!("{name} has none of its possible fields")),
        (Some(_), Some(_)) => Err(format!("{name} has more than one of its possible fields")),
    }
}

// Reads a list that A2A requires to have at least one element. `name`
// identifies the list in the error.
fn at_least_one<'de, D, T>(deserializer: D, name: &str) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let list = Vec::<T>::deserialize(deserializer)?;
    if list.is_empty() {
        return Err(D::Error::custom(format!(
            "{name} must have at least one element"
        )));
    }
    Ok(list)
}

fn message_parts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Part>, D::Error> {
    at_least_one(deserializer, "a message's parts")
}

fn artifact_parts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Part>, D::Error> {
    at_least_one(deserializer, "an artifact's parts")
}

// Reads a field that is present, so that JSON `null` is a value and not a
// missing field.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    fn message_json() -> Value {
        json!({
            "messageId": "m1",
            "role": "ROLE_USER",
            "parts": [{ "text": "What is 6 times 7?" }],
        })
    }

    #[test]
    fn an_enum_value_is_its_proto_name() {
        assert_eq!(json!(Role::User), json!("ROLE_USER"));
        assert_eq!(
            json!(TaskState::InputRequired),
            json!("TASK_STATE_INPUT_REQUIRED")
        );
        assert_eq!(
            read::<TaskState>(json!("TASK_STATE_CANCELED")),
            Ok(TaskState::Canceled)
        );
        assert!(read::<TaskState>(json!("canceled")).is_err());
    }

    #[test]
    fn a_part_is_written_as_the_field_for_its_content() {
        let part = Part {
            content: PartContent::Raw("iVBORw0KGgo=".to_string()),
            metadata: None,
            filename: Some("a.png".to_string()),
            media_type: Some("image/png".to_string()),
        };
        assert_eq!(
            json!(part),
            json!({ "raw": "iVBORw0KGgo=", "filename": "a.png", "mediaType": "image/png" })
        );
    }

    #[test]
    fn a_part_is_read_with_its_metadata() {
        let part: Part =
            read(json!({ "text": "Add the numbers.", "metadata": { "reasoning": true } })).unwrap();
        assert_eq!(
            part.content,
            PartContent::Text("Add the numbers.".to_string())
        );
        assert_eq!(part.metadata.unwrap()["reasoning"], json!(true));
    }

    #[test]
    fn a_part_has_exactly_one_content_field() {
        assert_eq!(
            read::<Part>(json!({ "mediaType": "text/plain" })).unwrap_err(),
            "a part has none of its possible fields"
        );
        assert_eq!(
            read::<Part>(json!({ "text": "hi", "url": "https://example.com/a" })).unwrap_err(),
            "a part has more than one of its possible fields"
        );
    }

    #[test]
    fn a_data_part_may_be_null() {
        let part: Part = read(json!({ "data": null })).unwrap();
        assert_eq!(part.content, PartContent::Data(Value::Null));
        assert_eq!(json!(part), json!({ "data": null }));
    }

    #[test]
    fn a_message_is_read_and_written_without_its_unset_fields() {
        let message: Message = read(message_json()).unwrap();
        assert_eq!(message.message_id, "m1");
        assert_eq!(message.context_id, None);
        assert!(message.extensions.is_empty());
        assert_eq!(json!(message), message_json());
    }

    #[test]
    fn a_message_without_a_required_field_is_not_read() {
        for field in ["messageId", "role", "parts"] {
            let mut json = message_json();
            json.as_object_mut().unwrap().remove(field);
            let error = read::<Message>(json).unwrap_err();
            assert!(
                error.contains(&format!("missing field `{field}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn a_required_list_must_have_an_element() {
        let mut json = message_json();
        json["parts"] = json!([]);
        assert_eq!(
            read::<Message>(json).unwrap_err(),
            "a message's parts must have at least one element"
        );
        let artifact = json!({ "artifactId": "a1", "parts": [] });
        assert_eq!(
            read::<Artifact>(artifact).unwrap_err(),
            "an artifact's parts must have at least one element"
        );
    }

    #[test]
    fn metadata_is_a_json_object() {
        let mut json = message_json();
        json["metadata"] = json!("not an object");
        assert!(read::<Message>(json).is_err());
    }

    fn task_json() -> Value {
        json!({
            "id": "t1",
            "contextId": "c1",
            "status": { "state": "TASK_STATE_WORKING" },
        })
    }

    #[test]
    fn a_task_is_read_and_written_without_its_unset_fields() {
        let task: Task = read(task_json()).unwrap();
        assert_eq!(task.status.state, TaskState::Working);
        assert!(task.artifacts.is_empty());
        assert_eq!(json!(task), task_json());
    }

    #[test]
    fn a_send_message_response_is_a_task_or_a_message() {
        let response: SendMessageResponse = read(json!({ "task": task_json() })).unwrap();
        assert!(matches!(&response, SendMessageResponse::Task(task) if task.id == "t1"));
        assert_eq!(json!(response), json!({ "task": task_json() }));

        let response: SendMessageResponse = read(json!({ "message": message_json() })).unwrap();
        assert!(matches!(response, SendMessageResponse::Message(_)));

        assert_eq!(
            read::<SendMessageResponse>(json!({})).unwrap_err(),
            "a response has none of its possible fields"
        );
    }

    #[test]
    fn each_kind_of_stream_response_is_read_and_written() {
        let status_update = json!({
            "statusUpdate": {
                "taskId": "t1",
                "contextId": "c1",
                "status": { "state": "TASK_STATE_COMPLETED" },
            }
        });
        // `append` is false, so it is left out.
        let artifact_update = json!({
            "artifactUpdate": {
                "taskId": "t1",
                "contextId": "c1",
                "artifact": { "artifactId": "a1", "parts": [{ "text": "42" }] },
                "lastChunk": true,
            }
        });
        for json in [
            json!({ "task": task_json() }),
            json!({ "message": message_json() }),
            status_update,
            artifact_update,
        ] {
            let response: StreamResponse = read(json.clone()).unwrap();
            assert_eq!(json!(response), json);
        }
    }

    #[test]
    fn a_stream_response_has_exactly_one_kind() {
        assert_eq!(
            read::<StreamResponse>(json!({})).unwrap_err(),
            "a response has none of its possible fields"
        );
        assert_eq!(
            read::<StreamResponse>(json!({ "task": task_json(), "message": message_json() }))
                .unwrap_err(),
            "a response has more than one of its possible fields"
        );
    }
}
