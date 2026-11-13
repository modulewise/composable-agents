//! Mapping between the `composable:models/types` WIT and Anthropic's Messages
//! API JSON, using SSE streaming.

use std::convert::Infallible;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use model_client_support::sse::{self, Events};
use model_client_support::types::{
    ContentDelta, ContentSource, MediaData, ModelDelta, ModelError, ModelExchange,
    ModelFinishReason, ModelInputItem, ModelOutputItem, ModelResponse, ModelSettings,
    OpaqueContent, Reasoning, ResultContent, TokenUsage, ToolCall, ToolCallDelta, ToolChoice,
    ToolDefinition,
};
use model_client_support::{
    Deltas, history_input_note, json_error_position, output_media_note, tool_result_media_note,
    unsendable_input_error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wit_bindgen::rt::async_support::StreamReader;

/// The `api` value of this client's opaque content.
pub(crate) const API: &str = "anthropic-messages";

// === Outgoing request =======================================================

/// One turn of the conversation, as the Messages API expects it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Message {
    role: &'static str,
    content: Content,
}

// A turn's content: blocks this client builds, or an assistant turn's blocks
// exactly as received, which preserved thinking requires.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
enum Content {
    Blocks(Vec<Block>),
    Received(Vec<Value>),
}

// A content block this client builds: for a user turn, or for an assistant
// turn rebuilt from another client's output.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        text: String,
    },
    Image {
        source: Source,
    },
    Document {
        source: Source,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<Block>,
        is_error: bool,
    },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Source {
    Base64 {
        media_type: String,
        data: String,
    },
    Text {
        media_type: &'static str,
        data: String,
    },
    Url {
        url: String,
    },
}

impl From<&ContentSource> for Source {
    fn from(source: &ContentSource) -> Self {
        match source {
            ContentSource::Data(MediaData { mime_type, data }) => Source::Base64 {
                media_type: mime_type.clone(),
                data: data.clone(),
            },
            ContentSource::Url(url) => Source::Url { url: url.clone() },
        }
    }
}

// A document as the Messages API expects it: a PDF as base64, text as plain
// text, or a URL. None for other types.
fn document_source(source: &ContentSource) -> Option<Source> {
    match source {
        ContentSource::Data(media) if media.mime_type == "application/pdf" => Some(source.into()),
        ContentSource::Data(media) if media.mime_type.starts_with("text/") => {
            let text = String::from_utf8(BASE64.decode(&media.data).ok()?).ok()?;
            Some(Source::Text {
                media_type: "text/plain",
                data: text,
            })
        }
        ContentSource::Data(_) => None,
        ContentSource::Url(url) => Some(Source::Url { url: url.clone() }),
    }
}

/// A new call's input as one user turn, or none if the input is empty. Fails
/// on audio, video, and documents other than PDF and text, before the call is
/// recorded.
pub(crate) fn input_items_to_message(
    input: &[ModelInputItem],
) -> Result<Option<Message>, ModelError> {
    input_message(input, |kind, source| {
        Err(unsendable_input_error(kind, source))
    })
}

// A call's input as one user turn, with any tool results first, as the API
// requires. `unsendable` returns an error or a text replacement for input
// this client cannot send.
fn input_message<E>(
    input: &[ModelInputItem],
    unsendable: impl Fn(&str, &ContentSource) -> Result<String, E>,
) -> Result<Option<Message>, E> {
    let mut content = Vec::new();
    for item in input {
        content.push(match item {
            ModelInputItem::Text(text) => Block::Text { text: text.clone() },
            ModelInputItem::Image(source) => Block::Image {
                source: source.into(),
            },
            ModelInputItem::Document(source) => match document_source(source) {
                Some(source) => Block::Document { source },
                None => Block::Text {
                    text: unsendable("document", source)?,
                },
            },
            ModelInputItem::ToolResult(result) => Block::ToolResult {
                tool_use_id: result.tool_call_id.clone(),
                content: result.content.iter().map(result_block).collect(),
                is_error: result.is_error,
            },
            ModelInputItem::Audio(source) => Block::Text {
                text: unsendable("audio", source)?,
            },
            ModelInputItem::Video(source) => Block::Text {
                text: unsendable("video", source)?,
            },
        });
    }
    content.sort_by_key(|block| !matches!(block, Block::ToolResult { .. }));
    Ok((!content.is_empty()).then_some(Message {
        role: "user",
        content: Content::Blocks(content),
    }))
}

// A tool result's content as a block: text, an image or document given as
// data, or a note for other media, including any URL, whose media type is
// unknown.
fn result_block(content: &ResultContent) -> Block {
    match content {
        ResultContent::Text(text) => Block::Text { text: text.clone() },
        ResultContent::Media(source @ ContentSource::Data(media))
            if media.mime_type.starts_with("image/") =>
        {
            Block::Image {
                source: source.into(),
            }
        }
        ResultContent::Media(source @ ContentSource::Data(_)) => match document_source(source) {
            Some(source) => Block::Document { source },
            None => Block::Text {
                text: tool_result_media_note(source),
            },
        },
        ResultContent::Media(source) => Block::Text {
            text: tool_result_media_note(source),
        },
    }
}

/// The messages a session starts from: each earlier exchange's input as a user
/// turn and its output as an assistant turn. Output with this client's opaque
/// content is sent exactly as received; other output is rebuilt without
/// reasoning. Fails on opaque content that isn't a list of blocks.
pub(crate) fn history_to_messages(history: &[ModelExchange]) -> Result<Vec<Message>, ModelError> {
    let mut messages = Vec::new();
    for exchange in history {
        let Ok(input) = input_message(&exchange.request.input, |kind, source| {
            Ok::<_, Infallible>(history_input_note(kind, source))
        });
        messages.extend(input);
        match (&exchange.opaque_content, &exchange.output) {
            (Some(opaque), _) if opaque.api == API => {
                let blocks: Vec<Value> = serde_json::from_str(&opaque.content).map_err(|e| {
                    failure(format!(
                        "history holds {API} content that is not a list of blocks ({e})"
                    ))
                })?;
                messages.push(Message {
                    role: "assistant",
                    content: Content::Received(blocks),
                });
            }
            (_, Some(output)) => messages.extend(output_items_to_message(output)),
            (_, None) => {}
        }
    }
    Ok(messages)
}

