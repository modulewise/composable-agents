//! A2A's JSON, read into the WIT types and written from them, through the
//! `a2a-json` types. A value the WIT holds as `json` is JSON text.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

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
    SendMessageConfiguration, SendMessageRequest, SendMessageResponse,
};

/// The JSON-RPC binding, as specified by an agent card.
const JSONRPC: &str = "JSONRPC";

/// The agent card in `bytes`. Fails when a required field is missing.
pub(crate) fn agent_card(bytes: &[u8]) -> Result<AgentCard, String> {
    let card: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("the agent's card is not valid: {e}"))?;
    parsed::<a2a_json::AgentCard>(card, "card").map(Into::into)
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
    parsed::<a2a_json::Task>(result, "task").map(Into::into)
}

/// A `SendMessageResponse` result.
pub(crate) fn send_message_response(result: Value) -> Result<SendMessageResponse, String> {
    Ok(
        match parsed::<a2a_json::SendMessageResponse>(result, "response")? {
            a2a_json::SendMessageResponse::Task(task) => SendMessageResponse::Task(task.into()),
            a2a_json::SendMessageResponse::Message(message) => {
                SendMessageResponse::Message(message.into())
            }
        },
    )
}

/// A `StreamResponse` result.
pub(crate) fn stream_response(result: Value) -> Result<StreamResponse, String> {
    parsed::<a2a_json::StreamResponse>(result, "response").map(Into::into)
}

/// A `ListTasksResponse` result. Fails on a negative page size or total.
pub(crate) fn list_tasks_response(result: Value) -> Result<ListTasksResponse, String> {
    const NAME: &str = "list of tasks";
    let response: a2a_json::ListTasksResponse = parsed(result, NAME)?;
    let count = |value: i32, field: &str| {
        u32::try_from(value)
            .map_err(|_| format!("the agent's {NAME} is not valid: {field} is negative"))
    };
    Ok(ListTasksResponse {
        tasks: response.tasks.into_iter().map(Into::into).collect(),
        next_page_token: response.next_page_token,
        page_size: count(response.page_size, "pageSize")?,
        total_size: count(response.total_size, "totalSize")?,
    })
}

// `value` as `T`. `name` identifies the value in the error.
fn parsed<T: DeserializeOwned>(value: Value, name: &str) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("the agent's {name} is not valid: {e}"))
}

/// The params of `SendMessage` and `SendStreamingMessage`. Fails on a message
/// with no id, on a `json` value that isn't JSON, or on a history length the
/// protocol can't hold.
pub(crate) fn send_message_params(
    request: &SendMessageRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    Ok(json!(a2a_json::SendMessageRequest {
        tenant: tenant.map(str::to_string),
        message: message(&request.message)?,
        configuration: request
            .configuration
            .as_ref()
            .map(configuration)
            .transpose()?,
        metadata: json_object("metadata", request.metadata.as_deref())?,
    }))
}

fn configuration(
    configuration: &SendMessageConfiguration,
) -> Result<a2a_json::SendMessageConfiguration, String> {
    Ok(a2a_json::SendMessageConfiguration {
        accepted_output_modes: configuration.accepted_output_modes.clone(),
        task_push_notification_config: None,
        history_length: int32("history-length", configuration.history_length)?,
        return_immediately: configuration.return_immediately,
    })
}

/// The params of `GetTask`. Fails on a history length the protocol can't
/// hold.
pub(crate) fn get_task_params(
    request: &GetTaskRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    Ok(json!(a2a_json::GetTaskRequest {
        tenant: tenant.map(str::to_string),
        id: request.id.clone(),
        history_length: int32("history-length", request.history_length)?,
    }))
}

/// The params of `CancelTask`. Fails on metadata that isn't a JSON object.
pub(crate) fn cancel_task_params(
    request: &CancelTaskRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    Ok(json!(a2a_json::CancelTaskRequest {
        tenant: tenant.map(str::to_string),
        id: request.id.clone(),
        metadata: json_object("metadata", request.metadata.as_deref())?,
    }))
}

