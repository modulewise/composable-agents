//! The `[model]`, `[agent]`, and `[a2a-tool]` config handlers.
//!
//! A model is a model-client component, chosen by its provider. An agent with
//! a toolset is a tool-calling-agent component importing its model and
//! toolset, and an agent without a toolset is a chat-agent component importing
//! its model. An A2A tool is an a2a-tool component importing the shared A2A
//! client.

use std::collections::HashMap;

use anyhow::{Result, bail};
use composable_runtime::{
    CategoryClaim, ConfigHandler, Definition, GenericDefinition, PropertyMap,
};
use serde_json::{Map, Value, json};

/// Handles each message by making one call to its model.
const CHAT_AGENT: &str = "oci://localhost:5001/composable-agents/chat-agent:latest";

/// Handles each message by calling its model in a loop, running the tools each
/// response requests from its toolset.
const TOOL_CALLING_AGENT: &str = "oci://localhost:5001/composable-agents/tool-calling-agent:latest";

/// The model client for the Chat Completions API.
const CHAT_COMPLETIONS_CLIENT: &str =
    "oci://localhost:5001/composable-agents/chat-completions-client:latest";

/// The model client for Anthropic's Messages API.
const ANTHROPIC_MESSAGES_CLIENT: &str =
    "oci://localhost:5001/composable-agents/anthropic-messages-client:latest";

/// Calls one agent on an A2A server, as a tool.
const A2A_TOOL: &str = "oci://localhost:5001/composable-agents/a2a-tool:latest";

/// The A2A client every A2A tool imports.
const A2A_CLIENT: &str = "oci://localhost:5001/composable-agents/a2a-client:latest";

/// The HTTP client every model client and the A2A client import.
const HTTP_CLIENT: &str = "oci://ghcr.io/componentized/http/client:0.1.0";

/// The shared A2A client component.
const A2A_CLIENT_NAME: &str = "_a2a-client";

/// The shared HTTP client component and the capability it imports.
const HTTP_CLIENT_NAME: &str = "_http-client";
const HTTP_CAPABILITY_NAME: &str = "_http";

/// The shared random capability agents and A2A tools import for ids.
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

/// Handles `[model.NAME]`, `[agent.NAME]` and `[a2a-tool.NAME]`, each of which
/// becomes a component named `NAME`.
#[derive(Default)]
pub(crate) struct AgentConfigHandler {
    a2a_client_emitted: bool,
    http_emitted: bool,
    random_emitted: bool,
    clocks_emitted: bool,
}

impl ConfigHandler for AgentConfigHandler {
    fn claimed_categories(&self) -> Vec<CategoryClaim> {
        vec![
            CategoryClaim::all("model"),
            CategoryClaim::all("agent"),
            CategoryClaim::all("a2a-tool"),
        ]
    }

