//! Tool names as sent to the model APIs: each `.` is replaced with `_`, and
//! the names in responses are mapped back. A name longer than 128 characters,
//! or two names that collide after one is modified (`a.b` and `a_b`), fail the
//! session. Other invalid names are sent as they are, to fail at the API.
//!
//! The client sees only the sent names (in settings, history and input).
//! Names in deltas and output are mapped back.

use std::collections::HashMap;

use crate::composable::agents::types::{
    ContentDelta, ModelDelta, ModelError, ModelExchange, ModelInputItem, ModelOutputItem,
    ModelSettings, TokenUsage, ToolChoice,
};

/// The maximum length of a tool name.
const MAX_LEN: usize = 128;

/// The name of a tool as sent to the client.
fn sent(name: &str) -> String {
    name.replace('.', "_")
}

/// Maps each sent tool name to its original name.
pub(crate) struct ToolNames {
    originals: HashMap<String, String>,
}

impl ToolNames {
    /// Fails on an empty name, a name over 128 characters, or two names that
    /// collide after modification (`a.b` and `a_b`).
    pub(crate) fn new(settings: &ModelSettings) -> Result<Self, ModelError> {
        let mut originals = HashMap::new();
        for definition in settings.tools.iter().flatten() {
            let length = definition.name.chars().count();
            if length == 0 || length > MAX_LEN {
                return Err(ModelError {
                    message: format!(
                        "tool name '{}' must be 1 to {MAX_LEN} characters",
                        definition.name
                    ),
                    status: None,
                    usage: TokenUsage::default(),
                });
            }
            let name = sent(&definition.name);
            if let Some(other) = originals.insert(name.clone(), definition.name.clone()) {
                return Err(ModelError {
                    message: format!(
                        "tools '{other}' and '{}' would both be sent as '{name}'",
                        definition.name
                    ),
                    status: None,
                    usage: TokenUsage::default(),
                });
            }
        }
        Ok(ToolNames { originals })
    }

    /// The original name for a sent name, or the name itself if no tool with
    /// that name was sent.
    pub(crate) fn original(&self, sent: &str) -> String {
        self.originals
            .get(sent)
            .cloned()
            .unwrap_or_else(|| sent.to_string())
    }

    /// A delta with a tool call's name mapped back.
    pub(crate) fn original_delta(&self, delta: ModelDelta) -> ModelDelta {
        match delta {
            ModelDelta::Content(ContentDelta::ToolCall(mut call)) => {
                call.name = call.name.map(|name| self.original(&name));
                ModelDelta::Content(ContentDelta::ToolCall(call))
            }
            delta => delta,
        }
    }

    /// A response's output with its tool calls' names mapped back.
    pub(crate) fn original_output(&self, output: Vec<ModelOutputItem>) -> Vec<ModelOutputItem> {
        output
            .into_iter()
            .map(|item| match item {
                ModelOutputItem::ToolCall(mut call) => {
                    call.name = self.original(&call.name);
                    ModelOutputItem::ToolCall(call)
                }
                item => item,
            })
            .collect()
    }
}

/// The settings with tool names as sent, including a specific tool choice.
pub(crate) fn sent_settings(settings: &ModelSettings) -> ModelSettings {
    let mut settings = settings.clone();
    for definition in settings.tools.iter_mut().flatten() {
        definition.name = sent(&definition.name);
    }
    if let Some(ToolChoice::Specific(name)) = &mut settings.tool_choice {
        *name = sent(name);
    }
    settings
}

/// A call's input with its tool results named as sent.
pub(crate) fn sent_input(input: &[ModelInputItem]) -> Vec<ModelInputItem> {
    input
        .iter()
        .cloned()
        .map(|item| match item {
            ModelInputItem::ToolResult(mut result) => {
                result.name = sent(&result.name);
                ModelInputItem::ToolResult(result)
            }
            item => item,
        })
        .collect()
}

