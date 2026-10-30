//! Mapping between the `composable:agents/types` WIT and the
//! OpenAI Chat Completions JSON, using SSE streaming.

use std::convert::Infallible;

use serde::{Deserialize, Serialize};

use crate::composable::agents::types::{
    ContentSource, ModelDelta, ModelError, ModelExchange, ModelFinishReason, ModelInputItem,
    ModelOutputItem, ModelResponse, ModelSettings, ResultContent, TokenUsage, ToolCall,
    ToolCallDelta, ToolChoice, ToolDefinition,
};
use wit_bindgen::rt::async_support::{StreamReader, StreamWriter};

// === Outgoing request =======================================================

#[derive(Serialize, Clone)]
pub(crate) struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "tool_call_id")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<AssistantToolCall>>,
}

// A message's content: a string or a list of parts.
#[derive(Serialize, Clone)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

// An image as a web URL or a base64 data URI.
#[derive(Serialize, Clone)]
struct ImageUrl {
    url: String,
}

// A tool call carried on an assistant message (OpenAI wire form), so that a
// subsequent `tool` result message is accepted.
#[derive(Serialize, Clone)]
struct AssistantToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: AssistantToolCallFunction,
}

#[derive(Serialize, Clone)]
struct AssistantToolCallFunction {
    name: String,
    arguments: String,
}

impl Message {
    fn system(text: String) -> Self {
        Message {
            role: "system".into(),
            content: Some(Content::Text(text)),
            tool_call_id: None,
            tool_calls: None,
        }
    }
    // A user turn: a string when a single text part, otherwise its parts.
    fn user(parts: Vec<ContentPart>) -> Self {
        let content = match <[ContentPart; 1]>::try_from(parts) {
            Ok([ContentPart::Text { text }]) => Content::Text(text),
            Ok([part]) => Content::Parts(vec![part]),
            Err(parts) => Content::Parts(parts),
        };
        Message {
            role: "user".into(),
            content: Some(content),
            tool_call_id: None,
            tool_calls: None,
        }
    }
    fn tool(tool_call_id: String, content: String) -> Self {
        Message {
            role: "tool".into(),
            content: Some(Content::Text(content)),
            tool_call_id: Some(tool_call_id),
            tool_calls: None,
        }
    }
    // An assistant turn. `content` is None when the turn is only tool calls
    // (OpenAI expects no empty-string content alongside tool_calls).
    fn assistant(content: Option<String>, tool_calls: Option<Vec<AssistantToolCall>>) -> Self {
        Message {
            role: "assistant".into(),
            content: content.map(Content::Text),
            tool_call_id: None,
            tool_calls,
        }
    }
}

/// A new call's input as messages. The user's content as one user turn, and
/// each tool result as a tool message. It fails on input this client cannot
/// send, before the call is recorded.
pub(crate) fn input_items_to_messages(
    input: &[ModelInputItem],
) -> Result<Vec<Message>, ModelError> {
    input_messages(input, |kind, source| {
        Err(ModelError {
            message: format!(
                "this client cannot send {kind} input ({})",
                media_detail(source)
            ),
            status: None,
        })
    })
}

// A call's input as messages. `unsendable` decides how to handle any item this
// client cannot send: an error, or a text replacement.
fn input_messages<E>(
    input: &[ModelInputItem],
    unsendable: impl Fn(&str, &ContentSource) -> Result<String, E>,
) -> Result<Vec<Message>, E> {
    let mut messages = Vec::new();
    let mut parts = Vec::new();
    for item in input {
        let part = match item {
            ModelInputItem::Text(text) => ContentPart::Text { text: text.clone() },
            ModelInputItem::Image(source) => ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: image_url(source),
                },
            },
            ModelInputItem::Audio(source) => ContentPart::Text {
                text: unsendable("audio", source)?,
            },
            ModelInputItem::Video(source) => ContentPart::Text {
                text: unsendable("video", source)?,
            },
            ModelInputItem::Document(source) => ContentPart::Text {
                text: unsendable("document", source)?,
            },
            ModelInputItem::ToolResult(result) => {
                if !parts.is_empty() {
                    messages.push(Message::user(std::mem::take(&mut parts)));
                }
                messages.push(Message::tool(
                    result.tool_call_id.clone(),
                    tool_result_text(&result.content),
                ));
                continue;
            }
        };
        parts.push(part);
    }
    if !parts.is_empty() {
        messages.push(Message::user(parts));
    }
    Ok(messages)
}