/// The params of `ListTasks`. Fails on a page size or history length the
/// protocol can't hold.
pub(crate) fn list_tasks_params(
    request: &ListTasksRequest,
    tenant: Option<&str>,
) -> Result<Value, String> {
    Ok(json!(a2a_json::ListTasksRequest {
        tenant: tenant.map(str::to_string),
        context_id: request.context_id.clone(),
        status: request.status.map(task_state),
        page_size: int32("page-size", request.page_size)?,
        page_token: request.page_token.clone(),
        history_length: int32("history-length", request.history_length)?,
        status_timestamp_after: request.status_timestamp_after.clone(),
        include_artifacts: request.include_artifacts,
    }))
}

// `value` as the protocol's `int32`. `name` identifies the value in the error.
fn int32(name: &str, value: Option<u32>) -> Result<Option<i32>, String> {
    value
        .map(|value| i32::try_from(value).map_err(|_| format!("'{name}' is too large")))
        .transpose()
}

// `text`, a `json` value, as JSON. `name` identifies the value in the error.
fn json_value(name: &str, text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("'{name}' is not valid JSON: {e}"))
}

// `text`, a `json` value, as a JSON object. `name` identifies the value in
// the error.
fn json_object(name: &str, text: Option<&str>) -> Result<Option<a2a_json::Struct>, String> {
    text.map(|text| match json_value(name, text)? {
        Value::Object(object) => Ok(object),
        _ => Err(format!("'{name}' is not a JSON object")),
    })
    .transpose()
}

fn message(message: &Message) -> Result<a2a_json::Message, String> {
    if message.message_id.is_empty() {
        return Err("the message has no message-id".to_string());
    }
    Ok(a2a_json::Message {
        message_id: message.message_id.clone(),
        context_id: message.context_id.clone(),
        task_id: message.task_id.clone(),
        role: role(message.role),
        parts: message.parts.iter().map(part).collect::<Result<_, _>>()?,
        metadata: json_object("metadata", message.metadata.as_deref())?,
        extensions: message.extensions.clone(),
        reference_task_ids: message.reference_task_ids.clone(),
    })
}

fn part(part: &Part) -> Result<a2a_json::Part, String> {
    Ok(a2a_json::Part {
        content: match &part.content {
            PartContent::Text(text) => a2a_json::PartContent::Text(text.clone()),
            PartContent::Raw(raw) => a2a_json::PartContent::Raw(raw.clone()),
            PartContent::Url(url) => a2a_json::PartContent::Url(url.clone()),
            PartContent::Data(data) => a2a_json::PartContent::Data(json_value("data", data)?),
        },
        metadata: json_object("metadata", part.metadata.as_deref())?,
        filename: part.filename.clone(),
        media_type: part.media_type.clone(),
    })
}

fn role(role: Role) -> a2a_json::Role {
    match role {
        Role::Unspecified => a2a_json::Role::Unspecified,
        Role::User => a2a_json::Role::User,
        Role::Agent => a2a_json::Role::Agent,
    }
}

fn task_state(state: TaskState) -> a2a_json::TaskState {
    match state {
        TaskState::Unspecified => a2a_json::TaskState::Unspecified,
        TaskState::Submitted => a2a_json::TaskState::Submitted,
        TaskState::Working => a2a_json::TaskState::Working,
        TaskState::Completed => a2a_json::TaskState::Completed,
        TaskState::Failed => a2a_json::TaskState::Failed,
        TaskState::Canceled => a2a_json::TaskState::Canceled,
        TaskState::InputRequired => a2a_json::TaskState::InputRequired,
        TaskState::Rejected => a2a_json::TaskState::Rejected,
        TaskState::AuthRequired => a2a_json::TaskState::AuthRequired,
    }
}

// JSON text for a `json` field that holds an object.
fn json_text(object: Option<a2a_json::Struct>) -> Option<String> {
    object.map(|object| Value::Object(object).to_string())
}

