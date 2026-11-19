//! Conversions between A2A's JSON types and the agent interface's types. Both
//! mirror `a2a.proto`, so each conversion is field for field. A value that is
//! JSON in A2A is JSON text in the agent's types.

use a2a_json as a2a;
use composable_agents::types as agent;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// A client's message, for the agent.
pub(crate) fn agent_message(message: a2a::Message) -> agent::Message {
    agent::Message {
        message_id: message.message_id,
        context_id: message.context_id,
        task_id: message.task_id,
        role: match message.role {
            a2a::Role::Unspecified => agent::Role::Unspecified,
            a2a::Role::User => agent::Role::User,
            a2a::Role::Agent => agent::Role::Agent,
        },
        parts: message.parts.into_iter().map(agent_part).collect(),
        metadata: json_text(message.metadata),
        extensions: message.extensions,
        reference_task_ids: message.reference_task_ids,
    }
}

fn agent_part(part: a2a::Part) -> agent::Part {
    agent::Part {
        content: match part.content {
            a2a::PartContent::Text(text) => agent::PartContent::Text(text),
            a2a::PartContent::Raw(raw) => agent::PartContent::Raw(raw),
            a2a::PartContent::Url(url) => agent::PartContent::Url(url),
            a2a::PartContent::Data(data) => agent::PartContent::Data(data.to_string()),
        },
        metadata: json_text(part.metadata),
        filename: part.filename,
        media_type: part.media_type,
    }
}

// JSON text for a `json` field that is an object in A2A.
fn json_text(object: Option<a2a::Struct>) -> Option<String> {
    object.map(|object| Value::Object(object).to_string())
}

/// An event from the agent, as a stream response. Fails on JSON text that
/// isn't valid, or on a message or artifact with no parts.
pub(crate) fn a2a_event(event: agent::AgentEvent) -> Result<a2a::StreamResponse, String> {
    Ok(match event {
        agent::AgentEvent::Status(update) => {
            a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
                task_id: update.task_id,
                context_id: update.context_id,
                status: a2a_status(update.status)?,
                metadata: json_object(update.metadata, "an event")?,
            })
        }
        agent::AgentEvent::Artifact(update) => {
            a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
                task_id: update.task_id,
                context_id: update.context_id,
                artifact: a2a_artifact(update.artifact)?,
                append: update.append,
                last_chunk: update.last_chunk,
                metadata: json_object(update.metadata, "an event")?,
            })
        }
    })
}

/// A status from the agent. A status without a timestamp is given the
/// current time.
pub(crate) fn a2a_status(status: agent::TaskStatus) -> Result<a2a::TaskStatus, String> {
    Ok(a2a::TaskStatus {
        state: a2a_state(status.state),
        message: status.message.map(a2a_message).transpose()?,
        timestamp: Some(status.timestamp.unwrap_or_else(now)),
    })
}

fn a2a_state(state: agent::TaskState) -> a2a::TaskState {
    match state {
        agent::TaskState::Unspecified => a2a::TaskState::Unspecified,
        agent::TaskState::Submitted => a2a::TaskState::Submitted,
        agent::TaskState::Working => a2a::TaskState::Working,
        agent::TaskState::Completed => a2a::TaskState::Completed,
        agent::TaskState::Failed => a2a::TaskState::Failed,
        agent::TaskState::Canceled => a2a::TaskState::Canceled,
        agent::TaskState::InputRequired => a2a::TaskState::InputRequired,
        agent::TaskState::Rejected => a2a::TaskState::Rejected,
        agent::TaskState::AuthRequired => a2a::TaskState::AuthRequired,
    }
}

fn a2a_message(message: agent::Message) -> Result<a2a::Message, String> {
    Ok(a2a::Message {
        message_id: message.message_id,
        context_id: message.context_id,
        task_id: message.task_id,
        role: match message.role {
            agent::Role::Unspecified => a2a::Role::Unspecified,
            agent::Role::User => a2a::Role::User,
            agent::Role::Agent => a2a::Role::Agent,
        },
        parts: a2a_parts(message.parts, "a message")?,
        metadata: json_object(message.metadata, "a message")?,
        extensions: message.extensions,
        reference_task_ids: message.reference_task_ids,
    })
}