// A tool result's content. Tool messages take text, so media becomes a note.
fn tool_result_text(content: &[ResultContent]) -> String {
    content
        .iter()
        .map(|content| match content {
            ResultContent::Text(text) => text.clone(),
            ResultContent::Media(source) => format!(
                "[The tool returned media ({}), which is left out of the result sent to this model.]",
                media_detail(source)
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// An image as a web URL or its bytes as a data URI.
fn image_url(source: &ContentSource) -> String {
    match source {
        ContentSource::Data(media) => format!("data:{};base64,{}", media.mime_type, media.data),
        ContentSource::Url(url) => url.clone(),
    }
}

// Media detail for a note, either its media type or its URL.
fn media_detail(source: &ContentSource) -> &str {
    match source {
        ContentSource::Data(media) => &media.mime_type,
        ContentSource::Url(url) => url,
    }
}

/// The messages that start a session: instructions as a `system` message, then
/// each earlier exchange's input and the model's answer to it. Input this
/// client cannot send becomes a note, so a conversation from another model can
/// continue.
pub(crate) fn history_to_messages(
    instructions: Option<&str>,
    history: &[ModelExchange],
) -> Vec<Message> {
    let mut messages = Vec::new();
    if let Some(instructions) = instructions
        && !instructions.is_empty()
    {
        messages.push(Message::system(instructions.to_string()));
    }
    for exchange in history {
        let Ok(input) = input_messages(&exchange.request.input, |kind, source| {
            Ok::<_, Infallible>(format!(
                "[This turn included {kind} input ({}), which is left out of the history sent to this model.]",
                media_detail(source)
            ))
        });
        messages.extend(input);
        if let Some(output) = &exchange.output {
            messages.push(output_items_to_message(output));
        }
    }
    messages
}

/// Fails on a tool call in `history` that the next exchange's request doesn't
/// answer with a tool result, since the API rejects a conversation with one.
pub(crate) fn require_answered_tool_calls(history: &[ModelExchange]) -> Result<(), ModelError> {
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
                });
            }
        }
    }
    Ok(())
}

// A response's output as one assistant message, with tool names sanitized as
// they were sent. Reasoning has no Chat Completions form. Media, which another
// client's model may have produced, becomes a note in its place.
fn output_items_to_message(output: &[ModelOutputItem]) -> Message {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut in_text = false;
    for item in output {
        if let ModelOutputItem::Text(text) = item {
            match paragraphs.last_mut() {
                Some(last) if in_text => last.push_str(text),
                _ => paragraphs.push(text.clone()),
            }
            in_text = true;
        } else if let Some(note) = media_note(item) {
            paragraphs.push(note);
            in_text = false;
        }
    }
    let text = paragraphs.join("\n\n");
    let tool_calls: Vec<AssistantToolCall> = output
        .iter()
        .filter_map(|item| match item {
            ModelOutputItem::ToolCall(call) => Some(AssistantToolCall {
                id: call.id.clone(),
                kind: "function",
                function: AssistantToolCallFunction {
                    name: sanitize_tool_name(&call.name),
                    arguments: call.arguments.clone(),
                },
            }),
            _ => None,
        })
        .collect();
    Message::assistant(
        (!text.is_empty()).then_some(text),
        (!tool_calls.is_empty()).then_some(tool_calls),
    )
}

// A note in place of media the model produced, which this client doesn't send
// back in an assistant message.
fn media_note(item: &ModelOutputItem) -> Option<String> {
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

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

// Chat Completions has no `top_k`, so `model-settings.top-k` is not sent.
// The WIT allows for settings an API does not support.
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

// Structured output: `{"type":"json_schema","json_schema":{...}}`.
#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: JsonSchemaFormat,
}

#[derive(Serialize)]
struct JsonSchemaFormat {
    name: &'static str,
    schema: serde_json::Value,
    strict: bool,
}

// The name Chat Completions requires for an output schema, a label only.
const OUTPUT_SCHEMA_NAME: &str = "output";

