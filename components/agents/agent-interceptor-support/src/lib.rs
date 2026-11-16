//! Shared code for `composable:agents/agent` interceptors. [`invoke`] calls
//! the agent's `invoke`, as well as an [`AgentInterceptor`] and an
//! [`InvocationInterceptor`] at each point of the invocation.
//!
//! An interceptor reuses this crate's bindings with `wit_bindgen::generate!`'s
//! `with` option:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     path: "wit",
//!     world: "my-interceptor",
//!     with: {
//!         "composable:models/types@0.1.0": agent_interceptor_support::model_types,
//!         "composable:agents/types@0.1.0": agent_interceptor_support::types,
//!         "composable:agents/agent@0.1.0": agent_interceptor_support::agent,
//!     },
//!     generate_all,
//! });
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "agent-interceptor-support",
    generate_all,
});

pub use composable::agents::{agent, types};
pub use composable::models::types as model_types;

use std::future::Future;

use types::{
    AgentError, AgentEvent, AgentInvocation, AgentMetadata, AgentRequest, AgentResponse,
    AgentResponseStream, Message, Part, PartContent, Role, TaskState, TaskStatus,
};
use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamWriter, spawn_local,
};

// JSON-RPC's internal error code.
const INTERNAL_ERROR: i32 = -32603;

/// The task's response, or why the agent failed to produce one.
pub type Outcome = Result<AgentResponse, AgentError>;

/// An agent's interceptor.
pub trait AgentInterceptor: 'static {
    /// The interceptor for one invocation.
    type Invocation: InvocationInterceptor;

    /// Creates an invocation's interceptor, before the agent receives the
    /// request. May change the request, or return an error, in which case no
    /// task starts.
    fn new_invocation(
        request: &mut AgentRequest,
    ) -> impl Future<Output = Result<Self::Invocation, AgentError>>;

    /// Called with the agent's metadata, and returns the metadata the caller
    /// receives.
    fn on_metadata(metadata: Result<AgentMetadata, String>) -> Result<AgentMetadata, String> {
        metadata
    }

    /// A new stream for a task's events (`wit_stream::new()`). The
    /// interceptor creates it because its bindings define the payload type.
    fn event_stream() -> (StreamWriter<AgentEvent>, StreamReader<AgentEvent>);

    /// A new future for a task's outcome (`wit_future::new(default)`). The
    /// interceptor creates it because its bindings define the payload type.
    fn outcome_future(default: fn() -> Outcome) -> (FutureWriter<Outcome>, FutureReader<Outcome>);
}

/// One invocation's interceptor.
pub trait InvocationInterceptor: Sized + 'static {
    /// Called with the invocation the agent started.
    fn on_invocation(&mut self, _invocation: &AgentInvocation) {}

    /// Called before the caller receives each event. May change the event, and
    /// returns what action to take.
    fn on_event(&mut self, _event: &mut AgentEvent) -> Action {
        Action::Continue
    }

    /// Called once with the task's outcome, and returns the outcome the caller
    /// receives.
    fn on_result(self, outcome: Outcome) -> Outcome {
        outcome
    }

    /// Called with the error the agent returned instead of starting a task,
    /// and returns the error the caller receives.
    fn on_error(self, error: AgentError) -> AgentError {
        error
    }
}

/// What to do with an event.
pub enum Action {
    /// Relay the event, and continue.
    Continue,
    /// Reject the task with this status message, without relaying the event.
    /// The task's outcome is the agent's response with the `rejected` state.
    Reject(Message),
}

/// The agent's metadata, as returned by `on_metadata`.
pub async fn metadata<I: AgentInterceptor>() -> Result<AgentMetadata, String> {
    I::on_metadata(agent::metadata().await)
}

/// Creates the invocation's interceptor, sends the request to the agent, and
/// relays the task's response.
pub async fn invoke<I: AgentInterceptor>(
    mut request: AgentRequest,
) -> Result<AgentInvocation, AgentError> {
    let mut interceptor = I::new_invocation(&mut request).await?;
    let invocation = match agent::invoke(request).await {
        Ok(invocation) => invocation,
        Err(error) => return Err(interceptor.on_error(error)),
    };
    interceptor.on_invocation(&invocation);
    let AgentInvocation {
        task_id,
        context_id,
        activated_extensions,
        response,
    } = invocation;
    Ok(AgentInvocation {
        task_id,
        context_id,
        activated_extensions,
        response: relay::<I>(response, interceptor),
    })
}

/// A status message from the agent in `invocation`'s task, with one text part.
pub fn agent_message(invocation: &AgentInvocation, message_id: String, text: String) -> Message {
    Message {
        message_id,
        context_id: Some(invocation.context_id.clone()),
        task_id: Some(invocation.task_id.clone()),
        role: Role::Agent,
        parts: vec![Part {
            content: PartContent::Text(text),
            metadata: None,
            filename: None,
            media_type: None,
        }],
        metadata: None,
        extensions: Vec::new(),
        reference_task_ids: Vec::new(),
    }
}

// Relays the agent's response to the caller in a new stream and future. The
// caller's events end before its future resolves.
fn relay<I: AgentInterceptor>(
    stream: AgentResponseStream,
    interceptor: I::Invocation,
) -> AgentResponseStream {
    let (caller_events, events) = I::event_stream();
    let (caller_outcome, outcome) = I::outcome_future(dropped);
    spawn_local(async move {
        let (interceptor, rejection) = forward(stream.events, caller_events, interceptor).await;
        let result = match (stream.result.await, rejection) {
            (Ok(mut response), Some(message)) => {
                response.status = TaskStatus {
                    state: TaskState::Rejected,
                    message: Some(message),
                    timestamp: None,
                };
                Ok(response)
            }
            (result, _) => result,
        };
        let _ = caller_outcome.write(interceptor.on_result(result)).await;
    });
    AgentResponseStream {
        events,
        result: outcome,
    }
}

// Forwards each event to the caller until the events end, the caller stops
// reading them, or the invocation's interceptor rejects the task, returning
// the rejection's status message. Both ends are dropped on return, which ends
// the caller's events.
async fn forward<C: InvocationInterceptor>(
    mut events: StreamReader<AgentEvent>,
    mut caller: StreamWriter<AgentEvent>,
    mut interceptor: C,
) -> (C, Option<Message>) {
    while let Some(mut event) = events.next().await {
        if let Action::Reject(message) = interceptor.on_event(&mut event) {
            return (interceptor, Some(message));
        }
        if caller.write_one(event).await.is_some() {
            return (interceptor, None);
        }
    }
    (interceptor, None)
}

fn dropped() -> Outcome {
    Err(AgentError {
        code: INTERNAL_ERROR,
        message: "the interceptor dropped the task".to_string(),
        data: None,
    })
}
