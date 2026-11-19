//! The agent card and the types it contains.

use std::collections::BTreeMap;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::{Struct, at_least_one, is_false, one_of};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    /// At least one, in the agent's order of preference.
    #[serde(deserialize_with = "interfaces")]
    pub supported_interfaces: Vec<AgentInterface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    pub capabilities: AgentCapabilities,
    /// The security schemes, each by its name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub security_schemes: BTreeMap<String, SecurityScheme>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_requirements: Vec<SecurityRequirement>,
    /// At least one media type.
    #[serde(deserialize_with = "input_modes")]
    pub default_input_modes: Vec<String>,
    /// At least one media type.
    #[serde(deserialize_with = "output_modes")]
    pub default_output_modes: Vec<String>,
    /// At least one.
    #[serde(deserialize_with = "skills")]
    pub skills: Vec<AgentSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<AgentCardSignature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    /// E.g. `JSONRPC`, `GRPC` or `HTTP+JSON`.
    pub protocol_binding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProvider {
    pub url: String,
    pub organization: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AgentExtension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentExtension {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether a client must use the extension.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Struct>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    /// At least one.
    #[serde(deserialize_with = "tags")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_requirements: Vec<SecurityRequirement>,
}

/// A JWS signature of the card (RFC 7515's JSON form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCardSignature {
    pub protected: String,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Struct>,
}

/// The schemes a request must satisfy, each by its name with the scopes it
/// requires.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SecurityRequirement {
    #[serde(default)]
    pub schemes: BTreeMap<String, StringList>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StringList {
    #[serde(default)]
    pub list: Vec<String>,
}

/// A security scheme, which is exactly one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SecuritySchemeFields", into = "SecuritySchemeFields")]
pub enum SecurityScheme {
    ApiKey(ApiKeySecurityScheme),
    HttpAuth(HttpAuthSecurityScheme),
    Oauth2(Oauth2SecurityScheme),
    OpenIdConnect(OpenIdConnectSecurityScheme),
    MutualTls(MutualTlsSecurityScheme),
}

// A security scheme as its JSON fields.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecuritySchemeFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_security_scheme: Option<ApiKeySecurityScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_auth_security_scheme: Option<HttpAuthSecurityScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth2_security_scheme: Option<Oauth2SecurityScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_id_connect_security_scheme: Option<OpenIdConnectSecurityScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtls_security_scheme: Option<MutualTlsSecurityScheme>,
}

impl TryFrom<SecuritySchemeFields> for SecurityScheme {
    type Error = String;

    fn try_from(fields: SecuritySchemeFields) -> Result<Self, String> {
        one_of(
            [
                fields.api_key_security_scheme.map(SecurityScheme::ApiKey),
                fields
                    .http_auth_security_scheme
                    .map(SecurityScheme::HttpAuth),
                fields.oauth2_security_scheme.map(SecurityScheme::Oauth2),
                fields
                    .open_id_connect_security_scheme
                    .map(SecurityScheme::OpenIdConnect),
                fields.mtls_security_scheme.map(SecurityScheme::MutualTls),
            ],
            "a security scheme",
        )
    }
}

impl From<SecurityScheme> for SecuritySchemeFields {
    fn from(scheme: SecurityScheme) -> Self {
        let mut fields = SecuritySchemeFields::default();
        match scheme {
            SecurityScheme::ApiKey(scheme) => fields.api_key_security_scheme = Some(scheme),
            SecurityScheme::HttpAuth(scheme) => fields.http_auth_security_scheme = Some(scheme),
            SecurityScheme::Oauth2(scheme) => fields.oauth2_security_scheme = Some(scheme),
            SecurityScheme::OpenIdConnect(scheme) => {
                fields.open_id_connect_security_scheme = Some(scheme)
            }
            SecurityScheme::MutualTls(scheme) => fields.mtls_security_scheme = Some(scheme),
        }
        fields
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKeySecurityScheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the key is sent: `query`, `header` or `cookie`.
    pub location: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpAuthSecurityScheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The HTTP authentication scheme, e.g. `Bearer`.
    pub scheme: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Oauth2SecurityScheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub flows: OauthFlows,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth2_metadata_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenIdConnectSecurityScheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub open_id_connect_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MutualTlsSecurityScheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An OAuth 2.0 scheme's flow, which is exactly one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "OauthFlowsFields", into = "OauthFlowsFields")]
pub enum OauthFlows {
    AuthorizationCode(AuthorizationCodeOauthFlow),
    ClientCredentials(ClientCredentialsOauthFlow),
    Implicit(ImplicitOauthFlow),
    Password(PasswordOauthFlow),
    DeviceCode(DeviceCodeOauthFlow),
}

// A scheme's flow as its JSON fields.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthFlowsFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    authorization_code: Option<AuthorizationCodeOauthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_credentials: Option<ClientCredentialsOauthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    implicit: Option<ImplicitOauthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<PasswordOauthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_code: Option<DeviceCodeOauthFlow>,
}

impl TryFrom<OauthFlowsFields> for OauthFlows {
    type Error = String;