    fn claimed_properties(&self) -> HashMap<&str, &[&str]> {
        HashMap::from([
            (
                "model",
                [
                    "provider",
                    "name",
                    "max-tokens",
                    "temperature",
                    "top-p",
                    "top-k",
                    "seed",
                    "stop-sequences",
                    "effort",
                    "include-reasoning",
                    "max-retries",
                ]
                .as_slice(),
            ),
            (
                "agent",
                [
                    "model",
                    "toolset",
                    "instructions",
                    "description",
                    "version",
                    "max-turns",
                    "report-reasoning",
                    "report-tool-calls",
                ]
                .as_slice(),
            ),
            ("a2a-tool", ["server-url"].as_slice()),
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
            "a2a-tool" => self.a2a_tool(&name, &mut properties),
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
        // Every client sends `effort`. The Messages API client checks its
        // value when a session starts.
        if let Some(effort) = optional_string(&context, "effort", properties)? {
            config.insert("effort".to_string(), json!(effort));
        }
        // `include-reasoning` and `max-retries` are Messages API client
        // settings, rejected for other providers.
        let messages_api = uri == ANTHROPIC_MESSAGES_CLIENT;
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
        // The sampling settings, each with whether the provider's API takes
        // it. The Messages API takes only `stop-sequences`, and no API here
        // takes `top-k`. The stop sequences are passed as a JSON array.
        let stop_sequences = optional_strings(&context, "stop-sequences", properties)?
            .map(|sequences| serde_json::to_string(&sequences))
            .transpose()?;
        let sampling = [
            (
                "temperature",
                optional_f64(&context, "temperature", properties)?.map(|value| json!(value)),
                !messages_api,
            ),
            (
                "top-p",
                optional_f64(&context, "top-p", properties)?.map(|value| json!(value)),
                !messages_api,
            ),
            (
                "top-k",
                optional_u32(&context, "top-k", properties)?.map(|value| json!(value)),
                false,
            ),
            (
                "seed",
                optional_u64(&context, "seed", properties)?.map(|value| json!(value)),
                !messages_api,
            ),
            (
                "stop-sequences",
                stop_sequences.map(|value| json!(value)),
                true,
            ),
        ];
        for (key, value, supported) in sampling {
            let Some(value) = value else { continue };
            if !supported {
                bail!("{context}: '{key}' is not supported for provider '{provider}'");
            }
            config.insert(key.to_string(), value);
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
        let toolset = optional_string(&context, "toolset", properties)?;

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
        if let Some(report_reasoning) = optional_bool(&context, "report-reasoning", properties)? {
            config.insert("report-reasoning".to_string(), json!(report_reasoning));
        }
        // `max-turns` and `report-tool-calls` are tool-calling-agent settings,
        // rejected for an agent without a toolset.
        if let Some(max_turns) = optional_u32(&context, "max-turns", properties)? {
            if toolset.is_none() {
                bail!("{context}: 'max-turns' is only supported for an agent with a 'toolset'");
            }
            if max_turns == 0 {
                bail!("{context}: 'max-turns' must be greater than 0");
            }
            config.insert("max-turns".to_string(), json!(max_turns));
        }
        if let Some(report_tool_calls) = optional_bool(&context, "report-tool-calls", properties)? {
            if toolset.is_none() {
                bail!(
                    "{context}: 'report-tool-calls' is only supported for an agent with a 'toolset'"
                );
            }
            config.insert("report-tool-calls".to_string(), json!(report_tool_calls));
        }
        let (uri, imports) = match toolset {
            Some(toolset) => (
                TOOL_CALLING_AGENT,
                json!([model, toolset, RANDOM_CAPABILITY_NAME]),
            ),
            None => (CHAT_AGENT, json!([model, RANDOM_CAPABILITY_NAME])),
        };
        let mut definitions = vec![component(
            name,
            json!({
                "uri": uri,
                "imports": imports,
                "config": config,
            }),
        )];
        definitions.extend(self.random_wiring());
        Ok(definitions)
    }

    fn a2a_tool(&mut self, name: &str, properties: &mut PropertyMap) -> Result<Vec<Definition>> {
        let context = format!("A2A tool '{name}'");
        let server_url = required_string(&context, "server-url", properties)?;
        let mut definitions = vec![component(
            name,
            json!({
                "uri": A2A_TOOL,
                "imports": [A2A_CLIENT_NAME, RANDOM_CAPABILITY_NAME],
                "config": { "server-url": server_url },
            }),
        )];
        definitions.extend(self.a2a_client_wiring());
        definitions.extend(self.random_wiring());
        Ok(definitions)
    }

    // The shared A2A client and the HTTP client it imports, unless already
    // emitted.
    fn a2a_client_wiring(&mut self) -> Vec<Definition> {
        let mut definitions = Vec::new();
        if !self.a2a_client_emitted {
            self.a2a_client_emitted = true;
            definitions.push(component(
                A2A_CLIENT_NAME,
                json!({ "uri": A2A_CLIENT, "imports": [HTTP_CLIENT_NAME] }),
            ));
        }
        definitions.extend(self.http_wiring());
        definitions
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

fn optional_f64(context: &str, key: &str, properties: &mut PropertyMap) -> Result<Option<f64>> {
    match properties.remove(key) {
        Some(Value::Number(number)) => number
            .as_f64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("{context}: '{key}' must be a number")),
        Some(got) => bail!("{context}: '{key}' must be a number, got {got}"),
        None => Ok(None),
    }
}

fn optional_u64(context: &str, key: &str, properties: &mut PropertyMap) -> Result<Option<u64>> {
    match properties.remove(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("{context}: '{key}' must be a non-negative integer")),
        Some(got) => bail!("{context}: '{key}' must be a number, got {got}"),
        None => Ok(None),
    }
}

fn optional_strings(
    context: &str,
    key: &str,
    properties: &mut PropertyMap,
) -> Result<Option<Vec<String>>> {
    match properties.remove(key) {
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(item) => Ok(item),
                got => bail!("{context}: '{key}' must be a list of strings, got {got}"),
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(got) => bail!("{context}: '{key}' must be a list of strings, got {got}"),
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
    fn the_effort_is_passed_to_a_chat_completions_client() {
        for provider in ["ollama", "openai"] {
            let mut handler = AgentConfigHandler::default();
            let definitions = handler
                .handle_definition(definition(
                    "model",
                    "other",
                    json!({ "provider": provider, "name": "m", "effort": "high" }),
                ))
                .unwrap();
            let produced = produced(definitions);
            assert_eq!(
                produced[&key("component", "other")]["config"]["effort"],
                "high",
                "{provider}"
            );
        }
    }

    #[test]
    fn sampling_settings_are_passed_to_the_client() {
        for provider in ["ollama", "openai"] {
            let mut handler = AgentConfigHandler::default();
            let definitions = handler
                .handle_definition(definition(
                    "model",
                    "other",
                    json!({
                        "provider": provider,
                        "name": "m",
                        "temperature": 0.2,
                        "top-p": 0.9,
                        "seed": 0,
                        "stop-sequences": ["END", "a, b"],
                    }),
                ))
                .unwrap();
            let produced = produced(definitions);
            let config = &produced[&key("component", "other")]["config"];
            assert_eq!(config["temperature"], 0.2, "{provider}");
            assert_eq!(config["top-p"], 0.9, "{provider}");
            assert_eq!(config["seed"], 0, "{provider}");
            assert_eq!(config["stop-sequences"], r#"["END","a, b"]"#, "{provider}");
        }

        let mut handler = AgentConfigHandler::default();
        let definitions = handler
            .handle_definition(definition(
                "model",
                "other",
                json!({ "provider": "anthropic", "name": "m", "stop-sequences": ["END"] }),
            ))
            .unwrap();
        let produced = produced(definitions);
        assert_eq!(
            produced[&key("component", "other")]["config"]["stop-sequences"],
            r#"["END"]"#
        );
    }

    #[test]
    fn a_sampling_setting_the_providers_api_does_not_take_is_rejected() {
        for (provider, property, value) in [
            ("anthropic", "temperature", json!(0.2)),
            ("anthropic", "top-p", json!(0.9)),
            ("anthropic", "top-k", json!(40)),
            ("anthropic", "seed", json!(7)),
            ("ollama", "top-k", json!(40)),
            ("openai", "top-k", json!(40)),
        ] {
            let mut properties = json!({ "provider": provider, "name": "m" });
            properties[property] = value;
            let mut handler = AgentConfigHandler::default();
            let error = handler
                .handle_definition(definition("model", "other", properties))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!(
                    "'{property}' is not supported for provider '{provider}'"
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn messages_api_settings_are_rejected_for_a_model_without_that_client() {
        for property in ["include-reasoning", "max-retries"] {
            for provider in ["ollama", "openai"] {
                let value = match property {
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
    fn an_a2a_tool_is_an_a2a_tool_component_importing_the_shared_client() {
        let mut handler = AgentConfigHandler::default();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "a2a-tool",
                    "weather-agent",
                    json!({ "server-url": "http://localhost:9091/" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            produced[&key("component", "weather-agent")],
            json!({
                "uri": A2A_TOOL,
                "imports": ["_a2a-client", "_random"],
                "config": { "server-url": "http://localhost:9091/" },
            })
        );
        assert_eq!(
            produced[&key("component", "_a2a-client")],
            json!({ "uri": A2A_CLIENT, "imports": ["_http-client"] })
        );
        assert_eq!(
            produced[&key("component", "_http-client")],
            json!({ "uri": HTTP_CLIENT, "imports": ["_http"] })
        );
        assert_eq!(
            produced[&key("capability", "_http")],
            json!({ "type": "wasi:http" })
        );
        assert_eq!(
            produced[&key("capability", "_random")],
            json!({ "type": "wasi:random" })
        );
        assert_eq!(produced.len(), 5);
    }

    #[test]
    fn a_second_a2a_tool_adds_only_its_own_component() {
        let mut handler = AgentConfigHandler::default();
        handler
            .handle_definition(definition(
                "a2a-tool",
                "weather-agent",
                json!({ "server-url": "http://localhost:9091/" }),
            ))
            .unwrap();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "a2a-tool",
                    "travel-agent",
                    json!({ "server-url": "http://localhost:9092/" }),
                ))
                .unwrap(),
        );
        assert_eq!(produced.len(), 1);
        assert!(produced.contains_key(&key("component", "travel-agent")));
    }

    #[test]
    fn an_a2a_tool_requires_a_server_url() {
        let mut handler = AgentConfigHandler::default();
        let error = handler
            .handle_definition(definition("a2a-tool", "weather-agent", json!({})))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("A2A tool 'weather-agent'") && error.contains("server-url"),
            "{error}"
        );
    }

    #[test]
    fn an_agent_without_a_toolset_is_a_chat_agent_importing_its_model() {
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
                        "report-reasoning": true,
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
                    "report-reasoning": true,
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
    fn an_agent_with_a_toolset_is_a_tool_calling_agent_importing_its_model_and_toolset() {
        let mut handler = AgentConfigHandler::default();
        let produced = produced(
            handler
                .handle_definition(definition(
                    "agent",
                    "helper",
                    json!({
                        "model": "local",
                        "toolset": "tools",
                        "description": "Helps",
                        "version": "1.0.0",
                        "max-turns": 4,
                        "report-reasoning": true,
                        "report-tool-calls": true,
                    }),
                ))
                .unwrap(),
        );
        assert_eq!(
            produced[&key("component", "helper")],
            json!({
                "uri": TOOL_CALLING_AGENT,
                "imports": ["local", "tools", "_random"],
                "config": {
                    "name": "helper",
                    "description": "Helps",
                    "version": "1.0.0",
                    "max-turns": 4,
                    "report-reasoning": true,
                    "report-tool-calls": true,
                },
            })
        );
        assert_eq!(produced.len(), 2);
    }

    #[test]
    fn tool_calling_settings_are_rejected_for_an_agent_without_a_toolset() {
        for (property, value) in [("max-turns", json!(4)), ("report-tool-calls", json!(true))] {
            let mut properties =
                json!({ "model": "local", "description": "Helps", "version": "1.0.0" });
            properties[property] = value;
            let mut handler = AgentConfigHandler::default();
            let error = handler
                .handle_definition(definition("agent", "helper", properties))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!(
                    "'{property}' is only supported for an agent with a 'toolset'"
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn max_turns_must_be_greater_than_0() {
        let mut handler = AgentConfigHandler::default();
        let error = handler
            .handle_definition(definition(
                "agent",
                "helper",
                json!({
                    "model": "local",
                    "toolset": "tools",
                    "description": "Helps",
                    "version": "1.0.0",
                    "max-turns": 0,
                }),
            ))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("'max-turns' must be greater than 0"),
            "{error}"
        );
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
