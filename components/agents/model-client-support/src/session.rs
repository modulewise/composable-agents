//! A model-client session, shared by every client.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamWriter, spawn_local,
};

use crate::composable::agents::types::{
    ModelDelta, ModelError, ModelExchange, ModelInputItem, ModelRequest, ModelResponse,
    ModelResponseStream, ModelSettings, TokenUsage,
};
use crate::config::CommonConfig;
use crate::http::send_request;
use crate::tool_names::{ToolNames, sent_history, sent_input, sent_settings};

/// A call's outcome: the assembled response, or why the call failed.
pub type Outcome = Result<ModelResponse, ModelError>;

/// A client for one model API, run by a [`Session`]. It sees tool names as
/// sent to the API (see `tool_names`).
pub trait Client: 'static {
    /// The client's own config, beyond [`CommonConfig`], read once per
    /// session.
    type Config: 'static;
    /// State kept between a session's calls, e.g. the conversation history in
    /// the API's form.
    type State: 'static;
    /// What a response adds to the state, such as the model's turn.
    type Output: 'static;

    /// The base URL when none is configured.
    const DEFAULT_BASE_URL: &'static str;
    /// The endpoint's path under the base URL.
    const PATH: &'static str;

    /// Reads the client's own config. Fails on a missing or invalid value.
    fn config() -> Result<Self::Config, ModelError>;

    /// The request headers, with the API key when one is configured.
    fn headers(api_key: Option<&str>) -> Vec<(String, String)>;

    /// The maximum number of retries for a failed request (default 0). A
    /// client that can return a nonzero value must implement `sleep`.
    fn max_retries(_config: &Self::Config) -> u32 {
        0
    }

    /// Waits `duration` before a retry. A client whose `max_retries` can be
    /// a nonzero value must implement it since the default panics.
    fn sleep(_duration: Duration) -> impl Future<Output = ()> {
        async { unreachable!("a client that retries must implement `sleep`") }
    }

    /// The initial state, from the settings and earlier exchanges. Fails on
    /// history the client can't use.
    fn start(
        settings: &ModelSettings,
        history: &[ModelExchange],
    ) -> Result<Self::State, ModelError>;

    /// Adds a new call's input to the state. Fails on input the client cannot
    /// send, before the call is recorded.
    fn add_input(state: &mut Self::State, input: &[ModelInputItem]) -> Result<(), ModelError>;

    /// The request body for a call. `max_tokens` is the call's limit, else the
    /// session's, else the configured one.
    fn request(
        model: &str,
        state: &Self::State,
        settings: &ModelSettings,
        config: &Self::Config,
        max_tokens: Option<u32>,
    ) -> Result<Vec<u8>, ModelError>;

    /// Reads the response body, writing deltas as they arrive, and returns the
    /// assembled response and its state update. Token counts the provider
    /// reports are written as usage deltas. If the caller stops reading the
    /// deltas, a write fails; the client returns that error and drops the
    /// body, closing the connection.
    fn read_response(
        body: StreamReader<u8>,
        deltas: &mut Deltas,
    ) -> impl Future<Output = Result<(ModelResponse, Self::Output), ModelError>>;

    /// Adds a response's output to the state, for the next call.
    fn add_output(state: &mut Self::State, output: Self::Output);

    /// A new stream for a call's deltas (`wit_stream::new()`). The client
    /// creates it because its bindings define the payload type.
    fn delta_stream() -> (StreamWriter<ModelDelta>, StreamReader<ModelDelta>);

    /// A new future for a call's outcome (`wit_future::new(default)`). The
    /// client creates it because its bindings define the payload type.
    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>);
}

/// Writes a call's deltas, mapping tool-call names back to the original names.
/// Records each usage delta in the call's exchange.
pub struct Deltas {
    writer: StreamWriter<ModelDelta>,
    names: Rc<ToolNames>,
    exchange: Rc<RefCell<ModelExchange>>,
}

impl Deltas {
    /// Writes `delta`. A usage delta is recorded in the call's exchange first.
    /// Fails once the caller has stopped reading the deltas.
    pub async fn write(&mut self, delta: ModelDelta) -> Result<(), ModelError> {
        if let ModelDelta::Usage(usage) = &delta {
            self.exchange.borrow_mut().usage = *usage;
        }
        match self
            .writer
            .write_one(self.names.original_delta(delta))
            .await
        {
            None => Ok(()),
            Some(_) => Err(ModelError {
                message: "the caller stopped reading the response".to_string(),
                status: None,
                usage: TokenUsage::default(),
            }),
        }
    }
}