impl From<a2a_json::Part> for Part {
    fn from(part: a2a_json::Part) -> Self {
        Part {
            content: match part.content {
                a2a_json::PartContent::Text(text) => PartContent::Text(text),
                a2a_json::PartContent::Raw(raw) => PartContent::Raw(raw),
                a2a_json::PartContent::Url(url) => PartContent::Url(url),
                a2a_json::PartContent::Data(data) => PartContent::Data(data.to_string()),
            },
            metadata: json_text(part.metadata),
            filename: part.filename,
            media_type: part.media_type,
        }
    }
}

impl From<a2a_json::Message> for Message {
    fn from(message: a2a_json::Message) -> Self {
        Message {
            message_id: message.message_id,
            context_id: message.context_id,
            task_id: message.task_id,
            role: message.role.into(),
            parts: message.parts.into_iter().map(Into::into).collect(),
            metadata: json_text(message.metadata),
            extensions: message.extensions,
            reference_task_ids: message.reference_task_ids,
        }
    }
}

impl From<a2a_json::Role> for Role {
    fn from(role: a2a_json::Role) -> Self {
        match role {
            a2a_json::Role::Unspecified => Role::Unspecified,
            a2a_json::Role::User => Role::User,
            a2a_json::Role::Agent => Role::Agent,
        }
    }
}

impl From<a2a_json::TaskState> for TaskState {
    fn from(state: a2a_json::TaskState) -> Self {
        match state {
            a2a_json::TaskState::Unspecified => TaskState::Unspecified,
            a2a_json::TaskState::Submitted => TaskState::Submitted,
            a2a_json::TaskState::Working => TaskState::Working,
            a2a_json::TaskState::Completed => TaskState::Completed,
            a2a_json::TaskState::Failed => TaskState::Failed,
            a2a_json::TaskState::Canceled => TaskState::Canceled,
            a2a_json::TaskState::InputRequired => TaskState::InputRequired,
            a2a_json::TaskState::Rejected => TaskState::Rejected,
            a2a_json::TaskState::AuthRequired => TaskState::AuthRequired,
        }
    }
}

impl From<a2a_json::TaskStatus> for TaskStatus {
    fn from(status: a2a_json::TaskStatus) -> Self {
        TaskStatus {
            state: status.state.into(),
            message: status.message.map(Into::into),
            timestamp: status.timestamp,
        }
    }
}

impl From<a2a_json::Artifact> for Artifact {
    fn from(artifact: a2a_json::Artifact) -> Self {
        Artifact {
            artifact_id: artifact.artifact_id,
            name: artifact.name,
            description: artifact.description,
            parts: artifact.parts.into_iter().map(Into::into).collect(),
            metadata: json_text(artifact.metadata),
            extensions: artifact.extensions,
        }
    }
}

impl From<a2a_json::Task> for Task {
    fn from(task: a2a_json::Task) -> Self {
        Task {
            id: task.id,
            context_id: task.context_id,
            status: task.status.into(),
            artifacts: task.artifacts.into_iter().map(Into::into).collect(),
            history: task.history.into_iter().map(Into::into).collect(),
            metadata: json_text(task.metadata),
        }
    }
}

impl From<a2a_json::StreamResponse> for StreamResponse {
    fn from(response: a2a_json::StreamResponse) -> Self {
        match response {
            a2a_json::StreamResponse::Task(task) => StreamResponse::Task(task.into()),
            a2a_json::StreamResponse::Message(message) => StreamResponse::Message(message.into()),
            a2a_json::StreamResponse::StatusUpdate(update) => {
                StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                    task_id: update.task_id,
                    context_id: update.context_id,
                    status: update.status.into(),
                    metadata: json_text(update.metadata),
                })
            }
            a2a_json::StreamResponse::ArtifactUpdate(update) => {
                StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                    task_id: update.task_id,
                    context_id: update.context_id,
                    artifact: update.artifact.into(),
                    append: update.append,
                    last_chunk: update.last_chunk,
                    metadata: json_text(update.metadata),
                })
            }
        }
    }
}

