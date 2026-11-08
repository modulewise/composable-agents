//! The model's reasoning, sent as `working` status messages.

use crate::composable::agents::types::{AgentEvent, Message, Part, TaskState};
use crate::composable::models::types::ModelOutputItem;
use crate::events::{TaskIds, agent_message, status_event, text_part};

// Part metadata that marks a status message as the model's reasoning.
const REASONING_METADATA: &str = r#"{"reasoning":true}"#;

/// Reasoning deltas collected since the last reasoning message was sent.
#[derive(Default)]
pub struct ReasoningRun {
    text: String,
    /// Whether any reasoning delta arrived.
    pub streamed: bool,
}

impl ReasoningRun {
    pub fn push(&mut self, chunk: String) {
        self.streamed = true;
        self.text.push_str(&chunk);
    }

    // Takes the collected text, if any.
    fn take(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.text);
        (!text.is_empty()).then_some(text)
    }

    /// A status event with the collected text, if any.
    pub fn finish(&mut self, ids: &TaskIds) -> Option<AgentEvent> {
        self.take().map(|text| reasoning_event(ids, text))
    }
}

/// The joined text of each run of consecutive reasoning items, skipping runs
/// without text (such as redacted reasoning).
pub fn reasoning_runs(output: &[ModelOutputItem]) -> Vec<String> {
    let mut runs = vec![String::new()];
    for item in output {
        match item {
            ModelOutputItem::Reasoning(reasoning) => {
                runs.last_mut().unwrap().push_str(&reasoning.text);
            }
            _ => runs.push(String::new()),
        }
    }
    runs.retain(|text| !text.is_empty());
    runs
}

/// A `working` status with the model's reasoning as its message.
pub fn reasoning_event(ids: &TaskIds, text: String) -> AgentEvent {
    let message = Message {
        parts: vec![reasoning_part(text)],
        ..agent_message(ids, String::new())
    };
    status_event(ids, TaskState::Working, Some(message))
}

// The model's reasoning as a text part marked by `REASONING_METADATA`.
fn reasoning_part(text: String) -> Part {
    Part {
        metadata: Some(REASONING_METADATA.to_string()),
        ..text_part(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::agents::types::PartContent;
    use crate::composable::models::types::Reasoning;

    fn reasoning(text: &str, redacted: bool) -> ModelOutputItem {
        ModelOutputItem::Reasoning(Reasoning {
            text: text.to_string(),
            redacted,
        })
    }

    #[test]
    fn a_run_of_reasoning_joins_its_deltas_until_it_ends() {
        let mut run = ReasoningRun::default();
        assert!(run.take().is_none());
        assert!(!run.streamed);

        run.push("Add ".to_string());
        run.push("them.".to_string());
        assert_eq!(run.take().as_deref(), Some("Add them."));

        // The next run starts empty, and reasoning still counts as streamed.
        assert!(run.take().is_none());
        assert!(run.streamed);
    }

    #[test]
    fn reasoning_is_a_text_part_marked_in_its_metadata() {
        let part = reasoning_part("Add them.".to_string());
        assert!(matches!(&part.content, PartContent::Text(text) if text == "Add them."));
        assert_eq!(part.metadata.as_deref(), Some(r#"{"reasoning":true}"#));
    }

    #[test]
    fn reasoning_runs_join_consecutive_items_and_skip_those_without_text() {
        let output = [
            reasoning("First ", false),
            reasoning("thought.", false),
            ModelOutputItem::Text("Hi".to_string()),
            reasoning("", true),
            ModelOutputItem::Text("!".to_string()),
            reasoning("Second.", false),
        ];
        assert_eq!(
            reasoning_runs(&output),
            ["First thought.".to_string(), "Second.".to_string()]
        );
        assert!(reasoning_runs(&[ModelOutputItem::Text("Hi".to_string())]).is_empty());
    }
}