// Output without this client's opaque content as one assistant turn, or none
// if there's nothing to send. Reasoning is left out, because the API accepts
// thinking only exactly as it sent it. Media becomes a note.
fn output_items_to_message(output: &[ModelOutputItem]) -> Option<Message> {
    let content: Vec<Block> = output
        .iter()
        .filter_map(|item| match item {
            ModelOutputItem::Text(text) if !text.is_empty() => {
                Some(Block::Text { text: text.clone() })
            }
            ModelOutputItem::Text(_) | ModelOutputItem::Reasoning(_) => None,
            ModelOutputItem::ToolCall(call) => Some(Block::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({})),
            }),
            item @ (ModelOutputItem::Image(_)
            | ModelOutputItem::Audio(_)
            | ModelOutputItem::Video(_)
            | ModelOutputItem::Document(_)) => {
                output_media_note(item).map(|text| Block::Text { text })
            }
        })
        .collect();
    (!content.is_empty()).then_some(Message {
        role: "assistant",
        content: Content::Blocks(content),
    })
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig<'a>>,
}

// `thinking`, sent when `include-reasoning` is true: adaptive thinking with
// summarized text. This also turns thinking on where it's off by default, and
// a model without adaptive thinking rejects it.
#[derive(Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    kind: &'static str,
    display: &'static str,
}

/// The `effort` config, sent as `output_config.effort`. A model that doesn't
/// support the level rejects the request.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl std::str::FromStr for Effort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, ()> {
        match value {
            "low" => Ok(Effort::Low),
            "medium" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            "xhigh" => Ok(Effort::Xhigh),
            "max" => Ok(Effort::Max),
            _ => Err(()),
        }
    }
}

/// A valid `effort` value, for the error on an invalid one.
pub(crate) const EFFORT_VALUES: &str = "one of low, medium, high, xhigh or max";

// `output_config`, sent when either part is set.
#[derive(Serialize)]
struct OutputConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<Effort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'a OutputFormat>,
}

// Structured output: `{"type":"json_schema","schema":{...}}`.
#[derive(Serialize)]
struct OutputFormat {
    #[serde(rename = "type")]
    kind: &'static str,
    schema: Value,
}

// `model-settings.output-schema` as a JSON Schema output format. A schema the
// API doesn't support fails at the provider.
fn output_format(output_schema: &str) -> Result<OutputFormat, ModelError> {
    let schema = serde_json::from_str(output_schema)
        .map_err(|e| failure(format!("the output-schema is invalid JSON: {e}")))?;
    Ok(OutputFormat {
        kind: "json_schema",
        schema,
    })
}

#[derive(Serialize)]
struct Tool {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    input_schema: Value,
}

/// A session's settings in Messages API form, with the instructions as
/// `system`. `temperature`, `top-p`, `top-k` and `seed` aren't sent, because
/// the API rejects the first three and has no `seed`.
pub(crate) struct ApiSettings {
    system: Option<String>,
    stop_sequences: Option<Vec<String>>,
    tools: Option<Vec<Tool>>,
    tool_choice: Option<Value>,
    format: Option<OutputFormat>,
}

impl ApiSettings {
    /// Fails on an invalid input-schema or output-schema.
    pub(crate) fn new(settings: &ModelSettings) -> Result<Self, ModelError> {
        Ok(ApiSettings {
            system: settings
                .instructions
                .clone()
                .filter(|instructions| !instructions.is_empty()),
            stop_sequences: settings.stop_sequences.clone(),
            tools: settings
                .tools
                .as_ref()
                .map(|definitions| {
                    definitions
                        .iter()
                        .map(to_tool)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            tool_choice: settings.tool_choice.as_ref().map(to_tool_choice),
            format: settings
                .output_schema
                .as_deref()
                .map(output_format)
                .transpose()?,
        })
    }
}

/// The request body for one call.
pub(crate) fn build_request(
    model: &str,
    history: &[Message],
    api_settings: &ApiSettings,
    effort: Option<Effort>,
    include_reasoning: bool,
    max_tokens: Option<u32>,
) -> Result<Vec<u8>, ModelError> {
    let format = api_settings.format.as_ref();
    let output_config =
        (effort.is_some() || format.is_some()).then_some(OutputConfig { effort, format });

    let request = MessagesRequest {
        model,
        max_tokens,
        system: api_settings.system.as_deref(),
        messages: history,
        stream: true,
        stop_sequences: api_settings.stop_sequences.as_deref(),
        tools: api_settings.tools.as_deref(),
        tool_choice: api_settings.tool_choice.as_ref(),
        thinking: include_reasoning.then_some(Thinking {
            kind: "adaptive",
            display: "summarized",
        }),
        output_config,
    };
    serde_json::to_vec(&request).map_err(|e| failure(format!("failed to serialize request: {e}")))
}

// A tool definition in Messages API form, with its input schema parsed into
// JSON. An invalid schema fails the session.
fn to_tool(definition: &ToolDefinition) -> Result<Tool, ModelError> {
    let input_schema = serde_json::from_str(&definition.input_schema).map_err(|e| {
        failure(format!(
            "tool '{}' has an invalid input-schema: {e}",
            definition.name
        ))
    })?;
    Ok(Tool {
        name: definition.name.clone(),
        description: (!definition.description.is_empty()).then(|| definition.description.clone()),
        input_schema,
    })
}

fn to_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Specific(name) => json!({ "type": "tool", "name": name }),
    }
}

// === Incoming SSE stream ====================================================

