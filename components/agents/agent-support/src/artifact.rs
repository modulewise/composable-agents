//! A task's text artifact, sent as chunks.

use crate::composable::agents::types::{AgentEvent, Artifact, TaskArtifactUpdateEvent};
use crate::events::{TaskIds, text_part};

/// A task's artifact with the text sent so far. It holds the latest chunk
/// until the next arrives, so that the last chunk sent is marked as such.
pub struct TextArtifact {
    artifact_id: String,
    // The text of the chunks sent so far.
    sent: String,
    held: Option<String>,
}

impl TextArtifact {
    pub fn new(artifact_id: String) -> Self {
        TextArtifact {
            artifact_id,
            sent: String::new(),
            held: None,
        }
    }

    /// Holds `chunk`, returning the event that sends the chunk it replaces. An
    /// empty chunk produces no event.
    pub fn push(&mut self, chunk: String, ids: &TaskIds) -> Option<AgentEvent> {
        if chunk.is_empty() {
            return None;
        }
        self.held
            .replace(chunk)
            .map(|previous| self.chunk_event(previous, false, ids))
    }

    /// The event that sends the held chunk, if any, as the last chunk.
    pub fn finish(&mut self, ids: &TaskIds) -> Option<AgentEvent> {
        self.held
            .take()
            .map(|last| self.chunk_event(last, true, ids))
    }

    fn chunk_event(&mut self, chunk: String, last_chunk: bool, ids: &TaskIds) -> AgentEvent {
        let append = !self.sent.is_empty();
        self.sent.push_str(&chunk);
        AgentEvent::Artifact(TaskArtifactUpdateEvent {
            task_id: ids.task_id.clone(),
            context_id: ids.context_id.clone(),
            artifact: self.with_text(chunk),
            append: Some(append),
            last_chunk: Some(last_chunk),
            metadata: None,
        })
    }

    /// The artifact as sent, or none if no chunk was sent.
    pub fn into_artifact(mut self) -> Option<Artifact> {
        if self.sent.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.sent);
        Some(self.with_text(text))
    }

    fn with_text(&self, text: String) -> Artifact {
        Artifact {
            artifact_id: self.artifact_id.clone(),
            name: None,
            description: None,
            parts: vec![text_part(text)],
            metadata: None,
            extensions: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::agents::types::{Part, PartContent};

    fn ids() -> TaskIds {
        TaskIds {
            task_id: "task-1".to_string(),
            context_id: "context-1".to_string(),
        }
    }

    // An artifact event's chunk text, `append`, and `last-chunk`.
    fn chunk(event: AgentEvent) -> (String, Option<bool>, Option<bool>) {
        let AgentEvent::Artifact(update) = event else {
            panic!("expected an artifact event");
        };
        assert_eq!(update.task_id, "task-1");
        assert_eq!(update.context_id, "context-1");
        assert_eq!(update.artifact.artifact_id, "artifact-1");
        let [
            Part {
                content: PartContent::Text(text),
                ..
            },
        ] = update.artifact.parts.as_slice()
        else {
            panic!("expected one text part");
        };
        (text.clone(), update.append, update.last_chunk)
    }

    fn text(artifact: Artifact) -> String {
        let [
            Part {
                content: PartContent::Text(text),
                ..
            },
        ] = artifact.parts.as_slice()
        else {
            panic!("expected one text part");
        };
        text.clone()
    }

    #[test]
    fn chunks_are_pushed_one_behind_with_the_last_chunk_marked() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());

        assert!(artifact.push("Hel".to_string(), &ids).is_none());
        assert!(artifact.push(String::new(), &ids).is_none());
        let first = artifact.push("lo".to_string(), &ids).unwrap();
        assert_eq!(chunk(first), ("Hel".to_string(), Some(false), Some(false)));
        let second = artifact.push("!".to_string(), &ids).unwrap();
        assert_eq!(chunk(second), ("lo".to_string(), Some(true), Some(false)));
        let last = artifact.finish(&ids).unwrap();
        assert_eq!(chunk(last), ("!".to_string(), Some(true), Some(true)));
        assert!(artifact.finish(&ids).is_none());

        assert_eq!(text(artifact.into_artifact().unwrap()), "Hello!");
    }

    #[test]
    fn a_single_chunk_is_the_first_and_the_last() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());

        assert!(artifact.push("Hi".to_string(), &ids).is_none());
        let last = artifact.finish(&ids).unwrap();
        assert_eq!(chunk(last), ("Hi".to_string(), Some(false), Some(true)));
    }

    #[test]
    fn an_artifact_with_nothing_sent_is_none() {
        let ids = ids();
        let mut artifact = TextArtifact::new("artifact-1".to_string());
        assert!(artifact.push("held".to_string(), &ids).is_none());

        // A held chunk that was never sent is not part of the artifact.
        assert!(artifact.into_artifact().is_none());
    }
}