// `model-settings.output-schema` as a strict JSON Schema response format. Only
// strict mode guarantees the output matches the schema, and a schema outside
// strict mode's subset fails the call with the provider's error.
fn response_format(output_schema: &str) -> Result<ResponseFormat, ModelError> {
    let schema = serde_json::from_str(output_schema).map_err(|e| ModelError {
        message: format!("the output-schema is invalid JSON: {e}"),
        status: None,
    })?;
    Ok(ResponseFormat {
        kind: "json_schema",
        json_schema: JsonSchemaFormat {
            name: OUTPUT_SCHEMA_NAME,
            schema,
            strict: true,
        },
    })
}

// OpenAI tool definition: `{"type":"function","function":{...}}`.
#[derive(Serialize)]
struct Tool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ToolFunction,
}

#[derive(Serialize)]
struct ToolFunction {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The tool's JSON Schema.
    parameters: serde_json::Value,
}

// OpenAI requires tool function names to match `^[a-zA-Z0-9_-]+$`, so a tool
// name containing a `.` (common in MCP names) translates to use `_`. The
// reverse mapping is applied to tool-call names in the response. Other invalid
// characters are left as is, so they will be rejected with an explicit error.
fn sanitize_tool_name(name: &str) -> String {
    name.replace('.', "_")
}

// Map a WIT tool-definition to the OpenAI tool shape (with a sanitized name).
// The input-schema is a JSON Schema string embedded as a JSON object. A parse
// failure leads to an error for the request since the tool should not be used.
fn to_openai_tool(def: &ToolDefinition) -> Result<Tool, ModelError> {
    let parameters = serde_json::from_str(&def.input_schema).map_err(|e| ModelError {
        message: format!("tool '{}' has an invalid input-schema: {e}", def.name),
        status: None,
    })?;
    Ok(Tool {
        kind: "function",
        function: ToolFunction {
            name: sanitize_tool_name(&def.name),
            description: if def.description.is_empty() {
                None
            } else {
                Some(def.description.clone())
            },
            parameters,
        },
    })
}

// A sanitized-name => original-name map for reversing tool-call names in the
// response. Only names that were actually sanitized are stored.
type ToolNameMap = std::collections::HashMap<String, String>;

/// Fails when two tools would reach the API having the same name. Sanitizing
/// makes `a.b` collide with any explicit `a_b`.
pub(crate) fn require_distinct_tool_names(settings: &ModelSettings) -> Result<(), ModelError> {
    let mut sent: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for def in settings.tools.iter().flatten() {
        let name = sanitize_tool_name(&def.name);
        if let Some(other) = sent.insert(name.clone(), &def.name) {
            return Err(ModelError {
                message: format!(
                    "tools '{other}' and '{}' would both be sent as '{name}'",
                    def.name
                ),
                status: None,
            });
        }
    }
    Ok(())
}

// Build the sanitized => original tool-name map from the configured tools.
fn tool_name_map(settings: &ModelSettings) -> ToolNameMap {
    let mut map = ToolNameMap::new();
    if let Some(tools) = &settings.tools {
        for def in tools {
            let sanitized = sanitize_tool_name(&def.name);
            if sanitized != def.name {
                map.insert(sanitized, def.name.clone());
            }
        }
    }
    map
}

// Map the WIT tool-choice to the OpenAI `tool_choice` value.
fn to_openai_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Specific(name) => {
            serde_json::json!({ "type": "function", "function": { "name": sanitize_tool_name(name) } })
        }
    }
}

