//! The `[model]` and `[agent]` config handlers.
//!
//! A model is a model-client component, chosen by its provider. An agent is a
//! chat-agent component importing its model (until other agent types land).

use std::collections::HashMap;

use anyhow::{Result, bail};
use composable_runtime::{
    CategoryClaim, ConfigHandler, Definition, GenericDefinition, PropertyMap,
};
use serde_json::{Map, Value, json};

/// Handles each message by making one call to its model.
const CHAT_AGENT: &str = "oci://localhost:5001/composable-agents/chat-agent:latest";

/// The model client for the Chat Completions API.
const CHAT_COMPLETIONS_CLIENT: &str =
    "oci://localhost:5001/composable-agents/chat-completions-client:latest";

/// The model client for Anthropic's Messages API.
const ANTHROPIC_MESSAGES_CLIENT: &str =
    "oci://localhost:5001/composable-agents/anthropic-messages-client:latest";

/// The HTTP client every model client imports.
const HTTP_CLIENT: &str = "oci://ghcr.io/componentized/http/client:0.1.0";

/// The shared HTTP client component and the capability it imports.
const HTTP_CLIENT_NAME: &str = "_http-client";
const HTTP_CAPABILITY_NAME: &str = "_http";

/// The shared random capability agents import for ids.
const RANDOM_CAPABILITY_NAME: &str = "_random";

/// The shared clocks capability, imported by the Messages API client for retry
/// waits.
const CLOCKS_CAPABILITY_NAME: &str = "_clocks";

/// The providers a `[model]` may name.
const PROVIDERS: [&str; 3] = ["anthropic", "ollama", "openai"];

/// A provider's endpoint, when it differs from the client's default.
const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

/// A provider's API key, read from the environment when the config resolves.
const ANTHROPIC_API_KEY: &str = "${process:env:ANTHROPIC_API_KEY}";
const OPENAI_API_KEY: &str = "${process:env:OPENAI_API_KEY}";

/// Handles `[model.NAME]` and `[agent.NAME]`, each of which becomes a
/// component named `NAME`.
#[derive(Default)]
pub(crate) struct AgentConfigHandler {
    http_emitted: bool,
    random_emitted: bool,
    clocks_emitted: bool,
}

impl ConfigHandler for AgentConfigHandler {
    fn claimed_categories(&self) -> Vec<CategoryClaim> {
        vec![CategoryClaim::all("model"), CategoryClaim::all("agent")]
    }

    fn claimed_properties(&self) -> HashMap<&str, &[&str]> {
        HashMap::from([
            (
                "model",
                [
                    "provider",
                    "name",
                    "max-tokens",
                    "effort",
                    "include-reasoning",
                    "max-retries",
                ]
                .as_slice(),
            ),
            (
                "agent",
                ["model", "instructions", "description", "version"].as_slice(),
            ),
        ])
    }

    fn handle_definition(&mut self, definition: GenericDefinition) -> Result<Vec<Definition>> {
        let GenericDefinition {
            category,
            name,
            mut properties,
        } = definition;
        match category.as_str() {
            "model" => self.model(&name, &mut properties),
            "agent" => self.agent(&name, &mut properties),
            other => bail!("AgentConfigHandler received unexpected category '{other}'"),
        }
    }
}

