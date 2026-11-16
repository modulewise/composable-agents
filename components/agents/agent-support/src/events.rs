//! Task events and messages, with the ids they carry.

use wit_bindgen::rt::async_support::StreamWriter;

use crate::composable::agents::types::{
    AgentEvent, Message, Part, PartContent, Role, TaskState, TaskStatus, TaskStatusUpdateEvent,
};

/// The ids every event of a task carries.
pub struct TaskIds {
    pub task_id: String,
    pub context_id: String,
}

/// A status update event.
pub fn status_event(ids: &TaskIds, state: TaskState, message: Option<Message>) -> AgentEvent {
    AgentEvent::Status(TaskStatusUpdateEvent {
        task_id: ids.task_id.clone(),
        context_id: ids.context_id.clone(),
        status: TaskStatus {
            state,
            message,
            timestamp: None,
        },
        metadata: None,
    })
}

/// The task's events. Once the caller stops reading, they're dropped.
pub struct Events {
    writer: Option<StreamWriter<AgentEvent>>,
}

impl Events {
    pub fn new(writer: StreamWriter<AgentEvent>) -> Self {
        Events {
            writer: Some(writer),
        }
    }

    pub async fn send(&mut self, event: AgentEvent) {
        if let Some(writer) = &mut self.writer
            && writer.write_one(event).await.is_some()
        {
            self.writer = None;
        }
    }

    /// Sends the events in order.
    pub async fn send_all(&mut self, events: Vec<AgentEvent>) {
        for event in events {
            self.send(event).await;
        }
    }
}

/// A message from the agent with one text part.
pub fn agent_message(ids: &TaskIds, text: String) -> Message {
    Message {
        message_id: new_id(),
        context_id: Some(ids.context_id.clone()),
        task_id: Some(ids.task_id.clone()),
        role: Role::Agent,
        parts: vec![text_part(text)],
        metadata: None,
        extensions: Vec::new(),
        reference_task_ids: Vec::new(),
    }
}

/// A text part with no metadata.
pub fn text_part(text: String) -> Part {
    Part {
        content: PartContent::Text(text),
        metadata: None,
        filename: None,
        media_type: None,
    }
}

/// A new id as a random (version 4) UUID. Two `get-random-u64` calls provide
/// the 16 bytes, since `get-random-bytes` may return fewer than requested.
pub fn new_id() -> String {
    use crate::wasi::random::random::get_random_u64;

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&get_random_u64().to_le_bytes());
    bytes[8..].copy_from_slice(&get_random_u64().to_le_bytes());
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}