pub(crate) fn build_request(
    settings: &ModelSettings,
    model: &str,
    max_tokens: Option<u32>,
    history: &[Message],
) -> Result<Vec<u8>, ModelError> {
    let tools = settings
        .tools
        .as_ref()
        .map(|defs| {
            defs.iter()
                .map(to_openai_tool)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let tool_choice = settings.tool_choice.as_ref().map(to_openai_tool_choice);
    let response_format = settings
        .output_schema
        .as_deref()
        .map(response_format)
        .transpose()?;

    let req = ChatRequest {
        model,
        messages: history,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        temperature: settings.temperature,
        top_p: settings.top_p,
        max_tokens,
        seed: settings.seed,
        stop: settings.stop_sequences.clone(),
        tools,
        tool_choice,
        response_format,
    };
    serde_json::to_vec(&req).map_err(|e| ModelError {
        message: format!("failed to serialize request: {e}"),
        status: None,
    })
}

// === Incoming SSE stream ====================================================

#[derive(Deserialize)]
struct Chunk {
    // Every chunk of a response carries the response's id and the model that
    // produced it.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    // An error the provider sends in place of the rest of the response.
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallChunk>,
}

#[derive(Deserialize)]
struct ToolCallChunk {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ToolCallFunctionChunk>,
}

#[derive(Deserialize)]
struct ToolCallFunctionChunk {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// A count is `None` when the provider leaves it out. 0 means a reported value.
#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    // Prompt tokens served from the provider's cache.
    #[serde(default)]
    cached_tokens: Option<u64>,
    // Prompt tokens written to the provider's cache.
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct CompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

/// Accumulator for the final assembled response.
#[derive(Default)]
struct Accumulator {
    response_id: Option<String>,
    model: Option<String>,
    text: String,
    /// Tool calls accumulated by stream index.
    tool_calls: Vec<AccTool>,
    finish_reason: Option<String>,
    // The usage the provider reported, if any. Only a final chunk carries it,
    // when the request asks for it.
    usage: Option<Usage>,
}

#[derive(Default, Clone)]
struct AccTool {
    id: String,
    name: String,
    arguments: String,
}

/// Consume the SSE body stream. Parse chunks, write `ModelDelta`s to `deltas`,
/// and return the assembled response plus the assistant message to record in
/// history. When the caller drops `deltas`, it stops reading and drops the
/// body, which closes the connection so the provider stops generating.
pub(crate) async fn stream_response(
    mut body: StreamReader<u8>,
    deltas: &mut StreamWriter<ModelDelta>,
    settings: &ModelSettings,
) -> Result<(ModelResponse, Message), ModelError> {
    let mut acc = Accumulator::default();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let (result, chunk) = body.read(Vec::with_capacity(8192)).await;
        if !chunk.is_empty() {
            buf.extend_from_slice(&chunk);
        }
        // Process complete newline-delimited SSE lines from the buffer.
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<u8>>();
            process_line(&line, &mut acc, deltas).await?;
        }
        if matches!(
            result,
            wit_bindgen::rt::async_support::StreamResult::Dropped
        ) {
            break;
        }
    }

    complete(acc, &tool_name_map(settings))
}

// The response the stream assembled. A response ends with a chunk that
// includes its finish reason. A body that ended before one arrived, as when
// the connection drops, is no response.
fn complete(
    acc: Accumulator,
    tool_names: &ToolNameMap,
) -> Result<(ModelResponse, Message), ModelError> {
    if acc.finish_reason.is_none() {
        return Err(ModelError {
            message: "the response ended before the provider finished it".to_string(),
            status: None,
        });
    }
    Ok(assemble(acc, tool_names))
}

// Apply one SSE line. It fails when the provider sends an error or data it
// cannot read, or when the caller stopped reading the deltas, which stops the
// response.
async fn process_line(
    line: &[u8],
    acc: &mut Accumulator,
    deltas: &mut StreamWriter<ModelDelta>,
) -> Result<(), ModelError> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    if !apply_chunk(chunk_of(data)?, acc, deltas).await {
        return Err(ModelError {
            message: "the caller stopped reading the response".to_string(),
            status: None,
        });
    }
    Ok(())
}

// The chunk a data line carries. It fails on data that is not a chunk or on an
// error the provider sent.
fn chunk_of(data: &str) -> Result<Chunk, ModelError> {
    let chunk: Chunk = serde_json::from_str(data).map_err(|e| ModelError {
        message: format!("the provider sent a chunk that is not valid JSON ({e}): {data}"),
        status: None,
    })?;
    match &chunk.error {
        Some(error) => Err(ModelError {
            message: format!("the provider failed the response: {}", error_message(error)),
            status: None,
        }),
        None => Ok(chunk),
    }
}

// The message of an error the provider sent or the error itself if no message.
fn error_message(error: &serde_json::Value) -> String {
    match error.get("message").and_then(serde_json::Value::as_str) {
        Some(message) => message.to_string(),
        None => error.to_string(),
    }
}