/// An artifact from the agent, or a chunk of one.
pub(crate) fn a2a_artifact(artifact: agent::Artifact) -> Result<a2a::Artifact, String> {
    Ok(a2a::Artifact {
        artifact_id: artifact.artifact_id,
        name: artifact.name,
        description: artifact.description,
        parts: a2a_parts(artifact.parts, "an artifact")?,
        metadata: json_object(artifact.metadata, "an artifact")?,
        extensions: artifact.extensions,
    })
}

// The parts of `owner`, e.g. "a message". A2A requires at least one.
fn a2a_parts(parts: Vec<agent::Part>, owner: &str) -> Result<Vec<a2a::Part>, String> {
    if parts.is_empty() {
        return Err(format!("the agent sent {owner} with no parts"));
    }
    parts.into_iter().map(a2a_part).collect()
}

fn a2a_part(part: agent::Part) -> Result<a2a::Part, String> {
    Ok(a2a::Part {
        content: match part.content {
            agent::PartContent::Text(text) => a2a::PartContent::Text(text),
            agent::PartContent::Raw(raw) => a2a::PartContent::Raw(raw),
            agent::PartContent::Url(url) => a2a::PartContent::Url(url),
            agent::PartContent::Data(data) => a2a::PartContent::Data(json(&data)?),
        },
        metadata: json_object(part.metadata, "a part")?,
        filename: part.filename,
        media_type: part.media_type,
    })
}

// JSON that the agent sent as text.
fn json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("the agent sent invalid JSON: {e}"))
}

// The metadata of `owner`, e.g. "a message", which is a JSON object in A2A.
fn json_object(text: Option<String>, owner: &str) -> Result<Option<a2a::Struct>, String> {
    text.map(|text| match json(&text)? {
        Value::Object(object) => Ok(object),
        _ => Err(format!(
            "the agent sent {owner} with metadata that is not a JSON object"
        )),
    })
    .transpose()
}

