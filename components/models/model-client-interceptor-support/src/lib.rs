//! Shared code for `composable:models/client` interceptors. A [`Session`]
//! wraps the client's session and calls a [`SessionInterceptor`] and a
//! [`CallInterceptor`] at each point of a session and its calls. It relays
//! every response itself, so an interceptor never handles a stream or future.
//!
//! An interceptor reuses this crate's bindings with `wit_bindgen::generate!`'s
//! `with` option:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     path: "wit",
//!     world: "my-interceptor",
//!     with: {
//!         "composable:models/types@0.1.0": model_client_interceptor_support::types,
//!         "composable:models/client@0.1.0": model_client_interceptor_support::client,
//!     },
//!     generate_all,
//! });
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "model-client-interceptor-support",
    generate_all,
});

pub use composable::models::{client, types};

use types::{
    ModelCall, ModelDelta, ModelError, ModelExchange, ModelFinishReason, ModelRequest,
    ModelResponse, ModelResponseStream, ModelSettings, SessionError, TokenUsage,
};
use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamWriter, spawn_local,
};

/// The response, or why the call failed.
pub type Outcome = Result<ModelResponse, ModelError>;

/// One session's interceptor.
pub trait SessionInterceptor: Sized + 'static {
    /// The interceptor for one of the session's calls.
    type Call: CallInterceptor;

    /// Creates the session's interceptor, before the client's session is
    /// created. May change the settings, or fail the session with the input it
    /// cannot accept. The history is the conversation so far, which the client
    /// continues as it is.
    fn new(settings: &mut ModelSettings, history: &[ModelExchange]) -> Result<Self, SessionError>;

    /// Creates a call's interceptor, before the client gets the call. May
    /// change the request, or refuse the call, which then fails with nothing
    /// sent or recorded.
    fn new_call(&self, request: &mut ModelRequest) -> Result<Self::Call, ModelError>;

    /// A new stream for a call's deltas (`wit_stream::new()`). The
    /// interceptor creates it because its bindings define the payload type.
    fn delta_stream() -> (StreamWriter<ModelDelta>, StreamReader<ModelDelta>);

    /// A new future for a call's outcome (`wit_future::new(default)`). The
    /// interceptor creates it because its bindings define the payload type.
    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>);
}

/// One call's interceptor.
pub trait CallInterceptor: Sized + 'static {
    /// Called with the call as the client describes it.
    fn on_call(&mut self, _call: &ModelCall) {}

    /// Called before the caller receives each delta, and returns what action
    /// to take.
    fn on_delta(&mut self, _delta: &ModelDelta) -> Action {
        Action::Continue
    }

    /// Called once with the call's outcome, which the caller then receives.
    /// `canceled` is true when the caller stopped reading the deltas.
    fn on_result(self, _outcome: &Outcome, _canceled: bool) {}

    /// Called with the error the call returned instead of a response, and
    /// returns the error the caller receives.
    fn on_error(self, error: ModelError) -> ModelError {
        error
    }
}

/// What to do with a delta.
pub enum Action {
    /// Relay the delta, and continue.
    Continue,
    /// Block the call's output, without relaying the delta. The client's call
    /// is canceled, and the outcome is a response with no output and the
    /// finish reason `blocked(value)`, where `value` is this raw finish reason
    /// (e.g. "content_filter").
    Reject(String),
}

/// A client session wrapped by interceptor `I`.
pub struct Session<I: SessionInterceptor> {
    target: client::Session,
    interceptor: I,
}

impl<I: SessionInterceptor> Session<I> {
    /// Creates the interceptor, then the client's session.
    pub fn new(
        mut settings: ModelSettings,
        history: Vec<ModelExchange>,
    ) -> Result<Self, SessionError> {
        let interceptor = I::new(&mut settings, &history)?;
        let target = client::Session::new(&settings, &history)?;
        Ok(Session {
            target,
            interceptor,
        })
    }

    /// Creates the call's interceptor, sends the call to the client, and
    /// relays its response.
    pub async fn call(&self, mut request: ModelRequest) -> Result<ModelCall, ModelError> {
        let mut interceptor = self.interceptor.new_call(&mut request)?;
        let call = match self.target.call(request).await {
            Ok(call) => call,
            Err(error) => return Err(interceptor.on_error(error)),
        };
        interceptor.on_call(&call);
        let response = match call.response {
            Ok(stream) => Ok(relay::<I>(stream, interceptor)),
            Err(error) => Err(interceptor.on_error(error)),
        };
        Ok(ModelCall { response, ..call })
    }

    /// The client's exchanges.
    pub fn exchanges(&self) -> Vec<ModelExchange> {
        self.target.exchanges()
    }
}

// Relays the client's response to the caller in a new stream and future. The
// caller's deltas end before its future resolves.
fn relay<I: SessionInterceptor>(
    stream: ModelResponseStream,
    interceptor: I::Call,
) -> ModelResponseStream {
    let (caller_deltas, deltas) = I::delta_stream();
    let (caller_outcome, outcome) = I::outcome_future(dropped);
    spawn_local(async move {
        let (interceptor, ending) = forward(stream.deltas, caller_deltas, interceptor).await;
        let result = stream.result.await;
        let (result, canceled) = match ending {
            Forwarded::Ended => (result, false),
            Forwarded::Canceled => (result, true),
            Forwarded::Rejected(reason, usage) => (Ok(blocked(reason, usage)), false),
        };
        interceptor.on_result(&result, canceled);
        let _ = caller_outcome.write(result).await;
    });
    ModelResponseStream {
        deltas,
        result: outcome,
    }
}

// Why `forward` returned.
enum Forwarded {
    Ended,
    Canceled,
    // The call interceptor rejected a delta with this finish reason. The
    // counts are from the last usage delta relayed.
    Rejected(String, TokenUsage),
}

// Forwards each delta to the caller until the deltas end, the caller stops
// reading them, or the call interceptor rejects one. Both ends are dropped on
// return, which ends the caller's deltas.
async fn forward<C: CallInterceptor>(
    mut deltas: StreamReader<ModelDelta>,
    mut caller: StreamWriter<ModelDelta>,
    mut interceptor: C,
) -> (C, Forwarded) {
    let mut usage = TokenUsage {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    };
    while let Some(delta) = deltas.next().await {
        if let Action::Reject(reason) = interceptor.on_delta(&delta) {
            return (interceptor, Forwarded::Rejected(reason, usage));
        }
        if let ModelDelta::Usage(counts) = &delta {
            usage = *counts;
        }
        if caller.write_one(delta).await.is_some() {
            return (interceptor, Forwarded::Canceled);
        }
    }
    (interceptor, Forwarded::Ended)
}

// The response to a call the call interceptor rejected, with its output
// discarded.
fn blocked(reason: String, usage: TokenUsage) -> ModelResponse {
    ModelResponse {
        output: Vec::new(),
        finish_reason: ModelFinishReason::Blocked(reason),
        usage,
        response_id: None,
        model: None,
        opaque_content: None,
    }
}

fn dropped() -> Outcome {
    Err(ModelError {
        message: "the interceptor dropped the call".to_string(),
        code: None,
        status: None,
    })
}
