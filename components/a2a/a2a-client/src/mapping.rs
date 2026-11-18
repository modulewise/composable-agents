//! A2A's JSON, read into the WIT types and written from them. The JSON is
//! ProtoJSON: field names are camelCase, and an enum value is its proto name
//! (e.g. `TASK_STATE_WORKING`). A value the WIT holds as `json` is JSON text.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::composable::a2a::types::{
    AgentCapabilities, AgentCard, AgentCardSignature, AgentInterface, ApiKeySecurityScheme,
    AuthorizationCodeOauthFlow, ClientCredentialsOauthFlow, DeviceCodeOauthFlow,
    HttpAuthSecurityScheme, ImplicitOauthFlow, MutualTlsSecurityScheme, Oauth2SecurityScheme,
    OauthFlows, OpenIdConnectSecurityScheme, PasswordOauthFlow, SecurityScheme, StreamResponse,
};
use crate::composable::agents::types::{
    AgentExtension, AgentProvider, AgentSkill, Artifact, Message, Part, PartContent, Role,
    SecurityRequirement, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};
use crate::exports::composable::a2a::client::{
    CancelTaskRequest, Error, GetTaskRequest, ListTasksRequest, ListTasksResponse,
    SendMessageRequest, SendMessageResponse,
};

/// The JSON-RPC binding, as specified by an agent card.
const JSONRPC: &str = "JSONRPC";

/// The agent card in `bytes`. Fails when a required field is missing.
pub(crate) fn agent_card(bytes: &[u8]) -> Result<AgentCard, String> {
    let card: wire::AgentCard =
        serde_json::from_slice(bytes).map_err(|e| format!("the agent's card is not valid: {e}"))?;
    card.try_into()
}

/// The first of the card's interfaces that uses JSON-RPC and A2A `version`.
pub(crate) fn select_interface(card: &AgentCard, version: &str) -> Result<AgentInterface, String> {
    card.supported_interfaces
        .iter()
        .find(|interface| {
            interface.protocol_binding.eq_ignore_ascii_case(JSONRPC)
                && interface.protocol_version == version
        })
        .cloned()
        .ok_or_else(|| format!("the agent's card has no {JSONRPC} interface for A2A {version}"))
}

/// The URI of an extension the card requires that isn't in `requested`.
pub(crate) fn unrequested_required_extension<'a>(
    card: &'a AgentCard,
    requested: &[String],
) -> Option<&'a str> {
    card.capabilities
        .extensions
        .iter()
        .filter(|extension| extension.required)
        .filter_map(|extension| extension.uri.as_deref())
        .find(|uri| !requested.iter().any(|requested| requested == uri))
}

/// A JSON-RPC response's `result`, or the error it has instead.
pub(crate) fn rpc_result(response: Value) -> Result<Value, Error> {
    let Value::Object(mut response) = response else {
        return Err("the agent's response is not a JSON-RPC response"
            .to_string()
            .into());
    };
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        return Err(rpc_error(error));
    }
    response
        .remove("result")
        .ok_or_else(|| "the agent's response has no result".to_string().into())
}

// A JSON-RPC error object, with its code and data.
fn rpc_error(error: &Value) -> Error {
    Error {
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the agent returned an error with no message")
            .to_string(),
        code: error
            .get("code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
        data: error
            .get("data")
            .filter(|data| !data.is_null())
            .map(Value::to_string),
    }
}

/// A `Task` result.
pub(crate) fn task(result: Value) -> Result<Task, String> {
    parsed::<wire::Task>(result, "task")?.try_into()
}

/// A `SendMessageResponse` result.
pub(crate) fn send_message_response(result: Value) -> Result<SendMessageResponse, String> {
    match stream_response(result)? {
        StreamResponse::Task(task) => Ok(SendMessageResponse::Task(task)),
        StreamResponse::Message(message) => Ok(SendMessageResponse::Message(message)),
        StreamResponse::StatusUpdate(_) | StreamResponse::ArtifactUpdate(_) => {
            Err("the agent's response is neither a task nor a message".to_string())
        }
    }
}

/// A `StreamResponse` result.
pub(crate) fn stream_response(result: Value) -> Result<StreamResponse, String> {
    parsed::<wire::StreamResponse>(result, "response")?.try_into()
}

/// A `ListTasksResponse` result.
pub(crate) fn list_tasks_response(result: Value) -> Result<ListTasksResponse, String> {
    let response: wire::ListTasksResponse = parsed(result, "list of tasks")?;
    Ok(ListTasksResponse {
        tasks: response
            .tasks
            .into_iter()
            .map(Task::try_from)
            .collect::<Result<_, _>>()?,
        next_page_token: response.next_page_token,
        page_size: response.page_size,
        total_size: response.total_size,
    })
}

// `value` as `T`. `name` identifies the value in the error.
fn parsed<T: DeserializeOwned>(value: Value, name: &str) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("the agent's {name} is not valid: {e}"))
}

/// The params of `SendMessage` and `SendStreamingMessage`. Fails on a message
/// with no id, or on a `json` value that isn't JSON.
pub(crate) fn send_message_params(
    request: &SendMessageRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    let mut params = with_tenant(tenant);
    params.insert("message".into(), message_json(&request.message)?);
    if let Some(configuration) = &request.configuration {
        let mut fields = Map::new();
        if !configuration.accepted_output_modes.is_empty() {
            fields.insert(
                "acceptedOutputModes".into(),
                json!(configuration.accepted_output_modes),
            );
        }
        if let Some(history_length) = configuration.history_length {
            fields.insert("historyLength".into(), json!(history_length));
        }
        if configuration.return_immediately {
            fields.insert("returnImmediately".into(), json!(true));
        }
        params.insert("configuration".into(), Value::Object(fields));
    }
    insert_json(&mut params, "metadata", request.metadata.as_deref())?;
    Ok(Value::Object(params))
}

