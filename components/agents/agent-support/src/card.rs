//! The agent card an agent's `metadata()` returns.

use crate::composable::agents::types::{AgentMetadata, AgentSkill};
use crate::config::required_config;

const NAME_CONFIG: &str = "name";
const DESCRIPTION_CONFIG: &str = "description";
const VERSION_CONFIG: &str = "version";

// The media type of the text an agent accepts and produces.
const TEXT_PLAIN: &str = "text/plain";

/// The agent's metadata from its `name`, `description`, and `version` config,
/// with one `chat` skill described by the agent's description.
pub fn agent_metadata() -> Result<AgentMetadata, String> {
    let description = required_config(DESCRIPTION_CONFIG)?;
    Ok(AgentMetadata {
        name: required_config(NAME_CONFIG)?,
        description: description.clone(),
        provider: None,
        version: required_config(VERSION_CONFIG)?,
        documentation_url: None,
        default_input_modes: vec![TEXT_PLAIN.to_string()],
        default_output_modes: vec![TEXT_PLAIN.to_string()],
        skills: vec![AgentSkill {
            id: "chat".to_string(),
            name: "chat".to_string(),
            description,
            tags: vec!["chat".to_string()],
            examples: Vec::new(),
            input_modes: Vec::new(),
            output_modes: Vec::new(),
            security_requirements: Vec::new(),
        }],
        icon_url: None,
        extensions: Vec::new(),
    })
}
