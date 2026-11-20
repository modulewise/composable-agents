//! The agent card, built from an agent's metadata.

use a2a_json as a2a;
use composable_agents::types::{AgentExtension, AgentMetadata, AgentSkill};
use serde_json::Value;

/// The binding of the interface a card declares.
const JSONRPC: &str = "JSONRPC";

/// The capabilities the server declares for an agent. Streaming is supported.
/// Push notifications and the extended agent card are not. The extensions
/// are the agent's. Fails on extension params that aren't a JSON object.
pub(crate) fn capabilities(metadata: &AgentMetadata) -> Result<a2a::AgentCapabilities, String> {
    Ok(a2a::AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        extensions: metadata
            .extensions
            .iter()
            .cloned()
            .map(extension)
            .collect::<Result<_, _>>()?,
        extended_agent_card: Some(false),
    })
}

/// The card for an agent whose JSON-RPC interface is at `url`. Fails when a
/// list that A2A requires to have at least one element is empty.
pub(crate) fn agent_card(
    metadata: AgentMetadata,
    capabilities: a2a::AgentCapabilities,
    url: &str,
) -> Result<a2a::AgentCard, String> {
    Ok(a2a::AgentCard {
        name: metadata.name,
        description: metadata.description,
        supported_interfaces: vec![a2a::AgentInterface {
            url: url.to_string(),
            protocol_binding: JSONRPC.to_string(),
            tenant: None,
            protocol_version: a2a::PROTOCOL_VERSION.to_string(),
        }],
        provider: metadata.provider.map(|provider| a2a::AgentProvider {
            url: provider.url,
            organization: provider.organization,
        }),
        version: metadata.version,
        documentation_url: metadata.documentation_url,
        capabilities,
        security_schemes: Default::default(),
        security_requirements: Vec::new(),
        default_input_modes: non_empty(metadata.default_input_modes, "default-input-modes")?,
        default_output_modes: non_empty(metadata.default_output_modes, "default-output-modes")?,
        skills: non_empty(metadata.skills, "skills")?
            .into_iter()
            .map(skill)
            .collect::<Result<_, _>>()?,
        signatures: Vec::new(),
        icon_url: metadata.icon_url,
    })
}

// `list`, which A2A requires to have at least one element. `name` identifies
// the list in the error.
fn non_empty<T>(list: Vec<T>, name: &str) -> Result<Vec<T>, String> {
    if list.is_empty() {
        return Err(format!("'{name}' must have at least one element"));
    }
    Ok(list)
}

fn skill(skill: AgentSkill) -> Result<a2a::AgentSkill, String> {
    let name = format!("the tags of skill '{}'", skill.id);
    if skill.tags.is_empty() {
        return Err(format!("{name} must have at least one element"));
    }
    Ok(a2a::AgentSkill {
        id: skill.id,
        name: skill.name,
        description: skill.description,
        tags: skill.tags,
        examples: skill.examples,
        input_modes: skill.input_modes,
        output_modes: skill.output_modes,
        security_requirements: skill
            .security_requirements
            .into_iter()
            .map(|requirement| a2a::SecurityRequirement {
                schemes: requirement
                    .schemes
                    .into_iter()
                    .map(|(scheme, list)| (scheme, a2a::StringList { list }))
                    .collect(),
            })
            .collect(),
    })
}

fn extension(extension: AgentExtension) -> Result<a2a::AgentExtension, String> {
    let params = extension
        .params
        .map(|params| match serde_json::from_str(&params) {
            Ok(Value::Object(params)) => Ok(params),
            _ => Err("an extension's params are not a JSON object".to_string()),
        })
        .transpose()?;
    Ok(a2a::AgentExtension {
        uri: extension.uri,
        description: extension.description,
        required: extension.required,
        params,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn metadata() -> AgentMetadata {
        AgentMetadata {
            name: "assistant".to_string(),
            description: "Answers questions concisely".to_string(),
            provider: None,
            version: "0.1.0".to_string(),
            documentation_url: None,
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
            skills: vec![AgentSkill {
                id: "chat".to_string(),
                name: "Chat".to_string(),
                description: "Answers questions concisely".to_string(),
                tags: vec!["chat".to_string()],
                examples: Vec::new(),
                input_modes: Vec::new(),
                output_modes: Vec::new(),
                security_requirements: Vec::new(),
            }],
            icon_url: None,
            extensions: vec![AgentExtension {
                uri: Some("https://example.com/ext/a".to_string()),
                description: None,
                required: true,
                params: Some(r#"{"k":"v"}"#.to_string()),
            }],
        }
    }

    fn card(metadata: AgentMetadata) -> Result<a2a::AgentCard, String> {
        let capabilities = capabilities(&metadata)?;
        agent_card(metadata, capabilities, "http://127.0.0.1:4001")
    }

    #[test]
    fn a_card_has_the_metadata_the_interface_and_the_capabilities() {
        assert_eq!(
            json!(card(metadata()).unwrap()),
            json!({
                "name": "assistant",
                "description": "Answers questions concisely",
                "supportedInterfaces": [{
                    "url": "http://127.0.0.1:4001",
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": "1.0",
                }],
                "version": "0.1.0",
                "capabilities": {
                    "streaming": true,
                    "pushNotifications": false,
                    "extensions": [{
                        "uri": "https://example.com/ext/a",
                        "required": true,
                        "params": { "k": "v" },
                    }],
                    "extendedAgentCard": false,
                },
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/plain"],
                "skills": [{
                    "id": "chat",
                    "name": "Chat",
                    "description": "Answers questions concisely",
                    "tags": ["chat"],
                }],
            })
        );
    }

    #[test]
    fn a_required_list_that_is_empty_is_an_error() {
        let mut no_input_modes = metadata();
        no_input_modes.default_input_modes.clear();
        assert_eq!(
            card(no_input_modes).unwrap_err(),
            "'default-input-modes' must have at least one element"
        );

        let mut no_output_modes = metadata();
        no_output_modes.default_output_modes.clear();
        assert_eq!(
            card(no_output_modes).unwrap_err(),
            "'default-output-modes' must have at least one element"
        );

        let mut no_skills = metadata();
        no_skills.skills.clear();
        assert_eq!(
            card(no_skills).unwrap_err(),
            "'skills' must have at least one element"
        );

        let mut no_tags = metadata();
        no_tags.skills[0].tags.clear();
        assert_eq!(
            card(no_tags).unwrap_err(),
            "the tags of skill 'chat' must have at least one element"
        );
    }
}
