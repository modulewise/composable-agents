//! Implements `composable:models/client` for Anthropic's Messages API,
//! streaming over SSE.

wit_bindgen::generate!({
    path: "wit",
    world: "anthropic-messages-client",
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
use std::time::Duration;

use wit_bindgen::rt::async_support::{FutureReader, FutureWriter, StreamReader, StreamWriter};

const EFFORT_CONFIG: &str = "effort";
const INCLUDE_REASONING_CONFIG: &str = "include-reasoning";
const MAX_RETRIES_CONFIG: &str = "max-retries";

// Retries when `max-retries` isn't configured.
const DEFAULT_MAX_RETRIES: u32 = 2;

// The Messages API version this client speaks.
const ANTHROPIC_VERSION: &str = "2026-10-21";

/// A client of Anthropic's Messages API. It keeps the conversation history in
/// Messages API form and sends it with each call.
struct MessagesClient;

/// The client's own config.
struct MessagesConfig {
    effort: Option<mapping::Effort>,
    include_reasoning: bool,
    max_retries: u32,
}

/// The conversation and the session's settings, in Messages API form.
struct MessagesState {
    messages: Vec<mapping::Message>,
    api_settings: mapping::ApiSettings,
}

impl Client for MessagesClient {
    type Config = MessagesConfig;
    type State = MessagesState;
    // The assistant turn, if the response has content.
    type Output = Option<mapping::Message>;

    const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com/v1";
    const PATH: &'static str = "/messages";
    // The Messages API requires `max_tokens`. This leaves room for thinking,
    // which counts toward it.
    const DEFAULT_MAX_TOKENS: Option<u32> = Some(64000);

    fn config() -> Result<MessagesConfig, ModelError> {
        Ok(MessagesConfig {
            effort: parsed_config(EFFORT_CONFIG, mapping::EFFORT_VALUES)?,
            include_reasoning: parsed_config(INCLUDE_REASONING_CONFIG, "true or false")?
                .unwrap_or(false),
            max_retries: parsed_config(MAX_RETRIES_CONFIG, "a number")?
                .unwrap_or(DEFAULT_MAX_RETRIES),
        })
    }

    fn max_retries(config: &MessagesConfig) -> u32 {
        config.max_retries
    }

    async fn sleep(duration: Duration) {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        wasi::clocks::monotonic_clock::wait_for(nanos).await;
    }

    fn headers(api_key: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "anthropic-version".to_string(),
                ANTHROPIC_VERSION.to_string(),
            ),
        ];
        if let Some(key) = api_key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }
        headers
    }

    fn start(
        settings: &ModelSettings,
        history: &[ModelExchange],
    ) -> Result<MessagesState, ModelError> {
        require_answered_tool_calls(history)?;
        Ok(MessagesState {
            messages: mapping::history_to_messages(history)?,
            api_settings: mapping::ApiSettings::new(settings)?,
        })
    }

    fn add_input(state: &mut MessagesState, input: &[ModelInputItem]) -> Result<(), ModelError> {
        state
            .messages
            .extend(mapping::input_items_to_message(input)?);
        Ok(())
    }

    fn request(
        model: &str,
        state: &MessagesState,
        config: &MessagesConfig,
        max_tokens: Option<u32>,
    ) -> Result<Vec<u8>, ModelError> {
        mapping::build_request(
            model,
            &state.messages,
            &state.api_settings,
            config.effort,
            config.include_reasoning,
            max_tokens,
        )
    }

    async fn read_response(
        body: StreamReader<u8>,
        deltas: &mut Deltas,
    ) -> Result<(ModelResponse, Option<mapping::Message>), ModelError> {
        mapping::stream_response(body, deltas).await
    }

    fn add_output(state: &mut MessagesState, output: Option<mapping::Message>) {
        state.messages.extend(output);
    }

    fn delta_stream() -> (StreamWriter<ModelDelta>, StreamReader<ModelDelta>) {
        wit_stream::new()
    }

    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>) {
        wit_future::new(default)
    }
}

struct SessionState(Session<MessagesClient>);

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