// One stream event. Its data's `type` matches the SSE event name.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    MessageStart {
        message: StartMessage,
    },
    // The block as received, kept whole for replay.
    ContentBlockStart {
        index: u32,
        content_block: Value,
    },
    ContentBlockDelta {
        index: u32,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    Error {
        error: ErrorBody,
    },
    // `ping`, and any unknown event type.
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct StartMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

// A block's type and starting content, for the output and deltas.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StartBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    RedactedThinking {},
    ToolUse {
        id: String,
        name: String,
    },
    // Server tool blocks, and any others with no output item.
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    CitationsDelta {
        citation: Value,
    },
    // Any unknown delta type.
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct MessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

// Usage counts. `message_delta` reports them cumulatively, so each later value
// replaces the earlier one.
#[derive(Deserialize, Debug, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Deserialize, Debug)]
struct OutputTokensDetails {
    #[serde(default)]
    thinking_tokens: Option<u64>,
}

impl Usage {
    fn update(&mut self, later: Usage) {
        self.input_tokens = later.input_tokens.or(self.input_tokens);
        self.output_tokens = later.output_tokens.or(self.output_tokens);
        self.cache_creation_input_tokens = later
            .cache_creation_input_tokens
            .or(self.cache_creation_input_tokens);
        self.cache_read_input_tokens = later
            .cache_read_input_tokens
            .or(self.cache_read_input_tokens);
        self.output_tokens_details = later
            .output_tokens_details
            .or(self.output_tokens_details.take());
    }
}

#[derive(Deserialize, Debug)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

// The response so far, assembled from the stream's events.
#[derive(Default)]
struct Accumulator {
    response_id: Option<String>,
    model: Option<String>,
    // The content blocks, by their index in the message.
    blocks: Vec<AccBlock>,
    stop_reason: Option<String>,
    usage: Usage,
    stopped: bool,
}

// One content block: as `content_block_start` sent it, with its deltas
// applied. Its kind sets what it becomes in the output.
#[derive(Default, Debug)]
struct AccBlock {
    received: Value,
    // A block's input as streamed, parsed into `received` when the block stops.
    input_json: String,
    kind: Kind,
}

#[derive(Default, Debug)]
enum Kind {
    #[default]
    Other,
    Text,
    Thinking,
    RedactedThinking,
    ToolUse {
        id: String,
        name: String,
    },
}

impl Accumulator {
    fn block(&mut self, index: u32) -> &mut AccBlock {
        let index = index as usize;
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, AccBlock::default);
        }
        &mut self.blocks[index]
    }
}

// Appends `more` to the string field `key` of `block`.
fn append(block: &mut Value, key: &str, more: &str) {
    if let Some(Value::String(value)) = block.get_mut(key) {
        value.push_str(more);
    } else if let Some(fields) = block.as_object_mut() {
        fields.insert(key.to_string(), Value::String(more.to_string()));
    }
}

/// Reads the SSE body, writing deltas to `deltas`, and returns the assembled
/// response and the assistant turn for history (none if the response has no
/// content). If the caller stops reading the deltas, it fails and drops the
/// body, closing the connection.
pub(crate) async fn stream_response(
    body: StreamReader<u8>,
    deltas: &mut Deltas,
) -> Result<(ModelResponse, Option<Message>), ModelError> {
    let mut acc = Accumulator::default();
    let mut events = Events::new(body);
    while let Some(event) = events.next().await {
        for delta in apply(&mut acc, parse_event(&event)?)? {
            deltas.write(delta).await?;
        }
    }
    assemble(acc)
}

// The Messages API event in an SSE event. Fails if the event name is missing
// or doesn't match the data's `type`, or if the data isn't a valid event.
// Errors name the event, never its data.
fn parse_event(event: &sse::Event) -> Result<Event, ModelError> {
    let name = event.event_type.as_deref().unwrap_or_default();
    let data: Value = serde_json::from_str(&event.data).map_err(|e| {
        failure(format!(
            "the provider sent a {name:?} event that is not valid JSON ({})",
            json_error_position(&e)
        ))
    })?;
    if name.is_empty() || data.get("type").and_then(Value::as_str) != Some(name) {
        return Err(failure(format!(
            "the provider sent an event named {name:?} whose data has another type"
        )));
    }
    serde_json::from_value(data).map_err(|_| {
        failure(format!(
            "the provider sent a {name:?} event that is not valid"
        ))
    })
}

// Applies one event to the response so far, returning its deltas. An event
// that reports token counts returns a usage delta with the counts so far. An
// error event fails the call immediately.
fn apply(acc: &mut Accumulator, event: Event) -> Result<Vec<ModelDelta>, ModelError> {
    Ok(match event {
        Event::MessageStart { message } => {
            acc.response_id = message.id;
            acc.model = message.model;
            match message.usage {
                Some(usage) => {
                    acc.usage.update(usage);
                    vec![ModelDelta::Usage(token_usage(&acc.usage))]
                }
                None => Vec::new(),
            }
        }
        Event::ContentBlockStart {
            index,
            content_block,
        } => {
            let start: StartBlock =
                serde_json::from_value(content_block.clone()).map_err(|_| {
                    failure(format!(
                        "the provider sent content block {index} that is not valid"
                    ))
                })?;
            let (kind, delta) = match start {
                StartBlock::Text { text } => (
                    Kind::Text,
                    (!text.is_empty()).then_some(ContentDelta::Text(text)),
                ),
                StartBlock::Thinking { thinking } => (
                    Kind::Thinking,
                    (!thinking.is_empty()).then_some(ContentDelta::Reasoning(thinking)),
                ),
                StartBlock::RedactedThinking {} => (Kind::RedactedThinking, None),
                StartBlock::ToolUse { id, name } => {
                    let delta = ContentDelta::ToolCall(ToolCallDelta {
                        index,
                        id: Some(id.clone()),
                        name: Some(name.clone()),
                        arguments: String::new(),
                    });
                    (Kind::ToolUse { id, name }, Some(delta))
                }
                StartBlock::Other => (Kind::Other, None),
            };
            *acc.block(index) = AccBlock {
                received: content_block,
                input_json: String::new(),
                kind,
            };
            delta.into_iter().map(ModelDelta::Content).collect()
        }
        Event::ContentBlockDelta { index, delta } => {
            let block = acc.block(index);
            let delta = match (&block.kind, delta) {
                (Kind::Text, BlockDelta::TextDelta { text }) => {
                    append(&mut block.received, "text", &text);
                    Some(ContentDelta::Text(text))
                }
                (Kind::Text, BlockDelta::CitationsDelta { citation }) => {
                    match block.received.get_mut("citations") {
                        Some(Value::Array(citations)) => citations.push(citation),
                        _ => {
                            if let Some(fields) = block.received.as_object_mut() {
                                fields
                                    .insert("citations".to_string(), Value::Array(vec![citation]));
                            }
                        }
                    }
                    None
                }
                (Kind::Thinking, BlockDelta::ThinkingDelta { thinking }) => {
                    append(&mut block.received, "thinking", &thinking);
                    Some(ContentDelta::Reasoning(thinking))
                }
                (Kind::Thinking, BlockDelta::SignatureDelta { signature }) => {
                    if let Some(fields) = block.received.as_object_mut() {
                        fields.insert("signature".to_string(), Value::String(signature));
                    }
                    None
                }
                // Tool and server tool input streams as partial JSON.
                (kind, BlockDelta::InputJsonDelta { partial_json }) => {
                    block.input_json.push_str(&partial_json);
                    matches!(kind, Kind::ToolUse { .. }).then(|| {
                        ContentDelta::ToolCall(ToolCallDelta {
                            index,
                            id: None,
                            name: None,
                            arguments: partial_json,
                        })
                    })
                }
                _ => None,
            };
            delta.into_iter().map(ModelDelta::Content).collect()
        }
        // The streamed input replaces the block's starting input. Its key order
        // is kept, so it's replayed unchanged.
        Event::ContentBlockStop { index } => {
            let block = acc.block(index);
            if !block.input_json.is_empty() {
                let input: Value = serde_json::from_str(&block.input_json).map_err(|e| {
                    failure(format!(
                        "the provider sent tool input for block {index} that is not valid JSON ({})",
                        json_error_position(&e)
                    ))
                })?;
                if let Some(fields) = block.received.as_object_mut() {
                    fields.insert("input".to_string(), input);
                }
            }
            Vec::new()
        }
        Event::MessageDelta { delta, usage } => {
            if delta.stop_reason.is_some() {
                acc.stop_reason = delta.stop_reason;
            }
            match usage {
                Some(usage) => {
                    acc.usage.update(usage);
                    vec![ModelDelta::Usage(token_usage(&acc.usage))]
                }
                None => Vec::new(),
            }
        }
        Event::MessageStop => {
            acc.stopped = true;
            Vec::new()
        }
        Event::Error { error } => {
            return Err(failure(format!("{}: {}", error.kind, error.message)));
        }
        Event::Other => Vec::new(),
    })
}