impl From<a2a_json::SecurityRequirement> for SecurityRequirement {
    fn from(requirement: a2a_json::SecurityRequirement) -> Self {
        SecurityRequirement {
            schemes: requirement
                .schemes
                .into_iter()
                .map(|(name, scopes)| (name, scopes.list))
                .collect(),
        }
    }
}

fn security_requirements(
    requirements: Vec<a2a_json::SecurityRequirement>,
) -> Vec<SecurityRequirement> {
    requirements.into_iter().map(Into::into).collect()
}

impl From<a2a_json::SecurityScheme> for SecurityScheme {
    fn from(scheme: a2a_json::SecurityScheme) -> Self {
        match scheme {
            a2a_json::SecurityScheme::ApiKey(scheme) => {
                SecurityScheme::ApiKeySecurityScheme(ApiKeySecurityScheme {
                    description: scheme.description,
                    location: scheme.location,
                    name: scheme.name,
                })
            }
            a2a_json::SecurityScheme::HttpAuth(scheme) => {
                SecurityScheme::HttpAuthSecurityScheme(HttpAuthSecurityScheme {
                    description: scheme.description,
                    scheme: scheme.scheme,
                    bearer_format: scheme.bearer_format,
                })
            }
            a2a_json::SecurityScheme::Oauth2(scheme) => {
                SecurityScheme::Oauth2SecurityScheme(Oauth2SecurityScheme {
                    description: scheme.description,
                    flows: scheme.flows.into(),
                    oauth2_metadata_url: scheme.oauth2_metadata_url,
                })
            }
            a2a_json::SecurityScheme::OpenIdConnect(scheme) => {
                SecurityScheme::OpenIdConnectSecurityScheme(OpenIdConnectSecurityScheme {
                    description: scheme.description,
                    open_id_connect_url: scheme.open_id_connect_url,
                })
            }
            a2a_json::SecurityScheme::MutualTls(scheme) => {
                SecurityScheme::MtlsSecurityScheme(MutualTlsSecurityScheme {
                    description: scheme.description,
                })
            }
        }
    }
}

impl From<a2a_json::OauthFlows> for OauthFlows {
    fn from(flows: a2a_json::OauthFlows) -> Self {
        match flows {
            a2a_json::OauthFlows::AuthorizationCode(flow) => {
                OauthFlows::AuthorizationCode(AuthorizationCodeOauthFlow {
                    authorization_url: flow.authorization_url,
                    token_url: flow.token_url,
                    refresh_url: flow.refresh_url,
                    scopes: flow.scopes.into_iter().collect(),
                    pkce_required: flow.pkce_required,
                })
            }
            a2a_json::OauthFlows::ClientCredentials(flow) => {
                OauthFlows::ClientCredentials(ClientCredentialsOauthFlow {
                    token_url: flow.token_url,
                    refresh_url: flow.refresh_url,
                    scopes: flow.scopes.into_iter().collect(),
                })
            }
            a2a_json::OauthFlows::Implicit(flow) => OauthFlows::Implicit(ImplicitOauthFlow {
                authorization_url: flow.authorization_url,
                refresh_url: flow.refresh_url,
                scopes: flow.scopes.into_iter().collect(),
            }),
            a2a_json::OauthFlows::Password(flow) => OauthFlows::Password(PasswordOauthFlow {
                token_url: flow.token_url,
                refresh_url: flow.refresh_url,
                scopes: flow.scopes.into_iter().collect(),
            }),
            a2a_json::OauthFlows::DeviceCode(flow) => OauthFlows::DeviceCode(DeviceCodeOauthFlow {
                device_authorization_url: flow.device_authorization_url,
                token_url: flow.token_url,
                refresh_url: flow.refresh_url,
                scopes: flow.scopes.into_iter().collect(),
            }),
        }
    }
}

