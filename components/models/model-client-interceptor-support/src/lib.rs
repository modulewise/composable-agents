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
    ModelCall, ModelDelta, ModelError, ModelExchange, ModelRequest, ModelResponse,
    ModelResponseStream, ModelSettings,
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
    /// created. May change the settings or history, or fail the session.
    fn new(
        settings: &mut ModelSettings,
        history: &mut Vec<ModelExchange>,
    ) -> Result<Self, ModelError>;

    /// Creates a call's interceptor, before the client gets the call. May
    /// change the request, or refuse the call, which then fails with nothing
    /// sent or recorded.
    fn new_call(&self, request: &mut ModelRequest) -> Result<Self::Call, ModelError>;

    /// Called with the client's exchanges, and returns the ones the caller
    /// gets.
    fn on_exchanges(&self, exchanges: Vec<ModelExchange>) -> Vec<ModelExchange> {
        exchanges
    }

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
    fn on_describe(&mut self, _call: &ModelCall) {}

    /// Called before the caller gets each delta. May change the delta, or stop
    /// the call, in which case this delta isn't relayed.
    fn on_delta(&mut self, _delta: &mut ModelDelta) -> Flow {
        Flow::Continue
    }

    /// Called once when the call ends, and returns the outcome the caller
    /// gets. A changed outcome isn't in the client's exchanges, which
    /// `on_exchanges` can change to match.
    fn on_end(self, _ending: Ending, outcome: Outcome) -> Outcome {
        outcome
    }
}

/// Whether a call continues after a delta.
pub enum Flow {
    Continue,
    /// Stops the call. The error is its outcome.
    Stop(ModelError),
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The client did not send the call.
    NotSent,
    /// The call's deltas ended.
    Ended,
    /// The caller stopped reading the deltas.
    Canceled,
    /// The call interceptor stopped the call.
    Stopped,
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
        mut history: Vec<ModelExchange>,
    ) -> Result<Self, ModelError> {
        let interceptor = I::new(&mut settings, &mut history)?;
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
        let call = self.target.call(request).await?;
        interceptor.on_describe(&call);
        let response = match call.response {
            Ok(stream) => Ok(relay::<I>(stream, interceptor)),
            Err(error) => match interceptor.on_end(Ending::NotSent, Err(error)) {
                Ok(response) => Ok(resolved::<I>(response)),
                Err(error) => Err(error),
            },
        };
        Ok(ModelCall { response, ..call })
    }

    /// The client's exchanges, as `on_exchanges` returns them.
    pub fn exchanges(&self) -> Vec<ModelExchange> {
        self.interceptor.on_exchanges(self.target.exchanges())
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
        let result = match ending {
            Forwarded::Ended => interceptor.on_end(Ending::Ended, result),
            Forwarded::Canceled => interceptor.on_end(Ending::Canceled, result),
            Forwarded::Stopped(reason) => interceptor.on_end(Ending::Stopped, Err(reason)),
        };
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
    Stopped(ModelError),
}

// Forwards each delta to the caller until the deltas end, the caller stops
// reading them, or the call interceptor stops the call. Both ends are dropped
// on return, which ends the caller's deltas.
async fn forward<C: CallInterceptor>(
    mut deltas: StreamReader<ModelDelta>,
    mut caller: StreamWriter<ModelDelta>,
    mut interceptor: C,
) -> (C, Forwarded) {
    while let Some(mut delta) = deltas.next().await {
        if let Flow::Stop(reason) = interceptor.on_delta(&mut delta) {
            return (interceptor, Forwarded::Stopped(reason));
        }
        if caller.write_one(delta).await.is_some() {
            return (interceptor, Forwarded::Canceled);
        }
    }
    (interceptor, Forwarded::Ended)
}

// A response with no deltas, for a call the client did not send that
// `on_end` answered.
fn resolved<I: SessionInterceptor>(response: ModelResponse) -> ModelResponseStream {
    let (caller_deltas, deltas) = I::delta_stream();
    drop(caller_deltas);
    let (caller_outcome, outcome) = I::outcome_future(dropped);
    spawn_local(async move {
        let _ = caller_outcome.write(Ok(response)).await;
    });
    ModelResponseStream {
        deltas,
        result: outcome,
    }
}

fn dropped() -> Outcome {
    Err(ModelError {
        message: "the interceptor dropped the call".to_string(),
        code: None,
        status: None,
    })
}