/// The current time in UTC, as an RFC 3339 timestamp to the millisecond.
pub(crate) fn now() -> String {
    let now = OffsetDateTime::now_utc();
    now.replace_millisecond(now.millisecond())
        .unwrap_or(now)
        .format(&Rfc3339)
        .expect("a UTC time has an RFC 3339 form")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text_part(text: &str, metadata: Option<&str>) -> agent::Part {
        agent::Part {
            content: agent::PartContent::Text(text.to_string()),
            metadata: metadata.map(str::to_string),
            filename: None,
            media_type: None,
        }
    }

    fn message_from_agent(parts: Vec<agent::Part>) -> agent::Message {
        agent::Message {
            message_id: "m2".to_string(),
            context_id: Some("c1".to_string()),
            task_id: Some("t1".to_string()),
            role: agent::Role::Agent,
            parts,
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }
    }

    fn artifact(parts: Vec<agent::Part>) -> agent::Artifact {
        agent::Artifact {
            artifact_id: "a1".to_string(),
            name: None,
            description: None,
            parts,
            metadata: None,
            extensions: Vec::new(),
        }
    }

    #[test]
    fn a_client_message_reaches_the_agent_field_for_field() {
        let message: a2a::Message = serde_json::from_value(json!({
            "messageId": "m1",
            "contextId": "c1",
            "role": "ROLE_USER",
            "parts": [
                { "text": "Describe this.", "metadata": { "k": 1 } },
                { "raw": "iVBORw0KGgo=", "mediaType": "image/png", "filename": "a.png" },
                { "data": { "a": 1 } },
            ],
            "metadata": { "k": "v" },
            "referenceTaskIds": ["t0"],
        }))
        .unwrap();

        let message = agent_message(message);

        assert_eq!(message.message_id, "m1");
        assert_eq!(message.context_id.as_deref(), Some("c1"));
        assert!(matches!(message.role, agent::Role::User));
        assert!(matches!(
            &message.parts[0].content,
            agent::PartContent::Text(text) if text == "Describe this."
        ));
        assert_eq!(message.parts[0].metadata.as_deref(), Some(r#"{"k":1}"#));
        assert!(matches!(
            &message.parts[1].content,
            agent::PartContent::Raw(raw) if raw == "iVBORw0KGgo="
        ));
        assert_eq!(message.parts[1].media_type.as_deref(), Some("image/png"));
        assert_eq!(message.parts[1].filename.as_deref(), Some("a.png"));
        assert!(matches!(
            &message.parts[2].content,
            agent::PartContent::Data(data) if data == r#"{"a":1}"#
        ));
        assert_eq!(message.metadata.as_deref(), Some(r#"{"k":"v"}"#));
        assert_eq!(message.reference_task_ids, ["t0"]);
    }

    #[test]
    fn a_status_from_the_agent_is_given_a_timestamp_and_keeps_its_message() {
        let reasoning = text_part("Add the numbers.", Some(r#"{"reasoning":true}"#));
        let status = a2a_status(agent::TaskStatus {
            state: agent::TaskState::Working,
            message: Some(message_from_agent(vec![reasoning])),
            timestamp: None,
        })
        .unwrap();

        assert_eq!(status.state, a2a::TaskState::Working);
        let timestamp = status.timestamp.unwrap();
        assert!(
            timestamp.ends_with('Z') && timestamp.contains('T'),
            "{timestamp}"
        );
        // A part's metadata is kept, so a client can recognize reasoning.
        assert_eq!(
            json!(status.message.unwrap()),
            json!({
                "messageId": "m2",
                "contextId": "c1",
                "taskId": "t1",
                "role": "ROLE_AGENT",
                "parts": [{ "text": "Add the numbers.", "metadata": { "reasoning": true } }],
            })
        );
    }

    #[test]
    fn a_status_keeps_the_timestamp_of_the_agent() {
        let status = a2a_status(agent::TaskStatus {
            state: agent::TaskState::Completed,
            message: None,
            timestamp: Some("2026-11-19T10:00:00Z".to_string()),
        })
        .unwrap();
        assert_eq!(status.timestamp.as_deref(), Some("2026-11-19T10:00:00Z"));
    }

    #[test]
    fn each_kind_of_event_becomes_a_stream_response() {
        let status = agent::AgentEvent::Status(agent::TaskStatusUpdateEvent {
            task_id: "t1".to_string(),
            context_id: "c1".to_string(),
            status: agent::TaskStatus {
                state: agent::TaskState::Working,
                message: None,
                timestamp: Some("2026-11-19T10:00:00Z".to_string()),
            },
            metadata: None,
        });
        assert_eq!(
            json!(a2a_event(status).unwrap()),
            json!({ "statusUpdate": {
                "taskId": "t1",
                "contextId": "c1",
                "status": { "state": "TASK_STATE_WORKING", "timestamp": "2026-11-19T10:00:00Z" },
            } })
        );

        let chunk = agent::AgentEvent::Artifact(agent::TaskArtifactUpdateEvent {
            task_id: "t1".to_string(),
            context_id: "c1".to_string(),
            artifact: artifact(vec![text_part("42", None)]),
            append: true,
            last_chunk: false,
            metadata: None,
        });
        assert_eq!(
            json!(a2a_event(chunk).unwrap()),
            json!({ "artifactUpdate": {
                "taskId": "t1",
                "contextId": "c1",
                "artifact": { "artifactId": "a1", "parts": [{ "text": "42" }] },
                "append": true,
            } })
        );
    }

    #[test]
    fn invalid_json_from_the_agent_is_an_error() {
        let data = agent::Part {
            content: agent::PartContent::Data("{not json".to_string()),
            ..text_part("", None)
        };
        let error = a2a_artifact(artifact(vec![data])).unwrap_err();
        assert!(error.starts_with("the agent sent invalid JSON"), "{error}");
    }

    #[test]
    fn metadata_from_the_agent_that_is_not_an_object_is_an_error() {
        let part = text_part("42", Some("[1]"));
        assert_eq!(
            a2a_artifact(artifact(vec![part])).unwrap_err(),
            "the agent sent a part with metadata that is not a JSON object"
        );
    }

    #[test]
    fn a_message_or_an_artifact_from_the_agent_with_no_parts_is_an_error() {
        assert_eq!(
            a2a_artifact(artifact(Vec::new())).unwrap_err(),
            "the agent sent an artifact with no parts"
        );
        let status = agent::TaskStatus {
            state: agent::TaskState::Failed,
            message: Some(message_from_agent(Vec::new())),
            timestamp: None,
        };
        assert_eq!(
            a2a_status(status).unwrap_err(),
            "the agent sent a message with no parts"
        );
    }
}