// Apply one chunk, returning whether the caller still reads the deltas.
async fn apply_chunk(
    chunk: Chunk,
    acc: &mut Accumulator,
    deltas: &mut StreamWriter<ModelDelta>,
) -> bool {
    if chunk.id.is_some() {
        acc.response_id = chunk.id;
    }
    if chunk.model.is_some() {
        acc.model = chunk.model;
    }
    if chunk.usage.is_some() {
        acc.usage = chunk.usage;
    }
    for choice in chunk.choices {
        if let Some(fr) = choice.finish_reason {
            acc.finish_reason = Some(fr);
        }
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            acc.text.push_str(&content);
            if deltas.write_one(ModelDelta::Text(content)).await.is_some() {
                return false;
            }
        }
        for tc in choice.delta.tool_calls {
            let idx = tc.index as usize;
            if acc.tool_calls.len() <= idx {
                acc.tool_calls.resize(idx + 1, AccTool::default());
            }
            let entry = &mut acc.tool_calls[idx];
            if let Some(id) = &tc.id {
                entry.id = id.clone();
            }
            let (name, args) = match &tc.function {
                Some(f) => (f.name.clone(), f.arguments.clone()),
                None => (None, None),
            };
            if let Some(n) = &name {
                entry.name = n.clone();
            }
            if let Some(a) = &args {
                entry.arguments.push_str(a);
            }
            let delta = ModelDelta::ToolCall(ToolCallDelta {
                index: tc.index,
                id: tc.id,
                name,
                arguments: args.unwrap_or_default(),
            });
            if deltas.write_one(delta).await.is_some() {
                return false;
            }
        }
    }
    true
}

fn assemble(acc: Accumulator, tool_names: &ToolNameMap) -> (ModelResponse, Message) {
    let mut output = Vec::new();
    if !acc.text.is_empty() {
        output.push(ModelOutputItem::Text(acc.text));
    }
    for tc in &acc.tool_calls {
        // The model echoes back the sanitized name, so restore the original if
        // necessary.
        let name = tool_names
            .get(&tc.name)
            .cloned()
            .unwrap_or_else(|| tc.name.clone());
        output.push(ModelOutputItem::ToolCall(ToolCall {
            id: tc.id.clone(),
            name,
            arguments: tc.arguments.clone(),
        }));
    }

    let raw = acc.finish_reason.unwrap_or_default();
    let response = ModelResponse {
        output,
        finish_reason: map_finish_reason(&raw),
        usage: token_usage(acc.usage.as_ref()),
        response_id: acc.response_id,
        model: acc.model,
    };

    // History is re-sent to the provider, so its assistant message carries the
    // tool names as they were sent. Sanitizing an original name gives back the
    // name the model called.
    let assistant = output_items_to_message(&response.output);
    (response, assistant)
}

// The usage the provider reported, count by count. A provider that reported
// none leaves every count `None` and the call unreported.
fn token_usage(usage: Option<&Usage>) -> TokenUsage {
    let Some(usage) = usage else {
        return TokenUsage {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            unreported_calls: 1,
        };
    };
    TokenUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cache_read_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens),
        cache_write_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cache_write_tokens),
        reasoning_tokens: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
        unreported_calls: 0,
    }
}