/// Earlier exchanges with their tool calls and tool results named as sent.
pub(crate) fn sent_history(history: Vec<ModelExchange>) -> Vec<ModelExchange> {
    history
        .into_iter()
        .map(|mut exchange| {
            exchange.request.input = sent_input(&exchange.request.input);
            for item in exchange.output.iter_mut().flatten() {
                if let ModelOutputItem::ToolCall(call) = item {
                    call.name = sent(&call.name);
                }
            }
            exchange
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::agents::types::{
        ModelRequest, OpaqueContent, ToolCall, ToolCallDelta, ToolDefinition, ToolResult,
    };

    fn settings(names: &[&str]) -> ModelSettings {
        ModelSettings {
            instructions: None,
            tools: Some(
                names
                    .iter()
                    .map(|name| ToolDefinition {
                        name: name.to_string(),
                        description: String::new(),
                        input_schema: "{}".into(),
                    })
                    .collect(),
            ),
            tool_choice: None,
            output_schema: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            seed: None,
            stop_sequences: None,
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    fn result(name: &str) -> ToolResult {
        ToolResult {
            tool_call_id: "call-1".into(),
            name: name.into(),
            content: Vec::new(),
            is_error: false,
        }
    }

    #[test]
    fn the_client_gets_every_tool_name_with_its_dots_replaced() {
        let settings = ModelSettings {
            tool_choice: Some(ToolChoice::Specific("calc.add".into())),
            ..settings(&["calc.add", "greet"])
        };
        let sent = sent_settings(&settings);
        let names: Vec<&str> = sent
            .tools
            .iter()
            .flatten()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(names, ["calc_add", "greet"]);
        assert!(
            matches!(sent.tool_choice, Some(ToolChoice::Specific(ref name)) if name == "calc_add")
        );

        let input = sent_input(&[
            ModelInputItem::Text("a.b stays".into()),
            ModelInputItem::ToolResult(result("calc.add")),
        ]);
        assert!(matches!(&input[0], ModelInputItem::Text(text) if text == "a.b stays"));
        assert!(
            matches!(&input[1], ModelInputItem::ToolResult(result) if result.name == "calc_add")
        );

        let history = sent_history(vec![ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::ToolResult(result("calc.mul"))],
                max_tokens: None,
            },
            output: Some(vec![ModelOutputItem::ToolCall(call("calc.add"))]),
            // Already in the API's form, with the names as they were sent.
            opaque_content: Some(OpaqueContent {
                api: "some-api".into(),
                content: r#"[{"name":"calc_add"}]"#.into(),
            }),
            provider: Some("some-provider".into()),
            request_model: Some("some-model".into()),
            response_model: None,
            usage: TokenUsage::default(),
        }]);
        assert_eq!(
            history[0].opaque_content.as_ref().unwrap().content,
            r#"[{"name":"calc_add"}]"#
        );
        assert!(
            matches!(&history[0].request.input[0], ModelInputItem::ToolResult(result) if result.name == "calc_mul")
        );
        assert!(
            matches!(&history[0].output.as_ref().unwrap()[0], ModelOutputItem::ToolCall(call) if call.name == "calc_add")
        );
    }

    #[test]
    fn names_the_model_calls_are_mapped_back() {
        let names = ToolNames::new(&settings(&["calc.add", "greet"])).unwrap();
        assert_eq!(names.original("calc_add"), "calc.add");
        assert_eq!(names.original("greet"), "greet");
        // A name the session didn't send comes back as the model called it.
        assert_eq!(names.original("other_tool"), "other_tool");

        let delta =
            names.original_delta(ModelDelta::Content(ContentDelta::ToolCall(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("calc_add".into()),
                arguments: String::new(),
            })));
        assert!(
            matches!(delta, ModelDelta::Content(ContentDelta::ToolCall(ToolCallDelta { name: Some(ref name), .. })) if name == "calc.add")
        );

        let output = names.original_output(vec![
            ModelOutputItem::Text("calc_add".into()),
            ModelOutputItem::ToolCall(call("calc_add")),
        ]);
        assert!(matches!(&output[0], ModelOutputItem::Text(text) if text == "calc_add"));
        assert!(matches!(&output[1], ModelOutputItem::ToolCall(call) if call.name == "calc.add"));
    }

    #[test]
    fn tools_that_would_share_a_name_fail() {
        assert_eq!(
            ToolNames::new(&settings(&["calc.add", "calc_add"]))
                .err()
                .unwrap()
                .message,
            "tools 'calc.add' and 'calc_add' would both be sent as 'calc_add'"
        );
        assert!(ToolNames::new(&settings(&["calc.add", "calc_mul"])).is_ok());
    }

    #[test]
    fn a_name_empty_or_longer_than_128_characters_fails() {
        let long = "x".repeat(129);
        for name in ["", long.as_str()] {
            assert_eq!(
                ToolNames::new(&settings(&[name])).err().unwrap().message,
                format!("tool name '{name}' must be 1 to 128 characters")
            );
        }
        assert!(ToolNames::new(&settings(&[&"x".repeat(128)])).is_ok());
    }
}