/// The params of `GetTask`.
pub(crate) fn get_task_params(request: &GetTaskRequest, tenant: Option<&str>) -> Value {
    let mut params = with_tenant(tenant);
    params.insert("id".into(), json!(request.id));
    if let Some(history_length) = request.history_length {
        params.insert("historyLength".into(), json!(history_length));
    }
    Value::Object(params)
}

/// The params of `CancelTask`. Fails on metadata that isn't JSON.
pub(crate) fn cancel_task_params(
    request: &CancelTaskRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    let mut params = with_tenant(tenant);
    params.insert("id".into(), json!(request.id));
    insert_json(&mut params, "metadata", request.metadata.as_deref())?;
    Ok(Value::Object(params))
}

/// The params of `ListTasks`.
pub(crate) fn list_tasks_params(request: &ListTasksRequest, tenant: Option<&str>) -> Value {
    let mut params = with_tenant(tenant);
    if let Some(context_id) = &request.context_id {
        params.insert("contextId".into(), json!(context_id));
    }
    if let Some(status) = request.status {
        params.insert("status".into(), json!(task_state_name(status)));
    }
    if let Some(page_size) = request.page_size {
        params.insert("pageSize".into(), json!(page_size));
    }
    if let Some(page_token) = &request.page_token {
        params.insert("pageToken".into(), json!(page_token));
    }
    if let Some(history_length) = request.history_length {
        params.insert("historyLength".into(), json!(history_length));
    }
    if let Some(timestamp) = &request.status_timestamp_after {
        params.insert("statusTimestampAfter".into(), json!(timestamp));
    }
    if let Some(include_artifacts) = request.include_artifacts {
        params.insert("includeArtifacts".into(), json!(include_artifacts));
    }
    Value::Object(params)
}

// Params with the tenant the selected interface declares, if any.
fn with_tenant(tenant: Option<&str>) -> Map<String, Value> {
    let mut params = Map::new();
    if let Some(tenant) = tenant {
        params.insert("tenant".into(), json!(tenant));
    }
    params
}

// Adds `text`, a `json` value, to `fields` as JSON. Fails when it isn't JSON.
fn insert_json(
    fields: &mut Map<String, Value>,
    key: &str,
    text: Option<&str>,
) -> Result<(), String> {
    if let Some(text) = text {
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("'{key}' is not valid JSON: {e}"))?;
        fields.insert(key.into(), value);
    }
    Ok(())
}

fn message_json(message: &Message) -> Result<Value, String> {
    if message.message_id.is_empty() {
        return Err("the message has no message-id".to_string());
    }
    let mut fields = Map::new();
    fields.insert("messageId".into(), json!(message.message_id));
    if let Some(context_id) = &message.context_id {
        fields.insert("contextId".into(), json!(context_id));
    }
    if let Some(task_id) = &message.task_id {
        fields.insert("taskId".into(), json!(task_id));
    }
    fields.insert("role".into(), json!(role_name(message.role)));
    let parts = message
        .parts
        .iter()
        .map(part_json)
        .collect::<Result<Vec<_>, _>>()?;
    fields.insert("parts".into(), Value::Array(parts));
    insert_json(&mut fields, "metadata", message.metadata.as_deref())?;
    if !message.extensions.is_empty() {
        fields.insert("extensions".into(), json!(message.extensions));
    }
    if !message.reference_task_ids.is_empty() {
        fields.insert("referenceTaskIds".into(), json!(message.reference_task_ids));
    }
    Ok(Value::Object(fields))
}

fn part_json(part: &Part) -> Result<Value, String> {
    let mut fields = Map::new();
    match &part.content {
        PartContent::Text(text) => fields.insert("text".into(), json!(text)),
        PartContent::Raw(raw) => fields.insert("raw".into(), json!(raw)),
        PartContent::Url(url) => fields.insert("url".into(), json!(url)),
        PartContent::Data(data) => {
            insert_json(&mut fields, "data", Some(data))?;
            None
        }
    };
    insert_json(&mut fields, "metadata", part.metadata.as_deref())?;
    if let Some(filename) = &part.filename {
        fields.insert("filename".into(), json!(filename));
    }
    if let Some(media_type) = &part.media_type {
        fields.insert("mediaType".into(), json!(media_type));
    }
    Ok(Value::Object(fields))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Unspecified => "ROLE_UNSPECIFIED",
        Role::User => "ROLE_USER",
        Role::Agent => "ROLE_AGENT",
    }
}

fn task_state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Unspecified => "TASK_STATE_UNSPECIFIED",
        TaskState::Submitted => "TASK_STATE_SUBMITTED",
        TaskState::Working => "TASK_STATE_WORKING",
        TaskState::Completed => "TASK_STATE_COMPLETED",
        TaskState::Failed => "TASK_STATE_FAILED",
        TaskState::Canceled => "TASK_STATE_CANCELED",
        TaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
        TaskState::Rejected => "TASK_STATE_REJECTED",
        TaskState::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
    }
}

