//! Message parts as model input, and model output as text.

use crate::composable::agents::types::{Part, PartContent};
use crate::composable::models::types::{ContentSource, MediaData, ModelInputItem, ModelOutputItem};

/// The default media type for file bytes.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// A message's parts as model input. Includes text and structured data as
/// text, and a file as media data with its specified media type, or
/// `application/octet-stream` as the default.
pub fn model_input(parts: &[Part]) -> Vec<ModelInputItem> {
    parts
        .iter()
        .map(|part| match &part.content {
            PartContent::Text(text) => ModelInputItem::Text(text.clone()),
            PartContent::Data(json) => ModelInputItem::Text(json.clone()),
            PartContent::Raw(data) => file_input(
                part.media_type.as_deref(),
                ContentSource::Data(MediaData {
                    mime_type: part
                        .media_type
                        .clone()
                        .unwrap_or_else(|| OCTET_STREAM.to_string()),
                    data: data.clone(),
                }),
            ),
            PartContent::Url(url) => {
                file_input(part.media_type.as_deref(), ContentSource::Url(url.clone()))
            }
        })
        .collect()
}

// A file as an image, audio, or video item based on its media type, otherwise
// a document.
fn file_input(media_type: Option<&str>, source: ContentSource) -> ModelInputItem {
    match media_type.and_then(|media_type| media_type.split('/').next()) {
        Some("image") => ModelInputItem::Image(source),
        Some("audio") => ModelInputItem::Audio(source),
        Some("video") => ModelInputItem::Video(source),
        _ => ModelInputItem::Document(source),
    }
}

/// The text of a response's output.
pub fn response_text(output: &[ModelOutputItem]) -> String {
    output
        .iter()
        .filter_map(|item| match item {
            ModelOutputItem::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::models::types::Reasoning;

    #[test]
    fn parts_become_model_input_by_media_type() {
        let part = |content: PartContent, media_type: Option<&str>| Part {
            content,
            metadata: None,
            filename: None,
            media_type: media_type.map(str::to_string),
        };
        let input = model_input(&[
            part(PartContent::Text("Describe this.".to_string()), None),
            part(PartContent::Data(r#"{"a":1}"#.to_string()), None),
            part(
                PartContent::Raw("iVBORw0KGgo=".to_string()),
                Some("image/png"),
            ),
            part(PartContent::Raw("ABC".to_string()), None),
            part(
                PartContent::Url("https://example.com/a.mp3".to_string()),
                Some("audio/mpeg"),
            ),
            part(
                PartContent::Url("https://example.com/a.mp4".to_string()),
                Some("video/mp4"),
            ),
            part(
                PartContent::Url("https://example.com/a.pdf".to_string()),
                None,
            ),
        ]);

        let expected = vec![
            ModelInputItem::Text("Describe this.".to_string()),
            ModelInputItem::Text(r#"{"a":1}"#.to_string()),
            ModelInputItem::Image(ContentSource::Data(MediaData {
                mime_type: "image/png".to_string(),
                data: "iVBORw0KGgo=".to_string(),
            })),
            ModelInputItem::Document(ContentSource::Data(MediaData {
                mime_type: OCTET_STREAM.to_string(),
                data: "ABC".to_string(),
            })),
            ModelInputItem::Audio(ContentSource::Url("https://example.com/a.mp3".to_string())),
            ModelInputItem::Video(ContentSource::Url("https://example.com/a.mp4".to_string())),
            ModelInputItem::Document(ContentSource::Url("https://example.com/a.pdf".to_string())),
        ];
        assert_eq!(format!("{input:?}"), format!("{expected:?}"));
    }

    #[test]
    fn response_text_joins_the_text_items() {
        let output = [
            ModelOutputItem::Reasoning(Reasoning {
                text: "Think.".to_string(),
                redacted: false,
            }),
            ModelOutputItem::Text("Hello".to_string()),
            ModelOutputItem::Text(", World".to_string()),
        ];
        assert_eq!(response_text(&output), "Hello, World");
    }
}
