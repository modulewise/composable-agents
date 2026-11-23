//! Parses `[server.*]` definitions where `type = "a2a"`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use composable_runtime::{
    CategoryClaim, Condition, ConfigHandler, Definition, GenericDefinition, Operator, PropertyMap,
    Selector,
};
use serde_json::Value;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 4001;
const DEFAULT_OTLP_PROTOCOL: &str = "grpc";

/// The OTLP protocols a server can export spans with.
pub const OTLP_PROTOCOLS: [&str; 2] = ["grpc", "http/protobuf"];

/// A parsed A2A server definition.
#[derive(Debug, Clone)]
pub struct A2aServerConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Selects the agent the server serves. `None` selects every agent.
    pub agent_selector: Option<Selector>,
    /// The URL of the server's JSON-RPC interface in the agent's card. `None`
    /// is `http://<host>:<port>`.
    pub url: Option<String>,
    /// The OTLP endpoint the server exports its spans to. `None` exports no
    /// spans.
    pub otlp_endpoint: Option<String>,
    /// The OTLP protocol, `grpc` or `http/protobuf`.
    pub otlp_protocol: String,
}

impl A2aServerConfig {
    /// The URL of the server's JSON-RPC interface.
    pub fn interface_url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| format!("http://{}:{}", self.host, self.port))
    }
}

pub type SharedConfig = Arc<Mutex<Vec<A2aServerConfig>>>;

/// The server that runs when no A2A server is defined.
pub fn default_server() -> A2aServerConfig {
    A2aServerConfig {
        name: "a2a".to_string(),
        host: DEFAULT_HOST.to_string(),
        port: DEFAULT_PORT,
        agent_selector: None,
        url: None,
        otlp_endpoint: None,
        otlp_protocol: DEFAULT_OTLP_PROTOCOL.to_string(),
    }
}

/// Claims `[server.*]` definitions where `type = "a2a"`.
pub struct A2aServerConfigHandler {
    servers: SharedConfig,
}

impl A2aServerConfigHandler {
    pub fn new(servers: SharedConfig) -> Self {
        Self { servers }
    }
}

impl ConfigHandler for A2aServerConfigHandler {
    fn claimed_categories(&self) -> Vec<CategoryClaim> {
        vec![CategoryClaim::with_selector(
            "server",
            Selector {
                conditions: vec![Condition {
                    key: "type".to_string(),
                    operator: Operator::Equals("a2a".to_string()),
                }],
            },
        )]
    }

    fn claimed_properties(&self) -> HashMap<&str, &[&str]> {
        HashMap::from([(
            "server",
            [
                "type",
                "host",
                "port",
                "agent-selector",
                "url",
                "otlp-endpoint",
                "otlp-protocol",
            ]
            .as_slice(),
        )])
    }

    fn handle_definition(&mut self, definition: GenericDefinition) -> Result<Vec<Definition>> {
        let GenericDefinition {
            category,
            name,
            mut properties,
        } = definition;
        if category != "server" {
            bail!("A2aServerConfigHandler received unexpected category '{category}'");
        }
        // `type` is used only by the claim's selector.
        properties.remove("type");

        let port = match properties.remove("port") {
            Some(Value::Number(port)) => port
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| anyhow!("Server '{name}': 'port' must be a valid port number"))?,
            Some(got) => bail!("Server '{name}': 'port' must be a number, got {got}"),
            None => bail!("Server '{name}' missing required 'port' field"),
        };
        let host = optional_string(&name, "host", &mut properties)?
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        let agent_selector = optional_string(&name, "agent-selector", &mut properties)?
            .map(|selector| {
                Selector::parse(&selector).map_err(|e| {
                    anyhow!("Server '{name}': invalid agent-selector '{selector}': {e}")
                })
            })
            .transpose()?;
        let url = optional_string(&name, "url", &mut properties)?;
        let otlp_endpoint = optional_string(&name, "otlp-endpoint", &mut properties)?;
        let otlp_protocol = optional_string(&name, "otlp-protocol", &mut properties)?
            .unwrap_or_else(|| DEFAULT_OTLP_PROTOCOL.to_string());
        if !OTLP_PROTOCOLS.contains(&otlp_protocol.as_str()) {
            bail!(
                "Server '{name}': 'otlp-protocol' must be 'grpc' or 'http/protobuf', got '{otlp_protocol}'"
            );
        }