// The final response and the assistant turn for history: the blocks exactly
// as received, also kept as the response's opaque content. Fails if the
// stream ended before `message_stop`.
fn assemble(acc: Accumulator) -> Result<(ModelResponse, Option<Message>), ModelError> {
    if !acc.stopped {
        return Err(failure(
            "the response stream ended before the message was complete".to_string(),
        ));
    }

    let mut output = Vec::new();
    let mut received = Vec::new();
    for block in acc.blocks {
        match block.kind {
            Kind::Text => {
                let text = block.received["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    output.push(ModelOutputItem::Text(text.to_string()));
                }
            }
            Kind::Thinking => {
                output.push(ModelOutputItem::Reasoning(Reasoning {
                    text: block.received["thinking"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    redacted: false,
                }));
            }
            Kind::RedactedThinking => {
                output.push(ModelOutputItem::Reasoning(Reasoning {
                    text: String::new(),
                    redacted: true,
                }));
            }
            Kind::ToolUse { id, name } => {
                let arguments = if block.input_json.is_empty() {
                    "{}".to_string()
                } else {
                    block.input_json
                };
                output.push(ModelOutputItem::ToolCall(ToolCall {
                    id,
                    name,
                    arguments,
                }));
            }
            Kind::Other => {}
        }
        // Skips a block index the stream never started.
        if !block.received.is_null() {
            received.push(block.received);
        }
    }

    let opaque_content = (!received.is_empty())
        .then(|| {
            serde_json::to_string(&received)
                .map(|content| OpaqueContent {
                    api: API.to_string(),
                    content,
                })
                .map_err(|e| failure(format!("failed to keep the response's content: {e}")))
        })
        .transpose()?;
    let assistant = (!received.is_empty()).then_some(Message {
        role: "assistant",
        content: Content::Received(received),
    });

    let raw = acc.stop_reason.unwrap_or_default();
    let response = ModelResponse {
        output,
        finish_reason: finish_reason(&raw),
        usage: token_usage(&acc.usage),
        response_id: acc.response_id,
        model: acc.model,
        opaque_content,
    };
    Ok((response, assistant))
}

// The token counts reported so far. `input_tokens` counts only what follows
// the last cache breakpoint, so the input count adds the cache read and write
// counts when reported.
fn token_usage(usage: &Usage) -> TokenUsage {
    let cache_read = usage.cache_read_input_tokens;
    let cache_write = usage.cache_creation_input_tokens;
    let input_tokens = usage
        .input_tokens
        .map(|input| input + cache_read.unwrap_or(0) + cache_write.unwrap_or(0));
    let output_tokens = usage.output_tokens;
    TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.thinking_tokens),
    }
}

fn finish_reason(raw: &str) -> ModelFinishReason {
    let raw_value = raw.to_string();
    match raw {
        "end_turn" => ModelFinishReason::Completed(raw_value),
        "tool_use" => ModelFinishReason::ToolRequested(raw_value),
        "max_tokens" | "model_context_window_exceeded" => ModelFinishReason::Truncated(raw_value),
        "stop_sequence" => ModelFinishReason::Stopped(raw_value),
        "refusal" => ModelFinishReason::Blocked(raw_value),
        "pause_turn" => ModelFinishReason::Paused(raw_value),
        _ => ModelFinishReason::Other(raw_value),
    }
}