/// The model calls of one agent invocation, made by client `C`. Reads config
/// on creation and records each call's exchange.
pub struct Session<C: Client> {
    // With tool names as sent.
    settings: ModelSettings,
    common: CommonConfig,
    config: C::Config,
    names: Rc<ToolNames>,
    // Shared with the streaming task, which records the response when it
    // completes.
    state: Rc<RefCell<C::State>>,
    // Each call's exchange, shared with the task that reads its response.
    exchanges: RefCell<Vec<Rc<RefCell<ModelExchange>>>>,
}

impl<C: Client> Session<C> {
    /// Fails on missing or invalid config, invalid tool names, or history the
    /// client cannot use.
    pub fn new(settings: ModelSettings, history: Vec<ModelExchange>) -> Result<Self, ModelError> {
        let common = CommonConfig::load(C::DEFAULT_BASE_URL)?;
        let config = C::config()?;
        let names = ToolNames::new(&settings)?;
        let settings = sent_settings(&settings);
        let state = C::start(&settings, &sent_history(history))?;
        Ok(Session {
            settings,
            common,
            config,
            names: Rc::new(names),
            state: Rc::new(RefCell::new(state)),
            exchanges: RefCell::new(Vec::new()),
        })
    }

    pub async fn call(&self, request: ModelRequest) -> Result<ModelResponseStream, ModelError> {
        // Add the input to the state and record the exchange. Its output and
        // counts are set as the response is read. Unsendable input fails the
        // call before it's recorded.
        C::add_input(&mut self.state.borrow_mut(), &sent_input(&request.input))?;
        // The call's limit, else the session's, else the configured one.
        let max_tokens = request
            .max_tokens
            .or(self.settings.max_tokens)
            .or(self.common.max_tokens);
        let exchange = Rc::new(RefCell::new(ModelExchange {
            request,
            output: None,
            opaque_content: None,
            provider: Some(self.common.provider.clone()),
            request_model: Some(self.common.model.clone()),
            response_model: None,
            usage: TokenUsage::default(),
        }));
        self.exchanges.borrow_mut().push(exchange.clone());

        let body = C::request(
            &self.common.model,
            &self.state.borrow(),
            &self.settings,
            &self.config,
            max_tokens,
        )?;
        let headers = C::headers(self.common.api_key.as_deref());
        let response = send_request(
            &self.common.endpoint(C::PATH),
            &headers,
            body,
            C::max_retries(&self.config),
            C::sleep,
        )
        .await?;

        // Read the response in a spawned task: write the deltas, assemble the
        // response, update the state, and resolve the outcome. A response's
        // output (with the original tool names), opaque content, model and
        // counts are recorded in the exchange. A failure gets the counts
        // that were recorded before it occurred.
        let (delta_tx, delta_rx) = C::delta_stream();
        let (outcome_tx, outcome_rx) = C::outcome_future(|| {
            Err(ModelError {
                message: "session dropped before completion".into(),
                status: None,
                usage: TokenUsage::default(),
            })
        });

        let state = self.state.clone();
        let names = self.names.clone();
        spawn_local(async move {
            let mut deltas = Deltas {
                writer: delta_tx,
                names: names.clone(),
                exchange: exchange.clone(),
            };
            let outcome = C::read_response(response.body, &mut deltas).await;
            drop(deltas);
            let outcome = match outcome {
                Ok((mut response, output)) => {
                    C::add_output(&mut state.borrow_mut(), output);
                    response.output = names.original_output(response.output);
                    let mut recorded = exchange.borrow_mut();
                    recorded.output = Some(response.output.clone());
                    recorded.opaque_content = response.opaque_content.clone();
                    recorded.response_model = response.model.clone();
                    recorded.usage = response.usage;
                    Ok(response)
                }
                Err(mut error) => {
                    error.usage = exchange.borrow().usage;
                    Err(error)
                }
            };
            let _ = outcome_tx.write(outcome).await;
        });

        Ok(ModelResponseStream {
            request_model: self.common.model.clone(),
            provider: self.common.provider.clone(),
            deltas: delta_rx,
            result: outcome_rx,
        })
    }

    pub fn exchanges(&self) -> Vec<ModelExchange> {
        self.exchanges
            .borrow()
            .iter()
            .map(|exchange| exchange.borrow().clone())
            .collect()
    }
}