fn map_finish_reason(raw: &str) -> ModelFinishReason {
    match raw {
        "stop" => ModelFinishReason::Completed(raw.to_string()),
        "tool_calls" => ModelFinishReason::ToolRequested(raw.to_string()),
        "length" => ModelFinishReason::Truncated(raw.to_string()),
        "content_filter" => ModelFinishReason::Blocked(raw.to_string()),
        _ => ModelFinishReason::Other(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composable::agents::types::{ModelRequest, ToolResult};
    use serde_json::json;

    fn exchange(input: Vec<ModelInputItem>, output: Option<Vec<ModelOutputItem>>) -> ModelExchange {
        ModelExchange {
            request: ModelRequest {
                input,
                max_tokens: None,
            },
            output,
        }
    }

    #[test]
    fn earlier_exchanges_follow_the_instructions_as_messages() {
        let history = vec![
            exchange(
                vec![ModelInputItem::Text("What's 6 times 7?".into())],
                Some(vec![ModelOutputItem::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "calc.mul".into(),
                    arguments: r#"{"a":6,"b":7}"#.into(),
                })]),
            ),
            exchange(
                vec![ModelInputItem::ToolResult(ToolResult {
                    tool_call_id: "call-1".into(),
                    name: "calc.mul".into(),
                    content: vec![ResultContent::Text("42".into())],
                    is_error: false,
                })],
                Some(vec![ModelOutputItem::Text("42".into())]),
            ),
        ];

        let messages = history_to_messages(Some("Use tools."), &history);

        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            json!([
                { "role": "system", "content": "Use tools." },
                { "role": "user", "content": "What's 6 times 7?" },
                { "role": "assistant", "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "calc_mul", "arguments": r#"{"a":6,"b":7}"# },
                }] },
                { "role": "tool", "content": "42", "tool_call_id": "call-1" },
                { "role": "assistant", "content": "42" },
            ])
        );
    }

    #[test]
    fn a_failed_call_leaves_only_its_input() {
        let history = vec![exchange(vec![ModelInputItem::Text("Hello".into())], None)];

        let messages = history_to_messages(None, &history);

        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            json!([{ "role": "user", "content": "Hello" }])
        );
    }

    fn png() -> ContentSource {
        ContentSource::Data(crate::composable::agents::types::MediaData {
            mime_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        })
    }

    fn settings_with_tools(names: &[&str]) -> ModelSettings {
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

    #[test]
    fn an_output_schema_maps_to_a_strict_json_schema_response_format() {
        let settings = ModelSettings {
            output_schema: Some(r#"{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"],"additionalProperties":false}"#.into()),
            ..settings_with_tools(&[])
        };
        let body: serde_json::Value =
            serde_json::from_slice(&build_request(&settings, "gpt-test", None, &[]).unwrap())
                .unwrap();
        assert_eq!(
            body["response_format"],
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "output",
                    "schema": {
                        "type": "object",
                        "properties": { "answer": { "type": "integer" } },
                        "required": ["answer"],
                        "additionalProperties": false,
                    },
                    "strict": true,
                },
            })
        );

        let without: serde_json::Value = serde_json::from_slice(
            &build_request(&settings_with_tools(&[]), "gpt-test", None, &[]).unwrap(),
        )
        .unwrap();
        assert!(without.get("response_format").is_none());

        let invalid = ModelSettings {
            output_schema: Some("{not json".into()),
            ..settings_with_tools(&[])
        };
        assert!(build_request(&invalid, "gpt-test", None, &[]).is_err());
    }

    #[test]
    fn tools_with_colliding_names_fail() {
        assert!(
            require_distinct_tool_names(&settings_with_tools(&["calc.add", "calc_mul"])).is_ok()
        );
        assert_eq!(
            require_distinct_tool_names(&settings_with_tools(&["calc.add", "calc_add"]))
                .unwrap_err()
                .message,
            "tools 'calc.add' and 'calc_add' would both be sent as 'calc_add'"
        );
    }

    #[test]
    fn a_forced_tool_is_named_as_it_was_sent() {
        assert_eq!(
            to_openai_tool_choice(&ToolChoice::Specific("calc.add".into())),
            json!({ "type": "function", "function": { "name": "calc_add" } })
        );
    }

    #[test]
    fn an_error_or_unreadable_data_fails_the_response() {
        let error =
            chunk_of(r#"{"error": {"message": "model overloaded", "type": "server_error"}}"#)
                .err()
                .unwrap();
        assert_eq!(
            error.message,
            "the provider failed the response: model overloaded"
        );
        assert!(chunk_of("{not json").is_err());
        assert!(chunk_of(r#"{"choices": [{"delta": {"content": "Hi"}}]}"#).is_ok());
    }

    #[test]
    fn a_stream_that_ends_without_a_finish_reason_is_no_response() {
        let unfinished = Accumulator {
            text: "Half an ans".into(),
            ..Accumulator::default()
        };
        assert_eq!(
            complete(unfinished, &ToolNameMap::new())
                .err()
                .unwrap()
                .message,
            "the response ended before the provider finished it"
        );

        let finished = Accumulator {
            text: "An answer.".into(),
            finish_reason: Some("stop".into()),
            ..Accumulator::default()
        };
        assert!(complete(finished, &ToolNameMap::new()).is_ok());
    }

    #[test]
    fn usage_includes_what_the_provider_reported_and_nothing_else() {
        let reported: Usage = serde_json::from_value(json!({
            "prompt_tokens": 12,
            "completion_tokens": 3,
            "total_tokens": 15,
            "prompt_tokens_details": { "cached_tokens": 0, "cache_write_tokens": 7 },
        }))
        .unwrap();
        let usage = token_usage(Some(&reported));
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(15));
        assert_eq!(usage.cache_write_tokens, Some(7));
        // A reported 0 stays 0; a count left out stays unknown.
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.reasoning_tokens, None);
        assert_eq!(usage.unreported_calls, 0);

        let partial: Usage = serde_json::from_value(json!({ "completion_tokens": 3 })).unwrap();
        let usage = token_usage(Some(&partial));
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.unreported_calls, 0);
    }

    #[test]
    fn a_response_without_usage_reports_no_counts_and_one_unreported_call() {
        let usage = token_usage(None);
        assert_eq!(
            [usage.input_tokens, usage.output_tokens, usage.total_tokens],
            [None, None, None]
        );
        assert_eq!(usage.unreported_calls, 1);
    }

    #[test]
    fn a_user_turn_with_images_becomes_text_and_image_url_parts() {
        let messages = input_items_to_messages(&[
            ModelInputItem::Text("What is in these?".into()),
            ModelInputItem::Image(png()),
            ModelInputItem::Image(ContentSource::Url("https://example.com/b.jpg".into())),
        ])
        .unwrap();

        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            json!([{ "role": "user", "content": [
                { "type": "text", "text": "What is in these?" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } },
                { "type": "image_url", "image_url": { "url": "https://example.com/b.jpg" } },
            ] }])
        );
    }

    #[test]
    fn a_new_call_fails_on_input_the_client_cannot_send() {
        let error = input_items_to_messages(&[
            ModelInputItem::Text("Transcribe this.".into()),
            ModelInputItem::Audio(ContentSource::Url("https://example.com/a.mp3".into())),
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
        let history = vec![exchange(
            vec![
                ModelInputItem::Text("Transcribe this.".into()),
                ModelInputItem::Audio(ContentSource::Url("https://example.com/a.mp3".into())),
            ],
            Some(vec![ModelOutputItem::Text("It says hello.".into())]),
        )];

        assert_eq!(
            serde_json::to_value(&history_to_messages(None, &history)[0]).unwrap(),
            json!({ "role": "user", "content": [
                { "type": "text", "text": "Transcribe this." },
                { "type": "text", "text": "[This turn included audio input (https://example.com/a.mp3), which is left out of the history sent to this model.]" },
            ] })
        );
    }

    #[test]
    fn tool_result_media_becomes_a_note_in_the_tool_message() {
        let messages = input_items_to_messages(&[ModelInputItem::ToolResult(ToolResult {
            tool_call_id: "call-1".into(),
            name: "chart".into(),
            content: vec![
                ResultContent::Text("Here is the chart.".into()),
                ResultContent::Media(png()),
            ],
            is_error: false,
        })])
        .unwrap();

        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            json!([{
                "role": "tool",
                "tool_call_id": "call-1",
                "content": "Here is the chart.\n[The tool returned media (image/png), which is left out of the result sent to this model.]",
            }])
        );
    }

    #[test]
    fn media_output_in_history_becomes_a_note_between_the_text() {
        use crate::composable::agents::types::MediaData;

        let history = vec![exchange(
            vec![ModelInputItem::Text("Draw a cat, then a dog.".into())],
            Some(vec![
                ModelOutputItem::Text("Here is ".into()),
                ModelOutputItem::Text("the cat:".into()),
                ModelOutputItem::Image(ContentSource::Data(MediaData {
                    mime_type: "image/png".into(),
                    data: "iVBORw0KGgo=".into(),
                })),
                ModelOutputItem::Text("And the dog:".into()),
                ModelOutputItem::Image(ContentSource::Url("https://example.com/dog.png".into())),
            ]),
        )];

        let messages = history_to_messages(None, &history);

        assert_eq!(
            serde_json::to_value(&messages[1]).unwrap(),
            json!({
                "role": "assistant",
                "content": "Here is the cat:\n\n\
                    [This turn produced image output (image/png), which is left out of the history sent to this model.]\n\n\
                    And the dog:\n\n\
                    [This turn produced image output (https://example.com/dog.png), which is left out of the history sent to this model.]",
            })
        );
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
}
