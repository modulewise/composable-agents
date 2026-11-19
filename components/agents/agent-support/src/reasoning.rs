//! The model's reasoning, sent as `working` status messages.

use crate::composable::agents::types::{AgentEvent, Message, Part, TaskState};
use crate::composable::models::types::ModelOutputItem;
use crate::events::{TaskIds, agent_message, status_event, text_part};

// Part metadata that marks a status message as the model's reasoning.
const REASONING_METADATA: &str = r#"{"reasoning":true}"#;

/// The config key for whether the agent reports the model's reasoning
/// (default `false`).
pub const REPORT_REASONING_CONFIG: &str = "report-reasoning";

/// Reasoning deltas collected since the last reasoning message was sent.
pub struct ReasoningRun {
    // Whether the agent reports reasoning. When false, no text is collected
    // and no event is produced.
    report: bool,
    text: String,
    // Whether any reasoning delta arrived.
    streamed: bool,
}

impl ReasoningRun {
    pub fn new(report: bool) -> Self {
        ReasoningRun {
            report,
            text: String::new(),
            streamed: false,
        }
    }

    pub fn push(&mut self, chunk: String) {
        self.streamed = true;
        if self.report {
            self.text.push_str(&chunk);
        }
    }

    /// A status event for each run of reasoning in `output`, for a response
    /// whose reasoning was not streamed.
    pub fn unstreamed(&self, ids: &TaskIds, output: &[ModelOutputItem]) -> Vec<AgentEvent> {
        if !self.report || self.streamed {
            return Vec::new();
        }
        reasoning_runs(output)
            .into_iter()
            .map(|text| reasoning_event(ids, text))
            .collect()
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

// The joined text of each run of consecutive reasoning items, skipping runs
// without text (such as redacted reasoning).
fn reasoning_runs(output: &[ModelOutputItem]) -> Vec<String> {
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

// A `working` status with the model's reasoning as its message.
fn reasoning_event(ids: &TaskIds, text: String) -> AgentEvent {
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

    fn ids() -> TaskIds {
        TaskIds {
            task_id: "task-1".to_string(),
            context_id: "context-1".to_string(),
        }
    }

    #[test]
    fn a_run_of_reasoning_joins_its_deltas_until_it_ends() {
        let mut run = ReasoningRun::new(true);
        assert!(run.take().is_none());
        assert!(!run.streamed);

        run.push("Add ".to_string());
        run.push("the numbers.".to_string());
        assert_eq!(run.take().as_deref(), Some("Add the numbers."));

        // The next run starts empty, and reasoning still counts as streamed.
        assert!(run.take().is_none());
        assert!(run.streamed);
    }

    #[test]
    fn reasoning_that_is_not_reported_produces_no_events() {
        let mut run = ReasoningRun::new(false);
        run.push("Add the numbers.".to_string());
        assert!(run.finish(&ids()).is_none());

        let run = ReasoningRun::new(false);
        let output = [reasoning("Add the numbers.", false)];
        assert!(run.unstreamed(&ids(), &output).is_empty());
    }

    #[test]
    fn reasoning_in_the_output_is_not_sent_when_it_was_streamed() {
        let mut run = ReasoningRun::new(true);
        run.push("Add the numbers.".to_string());
        let output = [reasoning("Add the numbers.", false)];
        assert!(run.unstreamed(&ids(), &output).is_empty());
    }

    #[test]
    fn reasoning_is_a_text_part_marked_in_its_metadata() {
        let part = reasoning_part("Add the numbers.".to_string());
        assert!(matches!(&part.content, PartContent::Text(text) if text == "Add the numbers."));
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