impl From<a2a_json::AgentExtension> for AgentExtension {
    fn from(extension: a2a_json::AgentExtension) -> Self {
        AgentExtension {
            uri: extension.uri,
            description: extension.description,
            required: extension.required,
            params: json_text(extension.params),
        }
    }
}

impl From<a2a_json::AgentCard> for AgentCard {
    fn from(card: a2a_json::AgentCard) -> Self {
        AgentCard {
            name: card.name,
            description: card.description,
            supported_interfaces: card
                .supported_interfaces
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
                .map(|(name, scheme)| (name, scheme.into()))
                .collect(),
            security_requirements: security_requirements(card.security_requirements),
            default_input_modes: card.default_input_modes,
            default_output_modes: card.default_output_modes,
            skills: card
                .skills
                .into_iter()
                .map(|skill| AgentSkill {
                    id: skill.id,
                    name: skill.name,
                    description: skill.description,
                    tags: skill.tags,
                    examples: skill.examples,
                    input_modes: skill.input_modes,
                    output_modes: skill.output_modes,
                    security_requirements: security_requirements(skill.security_requirements),
                })
                .collect(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            agent_card(b"not json")
                .err()
                .unwrap()
                .starts_with("the agent's card is not valid")
        );
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
                format!("the agent's card is not valid: {name} must have at least one element")
            );
        }
        let mut json = card_json();
        json["skills"][0]["tags"] = json!([]);
        assert_eq!(
            card(&json).err().unwrap(),
            "the agent's card is not valid: a skill's tags must have at least one element"
        );
    }

    #[test]
    fn a_security_scheme_has_exactly_one_kind() {
        let mut json = card_json();
        json["securitySchemes"]["bearer"] = json!({});
        assert_eq!(
            card(&json).err().unwrap(),
            "the agent's card is not valid: a security scheme has none of its possible fields"
        );
        json["securitySchemes"]["bearer"] = json!({
            "httpAuthSecurityScheme": { "scheme": "Bearer" },
            "mtlsSecurityScheme": {},
        });
        assert_eq!(
            card(&json).err().unwrap(),
            "the agent's card is not valid: a security scheme has more than one of its possible fields"
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
            "the agent's response is not valid: a response has none of its possible fields"
        );
        let message =
            json!({ "messageId": "m-1", "role": "ROLE_AGENT", "parts": [{ "text": "hi" }] });
        assert_eq!(
            stream_response(json!({ "task": task_json("TASK_STATE_WORKING"), "message": message }))
                .err()
                .unwrap(),
            "the agent's response is not valid: a response has more than one of its possible fields"
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
            "the agent's response is not valid: a response has none of its possible fields"
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

    #[test]
    fn a_list_of_tasks_with_a_negative_count_is_not_valid() {
        assert_eq!(
            list_tasks_response(json!({ "tasks": [], "totalSize": -1 }))
                .err()
                .unwrap(),
            "the agent's list of tasks is not valid: totalSize is negative"
        );
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
    fn metadata_that_is_not_a_json_object_is_not_sent() {
        let mut request = SendMessageRequest {
            message: message("m-1"),
            configuration: None,
            metadata: Some("not json".to_string()),
        };
        let error = send_message_params(&request, None).err().unwrap();
        assert!(error.starts_with("'metadata' is not valid JSON"), "{error}");

        request.metadata = Some("[1]".to_string());
        assert_eq!(
            send_message_params(&request, None).err().unwrap(),
            "'metadata' is not a JSON object"
        );
    }

    #[test]
    fn a_length_the_protocol_cannot_hold_is_not_sent() {
        let request = GetTaskRequest {
            id: "task-1".to_string(),
            history_length: Some(u32::MAX),
        };
        assert_eq!(
            get_task_params(&request, None).err().unwrap(),
            "'history-length' is too large"
        );
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
            list_tasks_params(&request, None).unwrap(),
            json!({
                "contextId": "ctx-1",
                "status": "TASK_STATE_INPUT_REQUIRED",
                "pageSize": 10,
                "includeArtifacts": true,
            })
        );
    }
}