        if !properties.is_empty() {
            let unknown: Vec<_> = properties.keys().collect();
            bail!("Server '{name}' has unknown properties: {unknown:?}");
        }
        self.servers.lock().unwrap().push(A2aServerConfig {
            name,
            host,
            port,
            agent_selector,
            url,
            otlp_endpoint,
            otlp_protocol,
        });
        Ok(Vec::new())
    }
}

fn optional_string(
    server: &str,
    key: &str,
    properties: &mut PropertyMap,
) -> Result<Option<String>> {
    match properties.remove(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(got) => bail!("Server '{server}': '{key}' must be a string, got {got}"),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn handled(properties: Value) -> Result<Vec<A2aServerConfig>> {
        let servers = SharedConfig::default();
        let mut handler = A2aServerConfigHandler::new(servers.clone());
        let properties = properties
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        handler.handle_definition(GenericDefinition {
            category: "server".to_string(),
            name: "agents".to_string(),
            properties,
        })?;
        let servers = servers.lock().unwrap().clone();
        Ok(servers)
    }

    #[test]
    fn a_server_has_its_port_and_the_defaults() {
        let servers = handled(json!({ "type": "a2a", "port": 4002 })).unwrap();
        let server = &servers[0];
        assert_eq!(server.name, "agents");
        assert_eq!((server.host.as_str(), server.port), ("127.0.0.1", 4002));
        assert!(server.agent_selector.is_none());
        assert_eq!(server.interface_url(), "http://127.0.0.1:4002");
        assert!(server.otlp_endpoint.is_none());
        assert_eq!(server.otlp_protocol, "grpc");
    }

    #[test]
    fn a_server_has_its_host_selector_url_and_otlp_settings_when_set() {
        let servers = handled(json!({
            "type": "a2a",
            "port": 4002,
            "host": "0.0.0.0",
            "agent-selector": "name=assistant",
            "url": "https://agents.example.com/assistant",
            "otlp-endpoint": "http://localhost:4318/v1/traces",
            "otlp-protocol": "http/protobuf",
        }))
        .unwrap();
        let server = &servers[0];
        assert_eq!(server.host, "0.0.0.0");
        assert!(server.agent_selector.is_some());
        assert_eq!(
            server.interface_url(),
            "https://agents.example.com/assistant"
        );
        assert_eq!(
            server.otlp_endpoint.as_deref(),
            Some("http://localhost:4318/v1/traces")
        );
        assert_eq!(server.otlp_protocol, "http/protobuf");
    }

    #[test]
    fn a_definition_that_is_not_valid_is_an_error() {
        for (properties, expected) in [
            (json!({ "type": "a2a" }), "missing required 'port' field"),
            (
                json!({ "type": "a2a", "port": 70000 }),
                "'port' must be a valid port number",
            ),
            (
                json!({ "type": "a2a", "port": "4002" }),
                "'port' must be a number",
            ),
            (
                json!({ "type": "a2a", "port": 4002, "host": 1 }),
                "'host' must be a string",
            ),
            (
                json!({ "type": "a2a", "port": 4002, "agent": "assistant" }),
                "unknown properties",
            ),
            (
                json!({ "type": "a2a", "port": 4002, "otlp-protocol": "http/json" }),
                "'otlp-protocol' must be 'grpc' or 'http/protobuf', got 'http/json'",
            ),
        ] {
            let error = handled(properties).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
    }
}
