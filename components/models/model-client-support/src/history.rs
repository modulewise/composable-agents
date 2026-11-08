//! Checks and notes for a call's input and a session's history. Input a client
//! can't send fails a new call, and becomes a note in history.

use crate::composable::models::types::{
    ContentSource, ModelError, ModelExchange, ModelInputItem, ModelOutputItem, TokenUsage,
};

/// Fails on a tool call in `history` without a tool result in the next
/// exchange's request.
pub fn require_answered_tool_calls(history: &[ModelExchange]) -> Result<(), ModelError> {
    for (i, exchange) in history.iter().enumerate() {
        let answers = history
            .get(i + 1)
            .map_or(&[][..], |next| next.request.input.as_slice());
        for item in exchange.output.iter().flatten() {
            if let ModelOutputItem::ToolCall(call) = item
                && !answers.iter().any(|input| {
                    matches!(input, ModelInputItem::ToolResult(result) if result.tool_call_id == call.id)
                })
            {
                return Err(ModelError {
                    message: format!(
                        "tool call '{}' to '{}' has no result in the next exchange",
                        call.id, call.name
                    ),
                    status: None,
                    usage: TokenUsage::default(),
                });
            }
        }
    }
    Ok(())
}

/// The error for a new call's input of `kind` that the client cannot send.
pub fn unsendable_input_error(kind: &str, source: &ContentSource) -> ModelError {
    ModelError {
        message: format!(
            "this client cannot send {kind} input ({})",
            media_detail(source)
        ),
        status: None,
        usage: TokenUsage::default(),
    }
}

/// A note in place of history input of `kind` that the client cannot send.
pub fn history_input_note(kind: &str, source: &ContentSource) -> String {
    format!(
        "[This turn included {kind} input ({}), which is left out of the history sent to this model.]",
        media_detail(source)
    )
}

/// A note in place of tool-result media the client can't send.
pub fn tool_result_media_note(source: &ContentSource) -> String {
    format!(
        "[The tool returned media ({}), which is left out of the result sent to this model.]",
        media_detail(source)
    )
}

/// A note in place of media output, which clients don't send back in
/// history. `None` for an item that isn't media.
pub fn output_media_note(item: &ModelOutputItem) -> Option<String> {
    let (kind, source) = match item {
        ModelOutputItem::Image(source) => ("image", source),
        ModelOutputItem::Audio(source) => ("audio", source),
        ModelOutputItem::Video(source) => ("video", source),
        ModelOutputItem::Document(source) => ("document", source),
        _ => return None,
    };
    Some(format!(
        "[This turn produced {kind} output ({}), which is left out of the history sent to this model.]",
        media_detail(source)
    ))
}

/// The media type or URL that a note shows.
pub fn media_detail(source: &ContentSource) -> &str {
    match source {
        ContentSource::Data(media) => &media.mime_type,
        ContentSource::Url(url) => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::models::types::{MediaData, ModelRequest, ToolCall, ToolResult};

    fn exchange(input: Vec<ModelInputItem>, output: Option<Vec<ModelOutputItem>>) -> ModelExchange {
        ModelExchange {
            request: ModelRequest {
                input,
                max_tokens: None,
            },
            output,
            opaque_content: None,
            provider: Some("some-provider".into()),
            request_model: Some("some-model".into()),
            response_model: None,
            usage: TokenUsage::default(),
        }
    }

    #[test]
    fn history_needs_a_result_for_every_tool_call() {
        let call = |id: &str| {
            ModelOutputItem::ToolCall(ToolCall {
                id: id.into(),
                name: "calc.add".into(),
                arguments: "{}".into(),
            })
        };
        let result = |id: &str| {
            ModelInputItem::ToolResult(ToolResult {
                tool_call_id: id.into(),
                name: "calc.add".into(),
                content: Vec::new(),
                is_error: true,
            })
        };
        let question = || vec![ModelInputItem::Text("Add.".into())];

        let answered = vec![
            exchange(question(), Some(vec![call("1"), call("2")])),
            exchange(vec![result("1"), result("2")], None),
        ];
        assert!(require_answered_tool_calls(&answered).is_ok());

        let unanswered = vec![exchange(question(), Some(vec![call("1")]))];
        assert_eq!(
            require_answered_tool_calls(&unanswered)
                .unwrap_err()
                .message,
            "tool call '1' to 'calc.add' has no result in the next exchange"
        );

        let partly_answered = vec![
            exchange(question(), Some(vec![call("1"), call("2")])),
            exchange(
                vec![result("1")],
                Some(vec![ModelOutputItem::Text("3".into())]),
            ),
        ];
        assert_eq!(
            require_answered_tool_calls(&partly_answered)
                .unwrap_err()
                .message,
            "tool call '2' to 'calc.add' has no result in the next exchange"
        );
    }

    #[test]
    fn notes_name_media_by_its_type_or_its_url() {
        let data = ContentSource::Data(MediaData {
            mime_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        });
        let url = ContentSource::Url("https://example.com/a.mp3".into());

        assert_eq!(
            unsendable_input_error("audio", &url).message,
            "this client cannot send audio input (https://example.com/a.mp3)"
        );
        assert_eq!(
            history_input_note("audio", &url),
            "[This turn included audio input (https://example.com/a.mp3), which is left out of the history sent to this model.]"
        );
        assert_eq!(
            tool_result_media_note(&data),
            "[The tool returned media (image/png), which is left out of the result sent to this model.]"
        );
        assert_eq!(
            output_media_note(&ModelOutputItem::Image(data)).as_deref(),
            Some(
                "[This turn produced image output (image/png), which is left out of the history sent to this model.]"
            )
        );
        assert_eq!(output_media_note(&ModelOutputItem::Text("Hi".into())), None);
    }
}