// JSON text for a `json` field.
fn json_text(value: Option<Value>) -> Option<String> {
    value.map(|value| value.to_string())
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

impl TryFrom<wire::Part> for Part {
    type Error = String;

    fn try_from(part: wire::Part) -> Result<Self, String> {
        let content = one_of(
            [
                part.text.map(PartContent::Text),
                part.raw.map(PartContent::Raw),
                part.url.map(PartContent::Url),
                part.data.map(|data| PartContent::Data(data.to_string())),
            ],
            "a part",
        )?;
        Ok(Part {
            content,
            metadata: json_text(part.metadata),
            filename: part.filename,
            media_type: part.media_type,
        })
    }
}

// `list`, which A2A requires to have at least one element. `name` identifies
// the list in the error.
fn non_empty<T>(list: Vec<T>, name: &str) -> Result<Vec<T>, String> {
    if list.is_empty() {
        return Err(format!("{name} must have at least one element"));
    }
    Ok(list)
}

// The parts of `owner`, e.g. "a message".
fn parts(parts: Vec<wire::Part>, owner: &str) -> Result<Vec<Part>, String> {
    non_empty(parts, &format!("{owner}'s parts"))?
        .into_iter()
        .map(Part::try_from)
        .collect()
}

impl TryFrom<wire::Message> for Message {
    type Error = String;

    fn try_from(message: wire::Message) -> Result<Self, String> {
        Ok(Message {
            message_id: message.message_id,
            context_id: message.context_id,
            task_id: message.task_id,
            role: message.role.into(),
            parts: parts(message.parts, "a message")?,
            metadata: json_text(message.metadata),
            extensions: message.extensions,
            reference_task_ids: message.reference_task_ids,
        })
    }
}

impl From<wire::Role> for Role {
    fn from(role: wire::Role) -> Self {
        match role {
            wire::Role::Unspecified => Role::Unspecified,
            wire::Role::User => Role::User,
            wire::Role::Agent => Role::Agent,
        }
    }
}

impl From<wire::TaskState> for TaskState {
    fn from(state: wire::TaskState) -> Self {
        match state {
            wire::TaskState::Unspecified => TaskState::Unspecified,
            wire::TaskState::Submitted => TaskState::Submitted,
            wire::TaskState::Working => TaskState::Working,
            wire::TaskState::Completed => TaskState::Completed,
            wire::TaskState::Failed => TaskState::Failed,
            wire::TaskState::Canceled => TaskState::Canceled,
            wire::TaskState::InputRequired => TaskState::InputRequired,
            wire::TaskState::Rejected => TaskState::Rejected,
            wire::TaskState::AuthRequired => TaskState::AuthRequired,
        }
    }
}

impl TryFrom<wire::TaskStatus> for TaskStatus {
    type Error = String;

    fn try_from(status: wire::TaskStatus) -> Result<Self, String> {
        Ok(TaskStatus {
            state: status.state.into(),
            message: status.message.map(Message::try_from).transpose()?,
            timestamp: status.timestamp,
        })
    }
}

impl TryFrom<wire::Artifact> for Artifact {
    type Error = String;

    fn try_from(artifact: wire::Artifact) -> Result<Self, String> {
        Ok(Artifact {
            artifact_id: artifact.artifact_id,
            name: artifact.name,
            description: artifact.description,
            parts: parts(artifact.parts, "an artifact")?,
            metadata: json_text(artifact.metadata),
            extensions: artifact.extensions,
        })
    }
}

impl TryFrom<wire::Task> for Task {
    type Error = String;

    fn try_from(task: wire::Task) -> Result<Self, String> {
        Ok(Task {
            id: task.id,
            context_id: task.context_id,
            status: task.status.try_into()?,
            artifacts: task
                .artifacts
                .into_iter()
                .map(Artifact::try_from)
                .collect::<Result<_, _>>()?,
            history: task
                .history
                .into_iter()
                .map(Message::try_from)
                .collect::<Result<_, _>>()?,
            metadata: json_text(task.metadata),
        })
    }
}

impl TryFrom<wire::StreamResponse> for StreamResponse {
    type Error = String;

    fn try_from(response: wire::StreamResponse) -> Result<Self, String> {
        let response = one_of(
            [
                response.task.map(wire::Payload::Task),
                response.message.map(wire::Payload::Message),
                response.status_update.map(wire::Payload::StatusUpdate),
                response.artifact_update.map(wire::Payload::ArtifactUpdate),
            ],
            "a response",
        )?;
        Ok(match response {
            wire::Payload::Task(task) => StreamResponse::Task(task.try_into()?),
            wire::Payload::Message(message) => StreamResponse::Message(message.try_into()?),
            wire::Payload::StatusUpdate(update) => {
                StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                    task_id: update.task_id,
                    context_id: update.context_id,
                    status: update.status.try_into()?,
                    metadata: json_text(update.metadata),
                })
            }
            wire::Payload::ArtifactUpdate(update) => {
                StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                    task_id: update.task_id,
                    context_id: update.context_id,
                    artifact: update.artifact.try_into()?,
                    append: update.append,
                    last_chunk: update.last_chunk,
                    metadata: json_text(update.metadata),
                })
            }
        })
    }
}

impl From<wire::SecurityRequirement> for SecurityRequirement {
    fn from(requirement: wire::SecurityRequirement) -> Self {
        SecurityRequirement {
            schemes: requirement
                .schemes
                .into_iter()
                .map(|(name, scopes)| (name, scopes.list))
                .collect(),
        }
    }
}