impl AgentConfigHandler {
    fn model(&mut self, name: &str, properties: &mut PropertyMap) -> Result<Vec<Definition>> {
        let context = format!("Model '{name}'");
        let provider = required_string(&context, "provider", properties)?;
        let model = required_string(&context, "name", properties)?;

        let mut config = Map::from_iter([
            ("model".to_string(), json!(model)),
            ("provider".to_string(), json!(provider)),
        ]);
        if let Some(max_tokens) = optional_u32(&context, "max-tokens", properties)? {
            config.insert("max-tokens".to_string(), json!(max_tokens));
        }
        // Each provider's client, base URL (None for the client's default) and
        // API key.
        let (uri, base_url, api_key) = match provider.as_str() {
            "anthropic" => (ANTHROPIC_MESSAGES_CLIENT, None, Some(ANTHROPIC_API_KEY)),
            "ollama" => (CHAT_COMPLETIONS_CLIENT, Some(OLLAMA_BASE_URL), None),
            "openai" => (CHAT_COMPLETIONS_CLIENT, None, Some(OPENAI_API_KEY)),
            other => bail!(
                "{context}: unknown provider '{other}'. Supported: {}",
                PROVIDERS.join(", ")
            ),
        };
        // `effort`, `include-reasoning` and `max-retries` are Messages API client
        // settings, rejected for other providers. The client checks `effort`'s
        // value when a session starts.
        let messages_api = uri == ANTHROPIC_MESSAGES_CLIENT;
        if let Some(effort) = optional_string(&context, "effort", properties)? {
            if !messages_api {
                bail!(
                    "{context}: 'effort' is only supported for provider 'anthropic', not '{provider}'"
                );
            }
            config.insert("effort".to_string(), json!(effort));
        }
        if let Some(include_reasoning) = optional_bool(&context, "include-reasoning", properties)? {
            if !messages_api {
                bail!(
                    "{context}: 'include-reasoning' is only supported for provider 'anthropic', not '{provider}'"
                );
            }
            config.insert("include-reasoning".to_string(), json!(include_reasoning));
        }
        if let Some(max_retries) = optional_u32(&context, "max-retries", properties)? {
            if !messages_api {
                bail!(
                    "{context}: 'max-retries' is only supported for provider 'anthropic', not '{provider}'"
                );
            }
            config.insert("max-retries".to_string(), json!(max_retries));
        }
        if let Some(base_url) = base_url {
            config.insert("base-url".to_string(), json!(base_url));
        }
        if let Some(api_key) = api_key {
            config.insert("api-key".to_string(), json!(api_key));
        }
        // Ollama reads only `max_tokens`.
        if provider == "ollama" {
            config.insert("max-tokens-field".to_string(), json!("max_tokens"));
        }

        // Only the Messages API client imports a clock.
        let mut imports = vec![HTTP_CLIENT_NAME];
        if messages_api {
            imports.push(CLOCKS_CAPABILITY_NAME);
        }
        let mut definitions = vec![component(
            name,
            json!({
                "uri": uri,
                "imports": imports,
                "config": config,
            }),
        )];
        definitions.extend(self.http_wiring());
        if messages_api {
            definitions.extend(self.clocks_wiring());
        }
        Ok(definitions)
    }

    fn agent(&mut self, name: &str, properties: &mut PropertyMap) -> Result<Vec<Definition>> {
        let context = format!("Agent '{name}'");
        let model = required_string(&context, "model", properties)?;
        let description = required_string(&context, "description", properties)?;
        let version = required_string(&context, "version", properties)?;

        // The agent's `metadata()` reads its name, description, and version,
        // each of which `agent-metadata` requires.
        let mut config = Map::from_iter([
            ("name".to_string(), json!(name)),
            ("description".to_string(), json!(description)),
            ("version".to_string(), json!(version)),
        ]);
        if let Some(instructions) = optional_string(&context, "instructions", properties)? {
            config.insert("instructions".to_string(), json!(instructions));
        }
        let mut definitions = vec![component(
            name,
            json!({
                "uri": CHAT_AGENT,
                "imports": [model, RANDOM_CAPABILITY_NAME],
                "config": config,
            }),
        )];
        definitions.extend(self.random_wiring());
        Ok(definitions)
    }

    // The shared random capability, unless already emitted.
    fn random_wiring(&mut self) -> Vec<Definition> {
        if self.random_emitted {
            return Vec::new();
        }
        self.random_emitted = true;
        vec![generic(
            "capability",
            RANDOM_CAPABILITY_NAME,
            json!({ "type": "wasi:random" }),
        )]
    }

    // The shared clocks capability, unless already emitted.
    fn clocks_wiring(&mut self) -> Vec<Definition> {
        if self.clocks_emitted {
            return Vec::new();
        }
        self.clocks_emitted = true;
        vec![generic(
            "capability",
            CLOCKS_CAPABILITY_NAME,
            json!({ "type": "wasi:clocks" }),
        )]
    }

    // The shared HTTP client and its capability, unless already emitted.
    fn http_wiring(&mut self) -> Vec<Definition> {
        if self.http_emitted {
            return Vec::new();
        }
        self.http_emitted = true;
        vec![
            component(
                HTTP_CLIENT_NAME,
                json!({ "uri": HTTP_CLIENT, "imports": [HTTP_CAPABILITY_NAME] }),
            ),
            generic(
                "capability",
                HTTP_CAPABILITY_NAME,
                json!({ "type": "wasi:http" }),
            ),
        ]
    }
}

fn required_string(context: &str, key: &str, properties: &mut PropertyMap) -> Result<String> {
    optional_string(context, key, properties)?
        .ok_or_else(|| anyhow::anyhow!("{context} is missing required '{key}'"))
}

fn optional_string(
    context: &str,
    key: &str,
    properties: &mut PropertyMap,
) -> Result<Option<String>> {
    match properties.remove(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(got) => bail!("{context}: '{key}' must be a string, got {got}"),
        None => Ok(None),
    }
}

