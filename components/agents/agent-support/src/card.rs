//! The agent card an agent's `metadata()` returns.

use crate::composable::agents::types::{AgentMetadata, AgentSkill};
use crate::config::{get_config, required_config};

const NAME_CONFIG: &str = "name";
const DESCRIPTION_CONFIG: &str = "description";
const VERSION_CONFIG: &str = "version";
const INPUT_MODES_CONFIG: &str = "input-modes";

// The media type of the text an agent produces, and the one it accepts when
// no input modes are configured.
const TEXT_PLAIN: &str = "text/plain";

/// The agent's metadata from its `name`, `description`, `version`, and
/// `input-modes` config, with one `chat` skill described by the agent's
/// description.
pub fn agent_metadata() -> Result<AgentMetadata, String> {
    let description = required_config(DESCRIPTION_CONFIG)?;
    Ok(AgentMetadata {
        name: required_config(NAME_CONFIG)?,
        description: description.clone(),
        provider: None,
        version: required_config(VERSION_CONFIG)?,
        documentation_url: None,
        default_input_modes: input_modes(get_config(INPUT_MODES_CONFIG))?,
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

// The input modes in `value`, a JSON array of strings, or `text/plain` when
// there is no value.
fn input_modes(value: Option<String>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(vec![TEXT_PLAIN.to_string()]);
    };
    match serde_json::from_str::<Vec<String>>(&value) {
        Ok(modes) if !modes.is_empty() => Ok(modes),
        _ => Err(format!(
            "'{INPUT_MODES_CONFIG}' must be a JSON array with at least one media type"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_modes_are_a_json_array_of_strings_or_text_plain() {
        assert_eq!(input_modes(None).unwrap(), ["text/plain"]);
        assert_eq!(
            input_modes(Some(r#"["text/plain","image/png"]"#.to_string())).unwrap(),
            ["text/plain", "image/png"]
        );
        for invalid in ["[]", "text/plain", "[1]"] {
            assert_eq!(
                input_modes(Some(invalid.to_string())).unwrap_err(),
                "'input-modes' must be a JSON array with at least one media type"
            );
        }
    }
}