    fn try_from(fields: OauthFlowsFields) -> Result<Self, String> {
        one_of(
            [
                fields.authorization_code.map(OauthFlows::AuthorizationCode),
                fields.client_credentials.map(OauthFlows::ClientCredentials),
                fields.implicit.map(OauthFlows::Implicit),
                fields.password.map(OauthFlows::Password),
                fields.device_code.map(OauthFlows::DeviceCode),
            ],
            "a security scheme's flows",
        )
    }
}

impl From<OauthFlows> for OauthFlowsFields {
    fn from(flows: OauthFlows) -> Self {
        let mut fields = OauthFlowsFields::default();
        match flows {
            OauthFlows::AuthorizationCode(flow) => fields.authorization_code = Some(flow),
            OauthFlows::ClientCredentials(flow) => fields.client_credentials = Some(flow),
            OauthFlows::Implicit(flow) => fields.implicit = Some(flow),
            OauthFlows::Password(flow) => fields.password = Some(flow),
            OauthFlows::DeviceCode(flow) => fields.device_code = Some(flow),
        }
        fields
    }
}

// A flow's scopes may be missing when read, because ProtoJSON can leave out
// an empty map. Each scope's name is the key and its description the value.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationCodeOauthFlow {
    pub authorization_url: String,
    pub token_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pkce_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCredentialsOauthFlow {
    pub token_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplicitOauthFlow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordOauthFlow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeOauthFlow {
    pub device_authorization_url: String,
    pub token_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: BTreeMap<String, String>,
}

fn interfaces<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<AgentInterface>, D::Error> {
    at_least_one(deserializer, "a card's interfaces")
}

fn input_modes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    at_least_one(deserializer, "a card's input modes")
}

fn output_modes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    at_least_one(deserializer, "a card's output modes")
}

fn skills<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<AgentSkill>, D::Error> {
    at_least_one(deserializer, "a card's skills")
}

fn tags<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    at_least_one(deserializer, "a skill's tags")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    fn card_json() -> Value {
        json!({
            "name": "assistant",
            "description": "Answers questions concisely",
            "supportedInterfaces": [{
                "url": "https://example.com/a2a",
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0",
            }],
            "version": "0.1.0",
            "capabilities": { "streaming": true },
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": [{
                "id": "assistant",
                "name": "assistant",
                "description": "Answers questions concisely",
                "tags": ["chat"],
            }],
        })
    }

    #[test]
    fn a_card_is_read_and_written_without_its_unset_fields() {
        let card: AgentCard = read(card_json()).unwrap();
        assert_eq!(card.capabilities.streaming, Some(true));
        assert_eq!(card.capabilities.push_notifications, None);
        assert!(card.security_schemes.is_empty());
        assert_eq!(json!(card), card_json());
    }

    #[test]
    fn a_card_without_a_required_field_is_not_read() {
        for field in ["name", "supportedInterfaces", "capabilities", "skills"] {
            let mut json = card_json();
            json.as_object_mut().unwrap().remove(field);
            let error = read::<AgentCard>(json).unwrap_err();
            assert!(
                error.contains(&format!("missing field `{field}`")),
                "{error}"
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
                read::<AgentCard>(json).unwrap_err(),
                format!("{name} must have at least one element")
            );
        }
        let mut json = card_json();
        json["skills"][0]["tags"] = json!([]);
        assert_eq!(
            read::<AgentCard>(json).unwrap_err(),
            "a skill's tags must have at least one element"
        );
    }

    #[test]
    fn a_security_scheme_is_read_and_written_as_its_one_kind() {
        let json = json!({
            "oauth2SecurityScheme": {
                "flows": {
                    "authorizationCode": {
                        "authorizationUrl": "https://example.com/authorize",
                        "tokenUrl": "https://example.com/token",
                        "scopes": { "read": "Read tasks" },
                        "pkceRequired": true,
                    },
                },
            },
        });
        let scheme: SecurityScheme = read(json.clone()).unwrap();
        let SecurityScheme::Oauth2(oauth2) = &scheme else {
            panic!("expected an OAuth 2.0 scheme");
        };
        let OauthFlows::AuthorizationCode(flow) = &oauth2.flows else {
            panic!("expected the authorization code flow");
        };
        assert!(flow.pkce_required);
        assert_eq!(flow.scopes["read"], "Read tasks");
        assert_eq!(json!(scheme), json);
    }

    #[test]
    fn a_security_scheme_has_exactly_one_kind() {
        assert_eq!(
            read::<SecurityScheme>(json!({})).unwrap_err(),
            "a security scheme has none of its possible fields"
        );
        assert_eq!(
            read::<SecurityScheme>(json!({
                "mtlsSecurityScheme": {},
                "httpAuthSecurityScheme": { "scheme": "Bearer" },
            }))
            .unwrap_err(),
            "a security scheme has more than one of its possible fields"
        );
    }

    #[test]
    fn a_security_requirement_lists_the_scopes_of_each_scheme() {
        let json = json!({ "schemes": { "oauth": { "list": ["read"] } } });
        let requirement: SecurityRequirement = read(json.clone()).unwrap();
        assert_eq!(requirement.schemes["oauth"].list, ["read"]);
        assert_eq!(json!(requirement), json);
    }
}