fn security_requirements(requirements: Vec<wire::SecurityRequirement>) -> Vec<SecurityRequirement> {
    requirements.into_iter().map(Into::into).collect()
}

impl TryFrom<wire::SecurityScheme> for SecurityScheme {
    type Error = String;

    fn try_from(scheme: wire::SecurityScheme) -> Result<Self, String> {
        one_of(
            [
                scheme.api_key_security_scheme.map(|scheme| {
                    Ok(SecurityScheme::ApiKeySecurityScheme(ApiKeySecurityScheme {
                        description: scheme.description,
                        location: scheme.location,
                        name: scheme.name,
                    }))
                }),
                scheme.http_auth_security_scheme.map(|scheme| {
                    Ok(SecurityScheme::HttpAuthSecurityScheme(
                        HttpAuthSecurityScheme {
                            description: scheme.description,
                            scheme: scheme.scheme,
                            bearer_format: scheme.bearer_format,
                        },
                    ))
                }),
                scheme.oauth2_security_scheme.map(|scheme| {
                    Ok(SecurityScheme::Oauth2SecurityScheme(Oauth2SecurityScheme {
                        description: scheme.description,
                        flows: scheme.flows.try_into()?,
                        oauth2_metadata_url: scheme.oauth2_metadata_url,
                    }))
                }),
                scheme.open_id_connect_security_scheme.map(|scheme| {
                    Ok(SecurityScheme::OpenIdConnectSecurityScheme(
                        OpenIdConnectSecurityScheme {
                            description: scheme.description,
                            open_id_connect_url: scheme.open_id_connect_url,
                        },
                    ))
                }),
                scheme.mtls_security_scheme.map(|scheme| {
                    Ok(SecurityScheme::MtlsSecurityScheme(
                        MutualTlsSecurityScheme {
                            description: scheme.description,
                        },
                    ))
                }),
            ],
            "a security scheme",
        )?
    }
}

impl TryFrom<wire::OauthFlows> for OauthFlows {
    type Error = String;

    fn try_from(flows: wire::OauthFlows) -> Result<Self, String> {
        one_of(
            [
                flows.authorization_code.map(|flow| {
                    OauthFlows::AuthorizationCode(AuthorizationCodeOauthFlow {
                        authorization_url: flow.authorization_url,
                        token_url: flow.token_url,
                        refresh_url: flow.refresh_url,
                        scopes: flow.scopes.into_iter().collect(),
                        pkce_required: flow.pkce_required,
                    })
                }),
                flows.client_credentials.map(|flow| {
                    OauthFlows::ClientCredentials(ClientCredentialsOauthFlow {
                        token_url: flow.token_url,
                        refresh_url: flow.refresh_url,
                        scopes: flow.scopes.into_iter().collect(),
                    })
                }),
                flows.implicit.map(|flow| {
                    OauthFlows::Implicit(ImplicitOauthFlow {
                        authorization_url: flow.authorization_url,
                        refresh_url: flow.refresh_url,
                        scopes: flow.scopes.into_iter().collect(),
                    })
                }),
                flows.password.map(|flow| {
                    OauthFlows::Password(PasswordOauthFlow {
                        token_url: flow.token_url,
                        refresh_url: flow.refresh_url,
                        scopes: flow.scopes.into_iter().collect(),
                    })
                }),
                flows.device_code.map(|flow| {
                    OauthFlows::DeviceCode(DeviceCodeOauthFlow {
                        device_authorization_url: flow.device_authorization_url,
                        token_url: flow.token_url,
                        refresh_url: flow.refresh_url,
                        scopes: flow.scopes.into_iter().collect(),
                    })
                }),
            ],
            "a security scheme's flows",
        )
    }
}

impl From<wire::AgentExtension> for AgentExtension {
    fn from(extension: wire::AgentExtension) -> Self {
        AgentExtension {
            uri: extension.uri,
            description: extension.description,
            required: extension.required,
            params: json_text(extension.params),
        }
    }
}

impl TryFrom<wire::AgentCard> for AgentCard {
    type Error = String;

    fn try_from(card: wire::AgentCard) -> Result<Self, String> {
        Ok(AgentCard {
            name: card.name,
            description: card.description,
            supported_interfaces: non_empty(card.supported_interfaces, "a card's interfaces")?
                .into_iter()
                .map(|interface| AgentInterface {
                    url: interface.url,
                    protocol_binding: interface.protocol_binding,
                    tenant: interface.tenant,
                    protocol_version: interface.protocol_version,
                })
                .collect(),
            provider: card.provider.map(|provider| AgentProvider {
                url: provider.url,
                organization: provider.organization,
            }),
            version: card.version,
            documentation_url: card.documentation_url,
            capabilities: AgentCapabilities {
                streaming: card.capabilities.streaming,
                push_notifications: card.capabilities.push_notifications,
                extensions: card
                    .capabilities
                    .extensions
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                extended_agent_card: card.capabilities.extended_agent_card,
            },
            security_schemes: card
                .security_schemes
                .into_iter()
                .map(|(name, scheme)| Ok((name, scheme.try_into()?)))
                .collect::<Result<_, String>>()?,
            security_requirements: security_requirements(card.security_requirements),
            default_input_modes: non_empty(card.default_input_modes, "a card's input modes")?,
            default_output_modes: non_empty(card.default_output_modes, "a card's output modes")?,
            skills: non_empty(card.skills, "a card's skills")?
                .into_iter()
                .map(|skill| {
                    Ok(AgentSkill {
                        id: skill.id,
                        name: skill.name,
                        description: skill.description,
                        tags: non_empty(skill.tags, "a skill's tags")?,
                        examples: skill.examples,
                        input_modes: skill.input_modes,
                        output_modes: skill.output_modes,
                        security_requirements: security_requirements(skill.security_requirements),
                    })
                })
                .collect::<Result<_, String>>()?,
            signatures: card
                .signatures
                .into_iter()
                .map(|signature| AgentCardSignature {
                    protected: signature.protected,
                    signature: signature.signature,
                    header: json_text(signature.header),
                })
                .collect(),
            icon_url: card.icon_url,
        })
    }
}

