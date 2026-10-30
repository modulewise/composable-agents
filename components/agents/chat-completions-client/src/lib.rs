//! An implementation of the `composable:agents/model-client` interface for
//! OpenAI-compatible Chat Completions providers, using SSE streaming.

wit_bindgen::generate!({
    path: "wit",
    world: "chat-completions-client",
    generate_all,
});

mod mapping;

use std::cell::RefCell;
use std::rc::Rc;

use componentized::http::client::{self, HttpResponse};
use composable::agents::types::{
    ModelError, ModelExchange, ModelRequest, ModelResponseStream, ModelSettings,
};
use exports::composable::agents::model_client::GuestSession;
use wit_bindgen::rt::async_support::spawn_local;

const API_KEY_CONFIG: &str = "api-key";
const BASE_URL_CONFIG: &str = "base-url";
const MAX_TOKENS_CONFIG: &str = "max-tokens";
const MODEL_CONFIG: &str = "model";
const PROVIDER_CONFIG: &str = "provider";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Holds the session's settings, the conversation history in Chat Completions
/// form, and the exchanges this session's calls have made. Chat Completions is
/// stateless, so we keep history client-side and replay it with each call.
/// Both are shared (`Rc`) so the streaming task can record the assembled
/// response once the response stream completes.
struct SessionState {
    settings: ModelSettings,
    history: Rc<RefCell<Vec<mapping::Message>>>,
    exchanges: Rc<RefCell<Vec<ModelExchange>>>,
}

impl GuestSession for SessionState {
    fn new(settings: ModelSettings, history: Vec<ModelExchange>) -> Result<Self, ModelError>
    where
        Self: Sized,
    {
        mapping::require_distinct_tool_names(&settings)?;
        mapping::require_answered_tool_calls(&history)?;
        let messages = mapping::history_to_messages(settings.instructions.as_deref(), &history);
        Ok(SessionState {
            settings,
            history: Rc::new(RefCell::new(messages)),
            exchanges: Rc::new(RefCell::new(Vec::new())),
        })
    }

    async fn call(&self, request: ModelRequest) -> Result<ModelResponseStream, ModelError> {
        // Append this call's input to history, and record the call as an
        // exchange whose output is set once the response is assembled. Input
        // the client cannot send fails the call before it is recorded.
        let input = mapping::input_items_to_messages(&request.input)?;
        self.history.borrow_mut().extend(input);
        // The call's own limit, then the session's, then the client's config.
        let max_tokens = match request.max_tokens.or(self.settings.max_tokens) {
            Some(max_tokens) => Some(max_tokens),
            None => configured_max_tokens()?,
        };
        let exchange = self.exchanges.borrow().len();
        self.exchanges.borrow_mut().push(ModelExchange {
            request,
            output: None,
        });

        let model = required_config(MODEL_CONFIG)?;
        let provider = required_config(PROVIDER_CONFIG)?;
        let body =
            mapping::build_request(&self.settings, &model, max_tokens, &self.history.borrow())?;
        let response = send_request(&self.config_endpoint(), &self.auth_headers(), body).await?;

        // Stream the SSE body in a spawned task. Write deltas as they arrive,
        // assemble the final response, and resolve the outcome future. The
        // assembled assistant turn is appended to history for the next call,
        // and the response's output is recorded in the call's exchange.
        let (mut delta_tx, delta_rx) = wit_stream::new::<composable::agents::types::ModelDelta>();
        let (outcome_tx, outcome_rx) =
            wit_future::new::<Result<composable::agents::types::ModelResponse, ModelError>>(|| {
                Err(ModelError {
                    message: "session dropped before completion".into(),
                    status: None,
                })
            });

        let history = self.history.clone();
        let exchanges = self.exchanges.clone();
        let settings = self.settings.clone();
        spawn_local(async move {
            let outcome =
                match mapping::stream_response(response.body, &mut delta_tx, &settings).await {
                    Ok((resp, assistant)) => {
                        history.borrow_mut().push(assistant);
                        exchanges.borrow_mut()[exchange].output = Some(resp.output.clone());
                        Ok(resp)
                    }
                    Err(e) => Err(e),
                };
            drop(delta_tx);
            let _ = outcome_tx.write(outcome).await;
        });

        Ok(ModelResponseStream {
            model,
            provider,
            deltas: delta_rx,
            result: outcome_rx,
        })
    }

    fn exchanges(&self) -> Vec<ModelExchange> {
        self.exchanges.borrow().clone()
    }
}

impl SessionState {
    fn config_endpoint(&self) -> String {
        let base_url = get_config(BASE_URL_CONFIG).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if let Some(key) = get_config(API_KEY_CONFIG) {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }
        headers
    }
}

/// POST the request body (as a stream) and return the streaming response.
/// On an HTTP error status, the error body is drained and returned as `ModelError`.
async fn send_request(
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<HttpResponse, ModelError> {
    // Feed the (already-assembled) request body through a stream.
    let (mut body_tx, body_rx) = wit_stream::new::<u8>();
    spawn_local(async move {
        let _ = body_tx.write_all(body).await;
    });

    let response = client::post(url.to_string(), headers.to_vec(), body_rx, None)
        .await
        .map_err(|e| ModelError {
            message: format!("HTTP request failed: {e}"),
            status: None,
        })?;

    if response.status >= 400 {
        let status = response.status;
        let mut err_body = Vec::new();
        let mut stream = response.body;
        loop {
            let (result, chunk) = stream.read(Vec::with_capacity(8192)).await;
            err_body.extend_from_slice(&chunk);
            if matches!(
                result,
                wit_bindgen::rt::async_support::StreamResult::Dropped
            ) {
                break;
            }
        }
        return Err(ModelError {
            message: format!(
                "provider returned {}: {}",
                status,
                String::from_utf8_lossy(&err_body)
            ),
            status: Some(status as u32),
        });
    }

    Ok(response)
}

fn get_config(key: &str) -> Option<String> {
    wasi::config::store::get(key).ok().flatten()
}

// The config value for a `key` that is required by the client.
fn required_config(key: &str) -> Result<String, ModelError> {
    get_config(key).ok_or_else(|| ModelError {
        message: format!("no '{key}' configured"),
        status: None,
    })
}

/// The `max-tokens` config, if set.
fn configured_max_tokens() -> Result<Option<u32>, ModelError> {
    get_config(MAX_TOKENS_CONFIG)
        .map(|value| {
            value.parse().map_err(|_| ModelError {
                message: format!("'{MAX_TOKENS_CONFIG}' must be a number, got '{value}'"),
                status: None,
            })
        })
        .transpose()
}

struct Component;

impl exports::composable::agents::model_client::Guest for Component {
    type Session = SessionState;
}

export!(Component);
