//! An implementation of the `composable:models/client` interface for
//! OpenAI-compatible Chat Completions providers, using SSE streaming.

wit_bindgen::generate!({
    path: "wit",
    world: "chat-completions-client",
    with: {
        "wasi:config/store@0.2.0-rc.1": model_client_support::config_store,
        "componentized:http/client@0.1.0": model_client_support::http_client,
        "composable:models/types@0.1.0": model_client_support::types,
    },
    generate_all,
});

mod mapping;

use exports::composable::models::client::GuestSession;
use model_client_support::types::{
    ModelDelta, ModelError, ModelExchange, ModelInputItem, ModelRequest, ModelResponse,
    ModelResponseStream, ModelSettings,
};
use model_client_support::{
    Client, Deltas, Outcome, Session, parsed_config, require_answered_tool_calls,
};
use wit_bindgen::rt::async_support::{FutureReader, FutureWriter, StreamReader, StreamWriter};

const MAX_TOKENS_FIELD_CONFIG: &str = "max-tokens-field";

/// A client of the Chat Completions API. It keeps the conversation history in
/// Chat Completions form and sends it with each call.
struct ChatCompletionsClient;

/// The client's own config.
struct ChatCompletionsConfig {
    max_tokens_field: mapping::MaxTokensField,
}

/// The conversation and the session's settings, in Chat Completions form.
struct ChatState {
    messages: Vec<mapping::Message>,
    api_settings: mapping::ApiSettings,
}

impl Client for ChatCompletionsClient {
    type Config = ChatCompletionsConfig;
    type State = ChatState;
    // The assistant message.
    type Output = mapping::Message;

    const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    const PATH: &'static str = "/chat/completions";

    fn config() -> Result<ChatCompletionsConfig, ModelError> {
        Ok(ChatCompletionsConfig {
            max_tokens_field: parsed_config(
                MAX_TOKENS_FIELD_CONFIG,
                mapping::MAX_TOKENS_FIELD_VALUES,
            )?
            .unwrap_or_default(),
        })
    }

    fn headers(api_key: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if let Some(key) = api_key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }
        headers
    }

    fn start(settings: &ModelSettings, history: &[ModelExchange]) -> Result<ChatState, ModelError> {
        require_answered_tool_calls(history)?;
        Ok(ChatState {
            messages: mapping::history_to_messages(settings.instructions.as_deref(), history),
            api_settings: mapping::ApiSettings::new(settings)?,
        })
    }

    fn add_input(state: &mut ChatState, input: &[ModelInputItem]) -> Result<(), ModelError> {
        state
            .messages
            .extend(mapping::input_items_to_messages(input)?);
        Ok(())
    }

    fn request(
        model: &str,
        state: &ChatState,
        config: &ChatCompletionsConfig,
        max_tokens: Option<u32>,
    ) -> Result<Vec<u8>, ModelError> {
        mapping::build_request(
            model,
            &state.messages,
            &state.api_settings,
            config.max_tokens_field,
            max_tokens,
        )
    }

    async fn read_response(
        body: StreamReader<u8>,
        deltas: &mut Deltas,
    ) -> Result<(ModelResponse, mapping::Message), ModelError> {
        mapping::stream_response(body, deltas).await
    }

    fn add_output(state: &mut ChatState, output: mapping::Message) {
        state.messages.push(output);
    }

    fn delta_stream() -> (StreamWriter<ModelDelta>, StreamReader<ModelDelta>) {
        wit_stream::new()
    }

    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>) {
        wit_future::new(default)
    }
}

struct SessionState(Session<ChatCompletionsClient>);

impl GuestSession for SessionState {
    fn new(settings: ModelSettings, history: Vec<ModelExchange>) -> Result<Self, ModelError>
    where
        Self: Sized,
    {
        Session::new(settings, history).map(SessionState)
    }

    async fn call(&self, request: ModelRequest) -> Result<ModelResponseStream, ModelError> {
        self.0.call(request).await
    }

    fn exchanges(&self) -> Vec<ModelExchange> {
        self.0.exchanges()
    }
}

struct Component;

impl exports::composable::models::client::Guest for Component {
    type Session = SessionState;
}

export!(Component);