fn failure(message: String) -> ModelError {
    ModelError {
        message,
        status: None,
        usage: TokenUsage::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_client_support::types::{ModelRequest, ToolResult};

    // What a stream assembles into.
    type Assembled = Result<(ModelResponse, Option<Message>), ModelError>;

    // A stop reason and the finish-reason variant it maps to.
    type StopReasonCase = (&'static str, fn(String) -> ModelFinishReason);

    fn settings() -> ModelSettings {
        ModelSettings {
            instructions: None,
            tools: None,
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

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("The {name} tool."),
            input_schema: r#"{"type":"object"}"#.to_string(),
        }
    }

    fn request(settings: &ModelSettings, history: &[Message]) -> Value {
        let api_settings = ApiSettings::new(settings).unwrap();
        let body = build_request(
            "claude-test",
            history,
            &api_settings,
            None,
            false,
            Some(4096),
        );
        serde_json::from_slice(&body.unwrap()).unwrap()
    }

    // Folds a stream into the response and returns its deltas. Each event is
    // given as its data and sent as the API sends it: named by the data's
    // `type`, ending with a blank line.
    fn fold(events: &[&str]) -> (Vec<ModelDelta>, Assembled) {
        let stream: String = events
            .iter()
            .map(|data| {
                let data_value: Value = serde_json::from_str(data).unwrap();
                format!(
                    "event: {}\ndata: {data}\n\n",
                    data_value["type"].as_str().unwrap()
                )
            })
            .collect();
        let mut acc = Accumulator::default();
        let mut deltas = Vec::new();
        for event in sse::Parser::default().feed(stream.as_bytes()) {
            match apply(&mut acc, parse_event(&event).unwrap()) {
                Ok(more) => deltas.extend(more),
                Err(error) => return (deltas, Err(error)),
            }
        }
        (deltas, assemble(acc))
    }

    #[test]
    fn a_request_carries_the_instructions_as_system_and_no_sampling_settings() {
        let settings = ModelSettings {
            instructions: Some("Be brief.".to_string()),
            temperature: Some(0.2),
            top_p: Some(0.9),
            top_k: Some(40),
            seed: Some(7),
            stop_sequences: Some(vec!["END".to_string()]),
            ..settings()
        };
        let history = [
            input_items_to_message(&[ModelInputItem::Text("Hi".to_string())])
                .unwrap()
                .unwrap(),
        ];
        assert_eq!(
            request(&settings, &history),
            json!({
                "model": "claude-test",
                "max_tokens": 4096,
                "system": "Be brief.",
                "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Hi" }] }],
                "stream": true,
                "stop_sequences": ["END"],
            })
        );
    }

    #[test]
    fn including_reasoning_asks_for_summarized_adaptive_thinking() {
        let body = |include_reasoning| -> Value {
            serde_json::from_slice(
                &build_request(
                    "claude-test",
                    &[],
                    &ApiSettings::new(&settings()).unwrap(),
                    None,
                    include_reasoning,
                    Some(4096),
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            body(true)["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert!(body(false).get("thinking").is_none());
    }

    #[test]
    fn an_output_schema_and_effort_go_in_output_config() {
        let schema = r#"{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"],"additionalProperties":false}"#;
        let with_schema = ModelSettings {
            output_schema: Some(schema.to_string()),
            ..settings()
        };
        let body = |settings: &ModelSettings, effort| -> Value {
            serde_json::from_slice(
                &build_request(
                    "claude-test",
                    &[],
                    &ApiSettings::new(settings).unwrap(),
                    effort,
                    false,
                    Some(4096),
                )
                .unwrap(),
            )
            .unwrap()
        };
        let format = json!({
            "type": "json_schema",
            "schema": serde_json::from_str::<Value>(schema).unwrap(),
        });

        assert_eq!(
            body(&with_schema, None)["output_config"],
            json!({ "format": format })
        );
        assert_eq!(
            body(&settings(), Some(Effort::Low))["output_config"],
            json!({ "effort": "low" })
        );
        assert_eq!(
            body(&with_schema, Some(Effort::Xhigh))["output_config"],
            json!({ "effort": "xhigh", "format": format })
        );
        assert!(body(&settings(), None).get("output_config").is_none());

        let invalid = ModelSettings {
            output_schema: Some("{not json".to_string()),
            ..settings()
        };
        let error = ApiSettings::new(&invalid).err().unwrap();
        assert!(
            error
                .message
                .starts_with("the output-schema is invalid JSON"),
            "{}",
            error.message
        );
    }

    #[test]
    fn effort_is_one_of_the_levels_the_api_defines() {
        for (value, effort) in [
            ("low", Effort::Low),
            ("medium", Effort::Medium),
            ("high", Effort::High),
            ("xhigh", Effort::Xhigh),
            ("max", Effort::Max),
        ] {
            assert_eq!(value.parse::<Effort>(), Ok(effort));
        }
        assert!("High".parse::<Effort>().is_err());
        assert!("extreme".parse::<Effort>().is_err());
    }

    #[test]
    fn tools_are_sent_with_the_choice_mapped() {
        let mut settings = ModelSettings {
            tools: Some(vec![tool("calc_add"), tool("greet")]),
            tool_choice: Some(ToolChoice::Specific("calc_add".to_string())),
            ..settings()
        };
        let body = request(&settings, &[]);
        assert_eq!(
            body["tools"],
            json!([
                { "name": "calc_add", "description": "The calc_add tool.", "input_schema": { "type": "object" } },
                { "name": "greet", "description": "The greet tool.", "input_schema": { "type": "object" } },
            ])
        );
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "tool", "name": "calc_add" })
        );

        for (choice, expected) in [
            (ToolChoice::Auto, json!({ "type": "auto" })),
            (ToolChoice::None, json!({ "type": "none" })),
            (ToolChoice::Required, json!({ "type": "any" })),
        ] {
            settings.tool_choice = Some(choice);
            assert_eq!(request(&settings, &[])["tool_choice"], expected);
        }
    }

    #[test]
    fn an_event_misnamed_or_malformed_fails_the_call() {
        let event = |event_type: Option<&str>, data: &str| sse::Event {
            event_type: event_type.map(str::to_string),
            data: data.to_string(),
        };
        assert!(parse_event(&event(Some("message_stop"), r#"{"type": "message_stop"}"#)).is_ok());

        // Each error names the event, never its data.
        for (bad, expected) in [
            (
                event(
                    Some("message_start"),
                    r#"{"type": "message_stop", "secret": "s3cr3t"}"#,
                ),
                r#"the provider sent an event named "message_start" whose data has another type"#,
            ),
            (
                event(None, r#"{"type": "message_stop", "secret": "s3cr3t"}"#),
                r#"the provider sent an event named "" whose data has another type"#,
            ),
            (
                event(Some("ping"), r#"{"secret": "s3cr3t"}"#),
                r#"the provider sent an event named "ping" whose data has another type"#,
            ),
            (
                event(Some("ping"), r#"{"secret": "s3cr3t""#),
                r#"the provider sent a "ping" event that is not valid JSON (line 1, column 19)"#,
            ),
            (
                event(
                    Some("content_block_delta"),
                    r#"{"type": "content_block_delta", "index": "s3cr3t"}"#,
                ),
                r#"the provider sent a "content_block_delta" event that is not valid"#,
            ),
        ] {
            let error = parse_event(&bad).err().unwrap();
            assert_eq!(error.message, expected);
        }
    }

    #[test]
    fn input_becomes_one_user_turn_of_the_blocks_the_api_takes() {
        let image = ContentSource::Data(MediaData {
            mime_type: "image/png".to_string(),
            data: "iVBORw0KGgo=".to_string(),
        });
        let message = input_items_to_message(&[
            ModelInputItem::ToolResult(ToolResult {
                tool_call_id: "toolu_1".to_string(),
                name: "calc_add".to_string(),
                content: vec![
                    ResultContent::Text("42".to_string()),
                    ResultContent::Media(image.clone()),
                    ResultContent::Media(ContentSource::Url(
                        "https://example.com/a.mp3".to_string(),
                    )),
                ],
                is_error: false,
            }),
            ModelInputItem::Image(image),
        ])
        .unwrap();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    { "type": "text", "text": "42" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=" } },
                    { "type": "text", "text": "[The tool returned media (https://example.com/a.mp3), which is left out of the result sent to this model.]" },
                ], "is_error": false },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=" } },
            ] })
        );

        assert_eq!(input_items_to_message(&[]).unwrap(), None);
    }

    #[test]
    fn tool_results_come_first_in_the_turn() {
        let message = input_items_to_message(&[
            ModelInputItem::Text("Here is the result.".to_string()),
            ModelInputItem::ToolResult(ToolResult {
                tool_call_id: "toolu_1".to_string(),
                name: "get_weather".to_string(),
                content: vec![ResultContent::Text("15 degrees".to_string())],
                is_error: false,
            }),
        ])
        .unwrap();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [{ "type": "text", "text": "15 degrees" }], "is_error": false },
                { "type": "text", "text": "Here is the result." },
            ] })
        );
    }

    #[test]
    fn documents_are_sent_as_pdf_text_or_a_url_and_other_types_fail() {
        let data = |mime_type: &str, data: &str| {
            ContentSource::Data(MediaData {
                mime_type: mime_type.to_string(),
                data: data.to_string(),
            })
        };
        let message = input_items_to_message(&[
            ModelInputItem::Document(data("application/pdf", "JVBERi0=")),
            ModelInputItem::Document(data("text/markdown", "IyBIaQ==")),
            ModelInputItem::Document(ContentSource::Url("https://example.com/a.pdf".to_string())),
            ModelInputItem::ToolResult(ToolResult {
                tool_call_id: "toolu_1".to_string(),
                name: "read_file".to_string(),
                content: vec![
                    ResultContent::Media(data("text/plain", "b2s=")),
                    ResultContent::Media(data("application/zip", "UEs=")),
                ],
                is_error: false,
            }),
        ])
        .unwrap();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    { "type": "document", "source": { "type": "text", "media_type": "text/plain", "data": "ok" } },
                    { "type": "text", "text": "[The tool returned media (application/zip), which is left out of the result sent to this model.]" },
                ], "is_error": false },
                { "type": "document", "source": { "type": "base64", "media_type": "application/pdf", "data": "JVBERi0=" } },
                { "type": "document", "source": { "type": "text", "media_type": "text/plain", "data": "# Hi" } },
                { "type": "document", "source": { "type": "url", "url": "https://example.com/a.pdf" } },
            ] })
        );

        let error =
            input_items_to_message(&[ModelInputItem::Document(data("application/msword", "AAA="))])
                .err()
                .unwrap();
        assert_eq!(
            error.message,
            "this client cannot send document input (application/msword)"
        );
    }

    #[test]
    fn a_new_call_fails_on_input_the_client_cannot_send() {
        let error = input_items_to_message(&[
            ModelInputItem::Text("Transcribe this.".to_string()),
            ModelInputItem::Audio(ContentSource::Url("https://example.com/a.mp3".to_string())),
        ])
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "this client cannot send audio input (https://example.com/a.mp3)"
        );
    }

    #[test]
    fn history_notes_input_the_client_cannot_send() {
        let history = vec![ModelExchange {
            request: ModelRequest {
                input: vec![
                    ModelInputItem::Text("Transcribe this.".to_string()),
                    ModelInputItem::Video(ContentSource::Url(
                        "https://example.com/a.mp4".to_string(),
                    )),
                ],
                max_tokens: None,
            },
            output: Some(vec![ModelOutputItem::Text("It shows a cat.".to_string())]),
            opaque_content: None,
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage::default(),
        }];

        assert_eq!(
            serde_json::to_value(&history_to_messages(&history).unwrap()[0]).unwrap(),
            json!({ "role": "user", "content": [
                { "type": "text", "text": "Transcribe this." },
                { "type": "text", "text": "[This turn included video input (https://example.com/a.mp4), which is left out of the history sent to this model.]" },
            ] })
        );
    }

    #[test]
    fn a_message_without_usage_reports_no_counts() {
        let (deltas, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_1", "model": "claude-test"}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}"#,
            r#"{"type": "content_block_stop", "index": 0}"#,
            r#"{"type": "message_delta", "delta": {"stop_reason": "end_turn"}}"#,
            r#"{"type": "message_stop"}"#,
        ]);
        assert!(
            !deltas
                .iter()
                .any(|delta| matches!(delta, ModelDelta::Usage(_)))
        );
        let (response, _) = outcome.unwrap();
        assert_eq!(
            format!("{:?}", response.usage),
            format!("{:?}", TokenUsage::default())
        );
    }

    #[test]
    fn a_streamed_answer_assembles_with_its_usage() {
        let (deltas, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "claude-test", "stop_reason": null, "usage": {"input_tokens": 25, "output_tokens": 1}}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}"#,
            r#"{"type": "ping"}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "!"}}"#,
            r#"{"type": "content_block_stop", "index": 0}"#,
            r#"{"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 15}}"#,
            r#"{"type": "message_stop"}"#,
        ]);
        // Each event that reports counts gives the counts so far.
        let counts = |output: u64| {
            ModelDelta::Usage(TokenUsage {
                input_tokens: Some(25),
                output_tokens: Some(output),
                ..TokenUsage::default()
            })
        };
        assert_eq!(
            format!("{deltas:?}"),
            format!(
                "{:?}",
                [
                    counts(1),
                    ModelDelta::Content(ContentDelta::Text("Hello".into())),
                    ModelDelta::Content(ContentDelta::Text("!".into())),
                    counts(15),
                ]
            )
        );

        let (response, assistant) = outcome.unwrap();
        assert_eq!(
            format!("{:?}", response.output),
            format!("{:?}", [ModelOutputItem::Text("Hello!".into())])
        );
        assert!(
            matches!(response.finish_reason, ModelFinishReason::Completed(ref raw) if raw == "end_turn")
        );
        assert_eq!(response.usage.input_tokens, Some(25));
        assert_eq!(response.usage.output_tokens, Some(15));
        assert_eq!(response.usage.cache_read_tokens, None);
        assert_eq!(response.usage.reasoning_tokens, None);
        assert_eq!(response.response_id.as_deref(), Some("msg_1"));
        assert_eq!(response.model.as_deref(), Some("claude-test"));
        assert_eq!(
            serde_json::to_value(assistant.unwrap()).unwrap(),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "Hello!" }] })
        );
    }

    #[test]
    fn thinking_and_a_tool_call_assemble_for_the_agent_and_for_history() {
        let (deltas, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_2", "model": "claude-test", "usage": {"input_tokens": 5, "cache_creation_input_tokens": 20, "cache_read_input_tokens": 100, "output_tokens": 1}}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Add them."}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}}"#,
            r#"{"type": "content_block_stop", "index": 0}"#,
            r#"{"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "calc_add", "input": {}}}"#,
            r#"{"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"a\": "}}"#,
            r#"{"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "1}"}}"#,
            r#"{"type": "content_block_stop", "index": 1}"#,
            r#"{"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30, "output_tokens_details": {"thinking_tokens": 12}}}"#,
            r#"{"type": "message_stop"}"#,
        ]);
        let content: Vec<&ModelDelta> = deltas
            .iter()
            .filter(|delta| matches!(delta, ModelDelta::Content(_)))
            .collect();
        assert_eq!(
            format!("{content:?}"),
            format!(
                "{:?}",
                [
                    ContentDelta::Reasoning("Add them.".into()),
                    ContentDelta::ToolCall(ToolCallDelta {
                        index: 1,
                        id: Some("toolu_1".into()),
                        name: Some("calc_add".into()),
                        arguments: String::new(),
                    }),
                    ContentDelta::ToolCall(ToolCallDelta {
                        index: 1,
                        id: None,
                        name: None,
                        arguments: "{\"a\": ".into(),
                    }),
                    ContentDelta::ToolCall(ToolCallDelta {
                        index: 1,
                        id: None,
                        name: None,
                        arguments: "1}".into(),
                    }),
                ]
                .map(ModelDelta::Content)
            )
        );

        let (response, assistant) = outcome.unwrap();
        // The last usage delta has the response's counts.
        assert_eq!(
            format!("{:?}", deltas.last().unwrap()),
            format!("{:?}", ModelDelta::Usage(response.usage))
        );
        assert_eq!(
            format!("{:?}", response.output),
            format!(
                "{:?}",
                [
                    ModelOutputItem::Reasoning(Reasoning {
                        text: "Add them.".into(),
                        redacted: false,
                    }),
                    ModelOutputItem::ToolCall(ToolCall {
                        id: "toolu_1".into(),
                        name: "calc_add".into(),
                        arguments: "{\"a\": 1}".into(),
                    }),
                ]
            )
        );
        assert!(matches!(
            response.finish_reason,
            ModelFinishReason::ToolRequested(_)
        ));
        assert_eq!(response.usage.input_tokens, Some(125));
        assert_eq!(response.usage.cache_read_tokens, Some(100));
        assert_eq!(response.usage.cache_write_tokens, Some(20));
        assert_eq!(response.usage.output_tokens, Some(30));
        assert_eq!(response.usage.reasoning_tokens, Some(12));
        assert_eq!(
            serde_json::to_value(assistant.unwrap()).unwrap(),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "Add them.", "signature": "sig-1" },
                { "type": "tool_use", "id": "toolu_1", "name": "calc_add", "input": { "a": 1 } },
            ] })
        );
        let opaque = response.opaque_content.unwrap();
        assert_eq!(opaque.api, "anthropic-messages");
        assert_eq!(
            opaque.content,
            r#"[{"type":"thinking","thinking":"Add them.","signature":"sig-1"},{"type":"tool_use","id":"toolu_1","name":"calc_add","input":{"a":1}}]"#
        );
    }

    #[test]
    fn every_block_goes_back_as_received() {
        let (deltas, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_5", "model": "claude-test"}}"#,
            // An empty text block, kept although the output has no item for it.
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}"#,
            r#"{"type": "content_block_stop", "index": 0}"#,
            // A server tool's use and result, which the output has no items for.
            r#"{"type": "content_block_start", "index": 1, "content_block": {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}}"#,
            r#"{"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"query\": \"rust\"}"}}"#,
            r#"{"type": "content_block_stop", "index": 1}"#,
            r#"{"type": "content_block_start", "index": 2, "content_block": {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [{"type": "web_search_result", "url": "https://example.com", "title": "Ex"}]}}"#,
            r#"{"type": "content_block_stop", "index": 2}"#,
            // Text with a citation.
            r#"{"type": "content_block_start", "index": 3, "content_block": {"type": "text", "text": ""}}"#,
            r#"{"type": "content_block_delta", "index": 3, "delta": {"type": "citations_delta", "citation": {"type": "web_search_result_location", "url": "https://example.com", "cited_text": "Fast."}}}"#,
            r#"{"type": "content_block_delta", "index": 3, "delta": {"type": "text_delta", "text": "Rust is fast."}}"#,
            r#"{"type": "content_block_stop", "index": 3}"#,
            // A tool's input, with its keys in the model's order.
            r#"{"type": "content_block_start", "index": 4, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "calc_add", "input": {}}}"#,
            r#"{"type": "content_block_delta", "index": 4, "delta": {"type": "input_json_delta", "partial_json": "{\"b\": 2, \"a\": 1}"}}"#,
            r#"{"type": "content_block_stop", "index": 4}"#,
            r#"{"type": "message_delta", "delta": {"stop_reason": "tool_use"}}"#,
            r#"{"type": "message_stop"}"#,
        ]);
        let (response, assistant) = outcome.unwrap();
        let blocks = r#"[{"type":"text","text":""},{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{"query":"rust"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","url":"https://example.com","title":"Ex"}]},{"type":"text","text":"Rust is fast.","citations":[{"type":"web_search_result_location","url":"https://example.com","cited_text":"Fast."}]},{"type":"tool_use","id":"toolu_2","name":"calc_add","input":{"b":2,"a":1}}]"#;
        assert_eq!(response.opaque_content.unwrap().content, blocks);
        assert_eq!(
            serde_json::to_string(&assistant.unwrap()).unwrap(),
            format!(r#"{{"role":"assistant","content":{blocks}}}"#)
        );
        // The agent gets the text and the tool call; the server tool's
        // input isn't one of its tool calls.
        assert_eq!(
            format!("{:?}", response.output),
            format!(
                "{:?}",
                [
                    ModelOutputItem::Text("Rust is fast.".into()),
                    ModelOutputItem::ToolCall(ToolCall {
                        id: "toolu_2".into(),
                        name: "calc_add".into(),
                        arguments: r#"{"b": 2, "a": 1}"#.into(),
                    }),
                ]
            )
        );
        assert_eq!(
            deltas
                .iter()
                .filter(|delta| matches!(delta, ModelDelta::Content(ContentDelta::ToolCall(_))))
                .count(),
            2
        );
    }

    #[test]
    fn tool_input_that_is_not_valid_json_fails_the_call() {
        let (_, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_6", "model": "claude-test"}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_3", "name": "calc_add", "input": {}}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"a\": "}}"#,
            r#"{"type": "content_block_stop", "index": 0}"#,
        ]);
        assert!(
            outcome
                .unwrap_err()
                .message
                .starts_with("the provider sent tool input for block 0 that is not valid JSON")
        );
    }

    #[test]
    fn an_error_event_or_a_stream_cut_short_fails_the_call() {
        // The call fails at the error event, so what follows it isn't applied.
        let (deltas, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_3", "model": "claude-test"}}"#,
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "late"}}"#,
        ]);
        assert_eq!(outcome.unwrap_err().message, "overloaded_error: Overloaded");
        assert!(deltas.is_empty(), "{deltas:?}");

        let (_, outcome) = fold(&[
            r#"{"type": "message_start", "message": {"id": "msg_4", "model": "claude-test"}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "Hel"}}"#,
        ]);
        assert!(
            outcome
                .unwrap_err()
                .message
                .contains("ended before the message was complete")
        );
    }

    #[test]
    fn earlier_exchanges_from_another_client_become_turns_without_their_reasoning() {
        let history = vec![
            ModelExchange {
                request: ModelRequest {
                    input: vec![ModelInputItem::Text("Add 1 and 2.".to_string())],
                    max_tokens: None,
                },
                output: Some(vec![
                    ModelOutputItem::Reasoning(Reasoning {
                        text: "Add them.".to_string(),
                        redacted: false,
                    }),
                    ModelOutputItem::ToolCall(ToolCall {
                        id: "toolu_1".to_string(),
                        name: "calc_add".to_string(),
                        arguments: r#"{"a":1,"b":2}"#.to_string(),
                    }),
                ]),
                opaque_content: None,
                provider: None,
                request_model: None,
                response_model: None,
                usage: TokenUsage::default(),
            },
            ModelExchange {
                request: ModelRequest {
                    input: vec![ModelInputItem::ToolResult(ToolResult {
                        tool_call_id: "toolu_1".to_string(),
                        name: "calc_add".to_string(),
                        content: vec![ResultContent::Text("3".to_string())],
                        is_error: false,
                    })],
                    max_tokens: None,
                },
                output: Some(vec![
                    ModelOutputItem::Reasoning(Reasoning {
                        text: "Done.".to_string(),
                        redacted: false,
                    }),
                    ModelOutputItem::Text("3".to_string()),
                ]),
                // Another API's form, which this client can't use.
                opaque_content: Some(OpaqueContent {
                    api: "open-responses".to_string(),
                    content: "[]".to_string(),
                }),
                provider: None,
                request_model: None,
                response_model: None,
                usage: TokenUsage::default(),
            },
        ];

        assert_eq!(
            serde_json::to_value(history_to_messages(&history).unwrap()).unwrap(),
            json!([
                { "role": "user", "content": [{ "type": "text", "text": "Add 1 and 2." }] },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_1", "name": "calc_add", "input": { "a": 1, "b": 2 } },
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": [{ "type": "text", "text": "3" }], "is_error": false },
                ] },
                { "role": "assistant", "content": [{ "type": "text", "text": "3" }] },
            ])
        );
    }

    #[test]
    fn an_earlier_turn_in_this_clients_form_goes_back_exactly() {
        let blocks = r#"[{"type":"thinking","thinking":"","signature":"sig-1"},{"type":"text","text":""},{"type":"tool_use","id":"toolu_1","name":"calc_add","input":{"b":2,"a":1}}]"#;
        let exchange = |content: &str| ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::Text("Add 1 and 2.".to_string())],
                max_tokens: None,
            },
            output: Some(vec![ModelOutputItem::ToolCall(ToolCall {
                id: "toolu_1".to_string(),
                name: "calc.add".to_string(),
                arguments: r#"{"b":2,"a":1}"#.to_string(),
            })]),
            opaque_content: Some(OpaqueContent {
                api: API.to_string(),
                content: content.to_string(),
            }),
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage::default(),
        };

        let messages = history_to_messages(&[exchange(blocks)]).unwrap();
        assert_eq!(
            serde_json::to_string(&messages[1]).unwrap(),
            format!(r#"{{"role":"assistant","content":{blocks}}}"#)
        );

        let error = history_to_messages(&[exchange("{not a list")])
            .err()
            .unwrap();
        assert!(
            error.message.starts_with(
                "history holds anthropic-messages content that is not a list of blocks"
            ),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_failed_call_leaves_only_its_input() {
        let history = vec![ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::Text("Hello".to_string())],
                max_tokens: None,
            },
            output: None,
            opaque_content: None,
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage::default(),
        }];

        assert_eq!(
            serde_json::to_value(history_to_messages(&history).unwrap()).unwrap(),
            json!([{ "role": "user", "content": [{ "type": "text", "text": "Hello" }] }])
        );
    }

    #[test]
    fn media_output_in_history_becomes_a_note_in_its_place() {
        let history = vec![ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::Text("Draw a cat.".to_string())],
                max_tokens: None,
            },
            output: Some(vec![
                ModelOutputItem::Text("Here it is:".to_string()),
                ModelOutputItem::Image(ContentSource::Data(MediaData {
                    mime_type: "image/png".to_string(),
                    data: "iVBORw0KGgo=".to_string(),
                })),
            ]),
            opaque_content: None,
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage::default(),
        }];

        assert_eq!(
            serde_json::to_value(&history_to_messages(&history).unwrap()[1]).unwrap(),
            json!({ "role": "assistant", "content": [
                { "type": "text", "text": "Here it is:" },
                { "type": "text", "text": "[This turn produced image output (image/png), which is left out of the history sent to this model.]" },
            ] })
        );
    }

    #[test]
    fn every_stop_reason_maps_to_a_finish_reason() {
        let cases: [StopReasonCase; 8] = [
            ("end_turn", ModelFinishReason::Completed),
            ("tool_use", ModelFinishReason::ToolRequested),
            ("max_tokens", ModelFinishReason::Truncated),
            (
                "model_context_window_exceeded",
                ModelFinishReason::Truncated,
            ),
            ("stop_sequence", ModelFinishReason::Stopped),
            ("refusal", ModelFinishReason::Blocked),
            ("pause_turn", ModelFinishReason::Paused),
            ("something_new", ModelFinishReason::Other),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                format!("{:?}", finish_reason(raw)),
                format!("{:?}", expected(raw.to_string()))
            );
        }
    }
}
