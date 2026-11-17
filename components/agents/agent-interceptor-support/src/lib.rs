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

use std::future::{Future, IntoFuture, poll_fn};
use std::pin::pin;

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

    /// A new future for a task's cancel signal (`wit_future::new(default)`).
    /// The interceptor creates it because its bindings define the payload
    /// type.
    fn cancel_future(default: fn()) -> (FutureWriter<()>, FutureReader<()>);
}

/// One invocation's interceptor.
pub trait InvocationInterceptor: Sized + 'static {
    /// Called with the invocation the agent started.
    fn on_invocation(&mut self, _invocation: &AgentInvocation) {}

    /// Called before the caller receives each event and returns what action to
    /// take.
    fn on_event(&mut self, _event: &AgentEvent) -> Action {
        Action::Continue
    }

    /// Called once with the task's outcome, which the caller then receives.
    fn on_result(self, _outcome: &Outcome) {}

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
    /// The agent's task is canceled, and its outcome is the agent's response
    /// with the `rejected` state.
    Reject(Message),
}

/// The agent's metadata, as returned by `on_metadata`.
pub async fn metadata<I: AgentInterceptor>() -> Result<AgentMetadata, String> {
    I::on_metadata(agent::metadata().await)
}

/// Creates the invocation's interceptor, sends the request to the agent, and
/// relays the task's response. The agent's task is canceled when `cancel`
/// resolves or the invocation's interceptor rejects the task.
pub async fn invoke<I: AgentInterceptor>(
    mut request: AgentRequest,
    cancel: FutureReader<()>,
) -> Result<AgentInvocation, AgentError> {
    let mut interceptor = I::new_invocation(&mut request).await?;
    let (target_cancel, target_cancel_reader) = I::cancel_future(|| ());
    let invocation = match agent::invoke(request, target_cancel_reader).await {
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
        response: relay::<I>(response, interceptor, cancel, target_cancel),
    })
}

/// A status message from the agent in `event`'s task, with one text part.
pub fn agent_message(event: &AgentEvent, message_id: String, text: String) -> Message {
    let (task_id, context_id) = match event {
        AgentEvent::Status(event) => (&event.task_id, &event.context_id),
        AgentEvent::Artifact(event) => (&event.task_id, &event.context_id),
    };
    Message {
        message_id,
        context_id: Some(context_id.clone()),
        task_id: Some(task_id.clone()),
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

// Relays the target's response to the caller in a new stream and future. The
// caller's events end before its future resolves.
fn relay<I: AgentInterceptor>(
    stream: AgentResponseStream,
    interceptor: I::Invocation,
    cancel: FutureReader<()>,
    target_cancel: FutureWriter<()>,
) -> AgentResponseStream {
    let (caller_events, events) = I::event_stream();
    let (caller_outcome, outcome) = I::outcome_future(dropped);
    spawn_local(async move {
        let (interceptor, rejection) = forward(
            stream.events,
            caller_events,
            interceptor,
            cancel,
            target_cancel,
        )
        .await;
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
        interceptor.on_result(&result);
        let _ = caller_outcome.write(result).await;
    });
    AgentResponseStream {
        events,
        result: outcome,
    }
}

// Forwards each event to the caller until the target's events end or the
// invocation's interceptor rejects the task, returning the rejection's status
// message. Dropping `target_cancel` cancels the target's task, which happens
// when `cancel` resolves or the task is rejected. Once the caller stops
// reading, the interceptor still sees each event, which is then dropped. Both
// event ends are dropped on return, which ends the caller's events.
async fn forward<C: InvocationInterceptor>(
    mut events: StreamReader<AgentEvent>,
    caller: StreamWriter<AgentEvent>,
    mut interceptor: C,
    cancel: FutureReader<()>,
    target_cancel: FutureWriter<()>,
) -> (C, Option<Message>) {
    let mut caller = Some(caller);
    let mut cancel = Some(pin!(cancel.into_future()));
    let mut target_cancel = Some(target_cancel);
    loop {
        // The next event. The caller's cancel is passed on to the target
        // while the event is read, so the read is not interrupted.
        let next = {
            let mut next = pin!(events.next());
            poll_fn(|cx| {
                if let Some(pending) = &mut cancel
                    && pending.as_mut().poll(cx).is_ready()
                {
                    cancel = None;
                    target_cancel = None;
                }
                next.as_mut().poll(cx)
            })
            .await
        };
        let Some(event) = next else {
            return (interceptor, None);
        };
        if let Action::Reject(message) = interceptor.on_event(&event) {
            drop(target_cancel);
            return (interceptor, Some(message));
        }
        if let Some(writer) = &mut caller
            && writer.write_one(event).await.is_some()
        {
            caller = None;
        }
    }
}

fn dropped() -> Outcome {
    Err(AgentError {
        code: INTERNAL_ERROR,
        message: "the interceptor dropped the task".to_string(),
        data: None,
    })
}