// A2A's messages as their JSON. A field without `Option` or a default is
// required, so JSON without it doesn't parse.
mod wire {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    pub(super) enum Role {
        #[serde(rename = "ROLE_UNSPECIFIED")]
        Unspecified,
        #[serde(rename = "ROLE_USER")]
        User,
        #[serde(rename = "ROLE_AGENT")]
        Agent,
    }

    #[derive(Deserialize)]
    pub(super) enum TaskState {
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

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Part {
        pub text: Option<String>,
        pub raw: Option<String>,
        pub url: Option<String>,
        pub data: Option<Value>,
        pub metadata: Option<Value>,
        pub filename: Option<String>,
        pub media_type: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Message {
        pub message_id: String,
        pub context_id: Option<String>,
        pub task_id: Option<String>,
        pub role: Role,
        pub parts: Vec<Part>,
        pub metadata: Option<Value>,
        #[serde(default)]
        pub extensions: Vec<String>,
        #[serde(default)]
        pub reference_task_ids: Vec<String>,
    }

    #[derive(Deserialize)]
    pub(super) struct TaskStatus {
        pub state: TaskState,
        pub message: Option<Message>,
        pub timestamp: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Artifact {
        pub artifact_id: String,
        pub name: Option<String>,
        pub description: Option<String>,
        pub parts: Vec<Part>,
        pub metadata: Option<Value>,
        #[serde(default)]
        pub extensions: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Task {
        pub id: String,
        pub context_id: Option<String>,
        pub status: TaskStatus,
        #[serde(default)]
        pub artifacts: Vec<Artifact>,
        #[serde(default)]
        pub history: Vec<Message>,
        pub metadata: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct TaskStatusUpdateEvent {
        pub task_id: String,
        pub context_id: String,
        pub status: TaskStatus,
        pub metadata: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct TaskArtifactUpdateEvent {
        pub task_id: String,
        pub context_id: String,
        pub artifact: Artifact,
        #[serde(default)]
        pub append: bool,
        #[serde(default)]
        pub last_chunk: bool,
        pub metadata: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct StreamResponse {
        pub task: Option<Task>,
        pub message: Option<Message>,
        pub status_update: Option<TaskStatusUpdateEvent>,
        pub artifact_update: Option<TaskArtifactUpdateEvent>,
    }

    // The field of a `StreamResponse` that is set.
    pub(super) enum Payload {
        Task(Task),
        Message(Message),
        StatusUpdate(TaskStatusUpdateEvent),
        ArtifactUpdate(TaskArtifactUpdateEvent),
    }

    // The page fields may be missing: ProtoJSON can leave out an empty string
    // or a 0.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct ListTasksResponse {
        #[serde(default)]
        pub tasks: Vec<Task>,
        #[serde(default)]
        pub next_page_token: String,
        #[serde(default)]
        pub page_size: u32,
        #[serde(default)]
        pub total_size: u32,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct AgentInterface {
        pub url: String,
        pub protocol_binding: String,
        pub tenant: Option<String>,
        pub protocol_version: String,
    }

    #[derive(Deserialize)]
    pub(super) struct AgentProvider {
        pub url: String,
        pub organization: String,
    }

    #[derive(Deserialize)]
    pub(super) struct AgentExtension {
        pub uri: Option<String>,
        pub description: Option<String>,
        #[serde(default)]
        pub required: bool,
        pub params: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct AgentCapabilities {
        pub streaming: Option<bool>,
        pub push_notifications: Option<bool>,
        #[serde(default)]
        pub extensions: Vec<AgentExtension>,
        pub extended_agent_card: Option<bool>,
    }

    #[derive(Deserialize)]
    pub(super) struct StringList {
        #[serde(default)]
        pub list: Vec<String>,
    }

    #[derive(Deserialize)]
    pub(super) struct SecurityRequirement {
        #[serde(default)]
        pub schemes: BTreeMap<String, StringList>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct AgentSkill {
        pub id: String,
        pub name: String,
        pub description: String,
        pub tags: Vec<String>,
        #[serde(default)]
        pub examples: Vec<String>,
        #[serde(default)]
        pub input_modes: Vec<String>,
        #[serde(default)]
        pub output_modes: Vec<String>,
        #[serde(default)]
        pub security_requirements: Vec<SecurityRequirement>,
    }

    #[derive(Deserialize)]
    pub(super) struct ApiKeySecurityScheme {
        pub description: Option<String>,
        pub location: String,
        pub name: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct HttpAuthSecurityScheme {
        pub description: Option<String>,
        pub scheme: String,
        pub bearer_format: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Oauth2SecurityScheme {
        pub description: Option<String>,
        pub flows: OauthFlows,
        pub oauth2_metadata_url: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct OpenIdConnectSecurityScheme {
        pub description: Option<String>,
        pub open_id_connect_url: String,
    }

    #[derive(Deserialize)]
    pub(super) struct MutualTlsSecurityScheme {
        pub description: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct SecurityScheme {
        pub api_key_security_scheme: Option<ApiKeySecurityScheme>,
        pub http_auth_security_scheme: Option<HttpAuthSecurityScheme>,
        pub oauth2_security_scheme: Option<Oauth2SecurityScheme>,
        pub open_id_connect_security_scheme: Option<OpenIdConnectSecurityScheme>,
        pub mtls_security_scheme: Option<MutualTlsSecurityScheme>,
    }

    // A flow's scopes may be missing: ProtoJSON can leave out an empty map.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct AuthorizationCodeOauthFlow {
        pub authorization_url: String,
        pub token_url: String,
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
        #[serde(default)]
        pub pkce_required: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct ClientCredentialsOauthFlow {
        pub token_url: String,
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct ImplicitOauthFlow {
        pub authorization_url: Option<String>,
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct PasswordOauthFlow {
        pub token_url: Option<String>,
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct DeviceCodeOauthFlow {
        pub device_authorization_url: String,
        pub token_url: String,
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct OauthFlows {
        pub authorization_code: Option<AuthorizationCodeOauthFlow>,
        pub client_credentials: Option<ClientCredentialsOauthFlow>,
        pub implicit: Option<ImplicitOauthFlow>,
        pub password: Option<PasswordOauthFlow>,
        pub device_code: Option<DeviceCodeOauthFlow>,
    }

    #[derive(Deserialize)]
    pub(super) struct AgentCardSignature {
        pub protected: String,
        pub signature: String,
        pub header: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct AgentCard {
        pub name: String,
        pub description: String,
        pub supported_interfaces: Vec<AgentInterface>,
        pub provider: Option<AgentProvider>,
        pub version: String,
        pub documentation_url: Option<String>,
        pub capabilities: AgentCapabilities,
        #[serde(default)]
        pub security_schemes: BTreeMap<String, SecurityScheme>,
        #[serde(default)]
        pub security_requirements: Vec<SecurityRequirement>,
        pub default_input_modes: Vec<String>,
        pub default_output_modes: Vec<String>,
        pub skills: Vec<AgentSkill>,
        #[serde(default)]
        pub signatures: Vec<AgentCardSignature>,
        pub icon_url: Option<String>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::composable::a2a::client::SendMessageConfiguration;

    fn card_json() -> Value {
        json!({
            "name": "Researcher",
            "description": "Explores and discovers",
            "supportedInterfaces": [
                { "url": "https://agent.example.com/grpc", "protocolBinding": "GRPC", "protocolVersion": "1.0" },
                { "url": "https://agent.example.com/old", "protocolBinding": "JSONRPC", "protocolVersion": "0.3" },
                { "url": "https://agent.example.com/rpc", "protocolBinding": "JSONRPC", "protocolVersion": "1.0", "tenant": "acme" },
            ],
            "version": "1.2.0",
            "capabilities": {
                "streaming": true,
                "extensions": [
                    { "uri": "https://example.com/ext/a", "required": true },
                    { "uri": "https://example.com/ext/b" },
                ],
            },
            "securitySchemes": {
                "bearer": { "httpAuthSecurityScheme": { "scheme": "Bearer", "bearerFormat": "JWT" } },
                "oauth": { "oauth2SecurityScheme": { "flows": { "clientCredentials": {
                    "tokenUrl": "https://auth.example.com/token",
                    "scopes": { "read": "Read tasks" },
                } } } },
            },
            "securityRequirements": [{ "schemes": { "oauth": { "list": ["read"] } } }],
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": [{ "id": "search", "name": "Search", "description": "Searches", "tags": ["web"] }],
            // The signature of the A2A specification's example card (8.4).
            "signatures": [{
                "protected": "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpPU0UiLCJraWQiOiJrZXktMSIsImprdSI6Imh0dHBzOi8vZXhhbXBsZS5jb20vYWdlbnQvandrcy5qc29uIn0",
                "signature": "QFdkNLNszlGj3z3u0YQGt_T9LixY3qtdQpZmsTdDHDe3fXV9y9-B3m2-XgCpzuhiLt8E0tV6HXoZKHv4GtHgKQ",
                "header": { "kid": "key-1" },
            }],
        })
    }

    fn card(json: &Value) -> Result<AgentCard, String> {
        agent_card(json.to_string().as_bytes())
    }

    #[test]
    fn a_card_is_read_with_its_security_schemes_and_signatures() {
        let card = card(&card_json()).unwrap();
        assert_eq!(card.name, "Researcher");
        assert_eq!(card.supported_interfaces.len(), 3);
        assert_eq!(card.capabilities.streaming, Some(true));
        assert!(card.capabilities.extensions[0].required);
        assert!(!card.capabilities.extensions[1].required);
        assert_eq!(card.skills[0].tags, vec!["web".to_string()]);
        assert_eq!(
            card.signatures[0].header.as_deref(),
            Some(r#"{"kid":"key-1"}"#)
        );

        let schemes: Vec<_> = card.security_schemes.into_iter().collect();
        assert_eq!(schemes.len(), 2);
        for (name, scheme) in schemes {
            match (name.as_str(), scheme) {
                ("bearer", SecurityScheme::HttpAuthSecurityScheme(scheme)) => {
                    assert_eq!(scheme.scheme, "Bearer");
                    assert_eq!(scheme.bearer_format.as_deref(), Some("JWT"));
                }
                ("oauth", SecurityScheme::Oauth2SecurityScheme(scheme)) => {
                    let OauthFlows::ClientCredentials(flow) = scheme.flows else {
                        panic!("expected the client credentials flow");
                    };
                    assert_eq!(flow.token_url, "https://auth.example.com/token");
                    let scopes: Vec<_> = flow.scopes.into_iter().collect();
                    assert_eq!(scopes, vec![("read".to_string(), "Read tasks".to_string())]);
                }
                (name, _) => panic!("unexpected scheme {name}"),
            }
        }
        let required: Vec<_> = card.security_requirements[0]
            .schemes
            .clone()
            .into_iter()
            .collect();
        assert_eq!(
            required,
            vec![("oauth".to_string(), vec!["read".to_string()])]
        );
    }

    #[test]
    fn a_card_without_a_required_field_is_not_valid() {
        for field in [
            "name",
            "version",
            "capabilities",
            "supportedInterfaces",
            "skills",
        ] {
            let mut json = card_json();
            json.as_object_mut().unwrap().remove(field);
            let error = card(&json).err().unwrap();
            assert!(
                error.starts_with("the agent's card is not valid"),
                "{field}: {error}"
            );
        }
    }

    #[test]
    fn a_required_list_must_have_an_element() {
        for (field, name) in [
            ("supportedInterfaces", "a card's interfaces"),
            ("defaultInputModes", "a card's input modes"),
            ("defaultOutputModes", "a card's output modes"),
            ("skills", "a card's skills"),
        ] {
            let mut json = card_json();
            json[field] = json!([]);
            assert_eq!(
                card(&json).err().unwrap(),
                format!("{name} must have at least one element")
            );
        }
        let mut json = card_json();
        json["skills"][0]["tags"] = json!([]);
        assert_eq!(
            card(&json).err().unwrap(),
            "a skill's tags must have at least one element"
        );
    }

    #[test]
    fn a_security_scheme_has_exactly_one_kind() {
        let mut json = card_json();
        json["securitySchemes"]["bearer"] = json!({});
        assert_eq!(
            card(&json).err().unwrap(),
            "a security scheme has none of its possible fields"
        );
        json["securitySchemes"]["bearer"] = json!({
            "httpAuthSecurityScheme": { "scheme": "Bearer" },
            "mtlsSecurityScheme": {},
        });
        assert_eq!(
            card(&json).err().unwrap(),
            "a security scheme has more than one of its possible fields"
        );
    }

    #[test]
    fn the_first_jsonrpc_interface_of_the_version_is_selected() {
        let card = card(&card_json()).unwrap();
        let interface = select_interface(&card, "1.0").unwrap();
        assert_eq!(interface.url, "https://agent.example.com/rpc");
        assert_eq!(interface.tenant.as_deref(), Some("acme"));
        assert_eq!(
            select_interface(&card, "2.0").err().unwrap(),
            "the agent's card has no JSONRPC interface for A2A 2.0"
        );
    }

    #[test]
    fn a_required_extension_that_was_not_requested_is_found() {
        let card = card(&card_json()).unwrap();
        const A: &str = "https://example.com/ext/a";
        const B: &str = "https://example.com/ext/b";
        assert_eq!(unrequested_required_extension(&card, &[]), Some(A));
        assert_eq!(
            unrequested_required_extension(&card, &[B.to_string()]),
            Some(A)
        );
        assert_eq!(
            unrequested_required_extension(&card, &[A.to_string()]),
            None
        );
    }

    #[test]
    fn a_jsonrpc_error_keeps_its_code_and_data() {
        let error = rpc_result(json!({ "jsonrpc": "2.0", "id": 1, "error": {
            "code": -32001,
            "message": "no task with id task-1",
            "data": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo" }],
        } }))
        .err()
        .unwrap();
        assert_eq!(error.message, "no task with id task-1");
        assert_eq!(error.code, Some(-32001));
        assert_eq!(
            error.data.as_deref(),
            Some(r#"[{"@type":"type.googleapis.com/google.rpc.ErrorInfo"}]"#)
        );
    }

    #[test]
    fn a_jsonrpc_response_gives_its_result() {
        assert_eq!(
            rpc_result(json!({ "jsonrpc": "2.0", "id": 1, "result": { "a": 1 } })).ok(),
            Some(json!({ "a": 1 }))
        );
        let error = rpc_result(json!({ "jsonrpc": "2.0", "id": 1 }))
            .err()
            .unwrap();
        assert_eq!(error.message, "the agent's response has no result");
        assert_eq!(error.code, None);
    }

    fn task_json(state: &str) -> Value {
        json!({ "id": "task-1", "contextId": "ctx-1", "status": { "state": state } })
    }

    #[test]
    fn each_kind_of_stream_response_is_read() {
        let StreamResponse::Task(task) =
            stream_response(json!({ "task": task_json("TASK_STATE_WORKING") })).unwrap()
        else {
            panic!("expected a task");
        };
        assert_eq!(task.id, "task-1");
        assert!(matches!(task.status.state, TaskState::Working));

        let message =
            json!({ "messageId": "m-1", "role": "ROLE_AGENT", "parts": [{ "text": "hi" }] });
        let StreamResponse::Message(message) =
            stream_response(json!({ "message": message })).unwrap()
        else {
            panic!("expected a message");
        };
        assert!(matches!(message.role, Role::Agent));
        assert!(matches!(&message.parts[0].content, PartContent::Text(text) if text == "hi"));

        let update = json!({ "taskId": "task-1", "contextId": "ctx-1", "status": { "state": "TASK_STATE_COMPLETED" } });
        let StreamResponse::StatusUpdate(update) =
            stream_response(json!({ "statusUpdate": update })).unwrap()
        else {
            panic!("expected a status update");
        };
        assert!(matches!(update.status.state, TaskState::Completed));

        let update = json!({
            "taskId": "task-1",
            "contextId": "ctx-1",
            "artifact": { "artifactId": "a-1", "parts": [{ "data": { "n": 1 }, "mediaType": "application/json" }] },
            "lastChunk": true,
        });
        let StreamResponse::ArtifactUpdate(update) =
            stream_response(json!({ "artifactUpdate": update })).unwrap()
        else {
            panic!("expected an artifact update");
        };
        assert!(!update.append);
        assert!(update.last_chunk);
        assert!(
            matches!(&update.artifact.parts[0].content, PartContent::Data(data) if data == r#"{"n":1}"#)
        );
    }

    #[test]
    fn a_stream_response_has_exactly_one_kind() {
        assert_eq!(
            stream_response(json!({})).err().unwrap(),
            "a response has none of its possible fields"
        );
        let message =
            json!({ "messageId": "m-1", "role": "ROLE_AGENT", "parts": [{ "text": "hi" }] });
        assert_eq!(
            stream_response(json!({ "task": task_json("TASK_STATE_WORKING"), "message": message }))
                .err()
                .unwrap(),
            "a response has more than one of its possible fields"
        );
    }

    #[test]
    fn a_response_without_a_required_field_is_not_valid() {
        // No task id, an unknown state, a message with no parts, and a part
        // with no content.
        for result in [
            json!({ "task": { "status": { "state": "TASK_STATE_WORKING" } } }),
            json!({ "task": task_json("TASK_STATE_RUNNING") }),
            json!({ "message": { "messageId": "m-1", "role": "ROLE_AGENT", "parts": [] } }),
            json!({ "message": { "messageId": "m-1", "role": "ROLE_AGENT", "parts": [{}] } }),
        ] {
            assert!(stream_response(result.clone()).is_err(), "{result}");
        }
    }

    #[test]
    fn a_send_message_response_is_a_task_or_a_message() {
        assert!(matches!(
            send_message_response(json!({ "task": task_json("TASK_STATE_COMPLETED") })),
            Ok(SendMessageResponse::Task(_))
        ));
        let update = json!({ "taskId": "task-1", "contextId": "ctx-1", "status": { "state": "TASK_STATE_COMPLETED" } });
        assert_eq!(
            send_message_response(json!({ "statusUpdate": update }))
                .err()
                .unwrap(),
            "the agent's response is neither a task nor a message"
        );
    }

    #[test]
    fn a_list_of_tasks_may_leave_out_its_page_fields() {
        let response =
            list_tasks_response(json!({ "tasks": [task_json("TASK_STATE_COMPLETED")] })).unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert_eq!(response.next_page_token, "");
        assert_eq!((response.page_size, response.total_size), (0, 0));
    }

    fn message(message_id: &str) -> Message {
        Message {
            message_id: message_id.to_string(),
            context_id: Some("ctx-1".to_string()),
            task_id: None,
            role: Role::User,
            parts: vec![Part {
                content: PartContent::Text("hello".to_string()),
                metadata: None,
                filename: None,
                media_type: None,
            }],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }
    }

    #[test]
    fn send_message_params_have_the_message_configuration_metadata_and_tenant() {
        let request = SendMessageRequest {
            message: message("m-1"),
            configuration: Some(SendMessageConfiguration {
                accepted_output_modes: vec!["text/plain".to_string()],
                history_length: Some(0),
                return_immediately: true,
            }),
            metadata: Some(r#"{"trace":"t-1"}"#.to_string()),
        };
        assert_eq!(
            send_message_params(&request, Some("acme")).unwrap(),
            json!({
                "tenant": "acme",
                "message": {
                    "messageId": "m-1",
                    "contextId": "ctx-1",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "hello" }],
                },
                "configuration": {
                    "acceptedOutputModes": ["text/plain"],
                    "historyLength": 0,
                    "returnImmediately": true,
                },
                "metadata": { "trace": "t-1" },
            })
        );
    }

    #[test]
    fn a_message_without_an_id_is_not_sent() {
        let request = SendMessageRequest {
            message: message(""),
            configuration: None,
            metadata: None,
        };
        assert_eq!(
            send_message_params(&request, None).err().unwrap(),
            "the message has no message-id"
        );
    }

    #[test]
    fn metadata_that_is_not_json_is_not_sent() {
        let request = SendMessageRequest {
            message: message("m-1"),
            configuration: None,
            metadata: Some("not json".to_string()),
        };
        let error = send_message_params(&request, None).err().unwrap();
        assert!(error.starts_with("'metadata' is not valid JSON"), "{error}");
    }

    #[test]
    fn list_tasks_params_have_only_what_the_request_sets() {
        let request = ListTasksRequest {
            context_id: Some("ctx-1".to_string()),
            status: Some(TaskState::InputRequired),
            page_size: Some(10),
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            include_artifacts: Some(true),
        };
        assert_eq!(
            list_tasks_params(&request, None),
            json!({
                "contextId": "ctx-1",
                "status": "TASK_STATE_INPUT_REQUIRED",
                "pageSize": 10,
                "includeArtifacts": true,
            })
        );
    }
}