fn optional_bool(context: &str, key: &str, properties: &mut PropertyMap) -> Result<Option<bool>> {
    match properties.remove(key) {
        Some(Value::Bool(value)) => Ok(Some(value)),
        Some(got) => bail!("{context}: '{key}' must be true or false, got {got}"),
        None => Ok(None),
    }
}

fn optional_u32(context: &str, key: &str, properties: &mut PropertyMap) -> Result<Option<u32>> {
    match properties.remove(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("{context}: '{key}' must be a positive integer")),
        Some(got) => bail!("{context}: '{key}' must be a number, got {got}"),
        None => Ok(None),
    }
}

/// A `[component.NAME]` definition, for the core component handler.
fn component(name: &str, properties: Value) -> Definition {
    generic("component", name, properties)
}

/// A definition for the handler that claims `category`.
fn generic(category: &str, name: &str, properties: Value) -> Definition {
    let Value::Object(properties) = properties else {
        unreachable!("definition properties are an object");
    };
    Definition::Generic(GenericDefinition {
        category: category.to_string(),
        name: name.to_string(),
        properties: properties.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(category: &str, name: &str, properties: Value) -> GenericDefinition {
        let Value::Object(properties) = properties else {
            panic!("properties must be an object");
        };
        GenericDefinition {
            category: category.to_string(),
            name: name.to_string(),
            properties: properties.into_iter().collect(),
        }
    }

    /// The definitions produced, by category and name.
    fn produced(definitions: Vec<Definition>) -> HashMap<(String, String), Value> {
        definitions
            .into_iter()
            .map(|produced| match produced {
                Definition::Generic(definition) => (
                    (definition.category, definition.name),
                    Value::Object(definition.properties.into_iter().collect()),
                ),
                other => panic!("expected a generic definition, got {other:?}"),
            })
            .collect()
    }

    fn key(category: &str, name: &str) -> (String, String) {
        (category.to_string(), name.to_string())
    }

    #[test]
    fn an_ollama_model_builds_a_chat_completions_client() {
        let mut handler = AgentConfigHandler::default();
        let definitions = handler
            .handle_definition(definition(
                "model",
                "local",
                json!({ "provider": "ollama", "name": "gpt-oss:20b", "max-tokens": 1024 }),
            ))
            .unwrap();
        let produced = produced(definitions);

        assert_eq!(
            produced[&key("component", "local")],
            json!({
                "uri": CHAT_COMPLETIONS_CLIENT,
                "imports": ["_http-client"],
                "config": {
                    "model": "gpt-oss:20b",
                    "provider": "ollama",
                    "max-tokens": 1024,
                    "base-url": OLLAMA_BASE_URL,
                    "max-tokens-field": "max_tokens",
                },
            })
        );
        assert_eq!(
            produced[&key("component", "_http-client")],
            json!({ "uri": HTTP_CLIENT, "imports": ["_http"] })
        );
        assert_eq!(
            produced[&key("capability", "_http")],
            json!({ "type": "wasi:http" })
        );
        assert_eq!(produced.len(), 3);
    }

    #[test]
    fn the_http_wiring_is_shared_by_all_models() {
        let mut handler = AgentConfigHandler::default();
        handler
            .handle_definition(definition(
                "model",
                "local",
                json!({ "provider": "ollama", "name": "gpt-oss:20b" }),
            ))
            .unwrap();
        let second = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "hosted",
                    json!({ "provider": "openai", "name": "gpt-5.4-mini" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            second.keys().collect::<Vec<_>>(),
            [&key("component", "hosted")]
        );
    }

    #[test]
    fn an_openai_model_reads_its_key_from_the_environment() {
        let mut handler = AgentConfigHandler::default();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "hosted",
                    json!({ "provider": "openai", "name": "gpt-5.4-mini" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            produced[&key("component", "hosted")]["config"],
            json!({ "model": "gpt-5.4-mini", "provider": "openai", "api-key": OPENAI_API_KEY })
        );
    }

    #[test]
    fn an_anthropic_model_builds_a_messages_api_client() {
        let mut handler = AgentConfigHandler::default();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "claude",
                    json!({
                        "provider": "anthropic",
                        "name": "claude-test",
                        "max-tokens": 8192,
                        "effort": "low",
                        "include-reasoning": true,
                        "max-retries": 4,
                    }),
                ))
                .unwrap(),
        );
        assert_eq!(
            produced[&key("component", "claude")],
            json!({
                "uri": ANTHROPIC_MESSAGES_CLIENT,
                "imports": ["_http-client", "_clocks"],
                "config": {
                    "model": "claude-test",
                    "provider": "anthropic",
                    "max-tokens": 8192,
                    "effort": "low",
                    "include-reasoning": true,
                    "max-retries": 4,
                    "api-key": ANTHROPIC_API_KEY,
                },
            })
        );
        assert_eq!(
            produced[&key("capability", "_clocks")],
            json!({ "type": "wasi:clocks" })
        );
    }

    #[test]
    fn only_a_messages_api_model_imports_the_clocks_capability_emitted_once() {
        let mut handler = AgentConfigHandler::default();
        let local = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "local",
                    json!({ "provider": "ollama", "name": "gpt-oss:20b" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            local[&key("component", "local")]["imports"],
            json!(["_http-client"])
        );
        assert!(!local.contains_key(&key("capability", "_clocks")));

        let first = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "claude",
                    json!({ "provider": "anthropic", "name": "claude-test" }),
                ))
                .unwrap(),
        );
        assert!(first.contains_key(&key("capability", "_clocks")));
        let second = produced(
            handler
                .handle_definition(definition(
                    "model",
                    "claude-2",
                    json!({ "provider": "anthropic", "name": "claude-test" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            second.keys().collect::<Vec<_>>(),
            [&key("component", "claude-2")]
        );
        assert_eq!(
            second[&key("component", "claude-2")]["imports"],
            json!(["_http-client", "_clocks"])
        );
    }

    #[test]
    fn messages_api_settings_are_rejected_for_a_model_without_that_client() {
        for property in ["effort", "include-reasoning", "max-retries"] {
            for provider in ["ollama", "openai"] {
                let value = match property {
                    "effort" => json!("low"),
                    "include-reasoning" => json!(true),
                    _ => json!(3),
                };
                let mut properties = json!({ "provider": provider, "name": "m" });
                properties[property] = value;
                let mut handler = AgentConfigHandler::default();
                let error = handler
                    .handle_definition(definition("model", "other", properties))
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains(&format!(
                        "'{property}' is only supported for provider 'anthropic', not '{provider}'"
                    )),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_provider_is_rejected_with_supported_list_in_error() {
        let mut handler = AgentConfigHandler::default();
        let error = handler
            .handle_definition(definition(
                "model",
                "other",
                json!({ "provider": "acme", "name": "m" }),
            ))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown provider 'acme'"), "{error}");
        assert!(error.contains("anthropic, ollama, openai"), "{error}");
    }

    #[test]
    fn an_agent_is_a_chat_agent_importing_its_model() {
        let mut handler = AgentConfigHandler::default();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "agent",
                    "helper",
                    json!({
                        "model": "local",
                        "instructions": "Help.",
                        "description": "Helps",
                        "version": "1.0.0",
                    }),
                ))
                .unwrap(),
        );
        assert_eq!(
            produced[&key("component", "helper")],
            json!({
                "uri": CHAT_AGENT,
                "imports": ["local", "_random"],
                "config": {
                    "name": "helper",
                    "instructions": "Help.",
                    "description": "Helps",
                    "version": "1.0.0",
                },
            })
        );
        assert_eq!(
            produced[&key("capability", "_random")],
            json!({ "type": "wasi:random" })
        );
        assert_eq!(produced.len(), 2);
    }

    #[test]
    fn the_random_capability_is_shared_by_all_agents() {
        let mut handler = AgentConfigHandler::default();
        let properties = json!({ "model": "local", "description": "Helps", "version": "1.0.0" });
        handler
            .handle_definition(definition("agent", "first", properties.clone()))
            .unwrap();
        let second = produced(
            handler
                .handle_definition(definition("agent", "second", properties))
                .unwrap(),
        );
        assert_eq!(
            second.keys().collect::<Vec<_>>(),
            [&key("component", "second")]
        );
    }

    #[test]
    fn an_agent_requires_a_model() {
        let mut handler = AgentConfigHandler::default();
        let error = handler
            .handle_definition(definition("agent", "helper", json!({})))
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing required 'model'"), "{error}");
    }

    #[test]
    fn an_agent_requires_a_description_and_version() {
        let mut handler = AgentConfigHandler::default();
        let error = handler
            .handle_definition(definition(
                "agent",
                "helper",
                json!({ "model": "local", "version": "1.0.0" }),
            ))
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing required 'description'"), "{error}");

        let error = handler
            .handle_definition(definition(
                "agent",
                "helper",
                json!({ "model": "local", "description": "Helps" }),
            ))
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing required 'version'"), "{error}");
    }
}
