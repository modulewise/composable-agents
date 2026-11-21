//! Runs of the agent. Each message the agent accepts runs as its own spawned
//! task, which applies the agent's events and outcome to the stores and
//! publishes them to the run's subscribers. A request observes a run, by
//! waiting for it to end, by subscribing to it, or by canceling it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use a2a_json as a2a;
use a2a_json::jsonrpc::{Error, code};
use composable_agents::types::{
    AgentError, AgentEvent, AgentRequest, AgentResponse, Message, Part, PartContent, Role, Task,
    TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use composable_agents::{AcceptedTask, AgentHost, Outcome};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot, watch};

use crate::convert::{a2a_artifact, a2a_event, a2a_status, a2a_task, timestamped, trimmed};
use crate::store::{
    ExchangeStore, StoreError, StoreResult, TaskStore, is_interrupted, is_terminal,
};

/// How many responses can wait for a subscriber to read them. A subscriber
/// whose buffer is full is removed, which ends its stream.
pub(crate) const SUBSCRIBER_BUFFER: usize = 256;

/// The lifetime scope of a subscriber's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The run a message starts. The stream ends when that run ends.
    Run,
    /// The task. The stream ends when the task is in a terminal state.
    Task,
}

// A subscriber to a task's responses.
struct Subscriber {
    sender: mpsc::Sender<a2a::StreamResponse>,
    scope: Scope,
}

// A task's subscribers. The lock is held while an event is applied to the
// store and published, and while a subscriber is sent the stored task and
// added. So each event reaches a subscriber once, in the task or as a response.
type Subscribers = Arc<tokio::sync::Mutex<Vec<Subscriber>>>;

/// Runs one agent, with the stores its runs update.
pub(crate) struct Runner<T, E> {
    host: Arc<AgentHost>,
    agent: String,
    pub(crate) tasks: T,
    pub(crate) exchanges: E,
    // The runs in progress, by task id.
    running: Mutex<HashMap<String, Run>>,
    // The subscribers of each task that has any, by task id. A task's
    // subscribers are removed when it is in a terminal state.
    subscribers: Mutex<HashMap<String, Subscribers>>,
}

// A run in progress.
struct Run {
    // Resolves the run's cancel future. `None` once a cancel was requested.
    cancel: Option<oneshot::Sender<()>>,
    // Becomes `true` when the run ends.
    done: watch::Receiver<bool>,
}

impl<T: TaskStore, E: ExchangeStore> Runner<T, E> {
    pub(crate) fn new(host: Arc<AgentHost>, agent: String, tasks: T, exchanges: E) -> Self {
        Runner {
            host,
            agent,
            tasks,
            exchanges,
            running: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(HashMap::new()),
        }
    }

    /// Starts a run of the agent with `request`, and returns as soon as the
    /// agent accepts the request as a task. The task is then in the store
    /// either as a new task, or with `continued` the task the request names,
    /// with the request's message added to its history. Fails with the agent's
    /// error when it does not accept the request, and then no task is stored.
    ///
    /// A `subscriber` is sent the task as stored when the agent accepts, with
    /// at most `history_length` messages of its history, and then every
    /// response of the run.
    pub(crate) async fn start(
        self: &Arc<Self>,
        request: AgentRequest,
        continued: bool,
        subscriber: Option<mpsc::Sender<a2a::StreamResponse>>,
        history_length: Option<usize>,
    ) -> Result<AcceptedTask, Error> {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let runner = self.clone();
        tokio::spawn(async move {
            runner
                .run(request, continued, subscriber, history_length, accepted_tx)
                .await
        });
        accepted_rx
            .await
            .unwrap_or_else(|_| Err(internal("the agent's run stopped before it accepted")))
    }

    /// Subscribes to the task with `task_id`, with or without a run in
    /// progress. The `subscriber` is sent the task as stored, and then every
    /// response published for the task after that, until the task is in a
    /// terminal state. Fails when the task is not stored, or is already in a
    /// terminal state.
    pub(crate) async fn subscribe(
        &self,
        task_id: &str,
        subscriber: mpsc::Sender<a2a::StreamResponse>,
    ) -> Result<(), Error> {
        let mut subscribers = self.lock_subscribers(task_id).await;
        // A subscriber whose client has disconnected is removed.
        subscribers.retain(|subscriber| !subscriber.sender.is_closed());
        let task = match self.tasks.get(task_id).await.map_err(store_error) {
            Ok(Some(task)) if !is_terminal(task.status.state) => Ok(task),
            Ok(Some(_)) => Err(Error::new(
                code::UNSUPPORTED_OPERATION,
                format!("task {task_id} is in a terminal state"),
            )),
            Ok(None) => Err(task_not_found(task_id)),
            Err(error) => Err(error),
        };
        let added = task
            .and_then(|task| add_subscriber(&mut subscribers, task, subscriber, Scope::Task, None));
        if subscribers.is_empty() {
            self.subscribers.lock().unwrap().remove(task_id);
        }
        added
    }

    // Locks the subscribers of the task with `task_id`. If the task has no
    // subscribers, it's an empty list.
    async fn lock_subscribers(&self, task_id: &str) -> OwnedMutexGuard<Vec<Subscriber>> {
        loop {
            let subscribers = {
                let mut all = self.subscribers.lock().unwrap();
                match all.get(task_id) {
                    Some(subscribers) => subscribers.clone(),
                    None => all.entry(task_id.to_string()).or_default().clone(),
                }
            };
            let locked = subscribers.clone().lock_owned().await;
            // The list may have been removed while this waited for its lock.
            let current = self
                .subscribers
                .lock()
                .unwrap()
                .get(task_id)
                .is_some_and(|current| Arc::ptr_eq(current, &subscribers));
            if current {
                return locked;
            }
        }
    }

    /// Waits until the run of the task with `task_id` ends. Returns at once
    /// when no run of that task is in progress.
    pub(crate) async fn wait(&self, task_id: &str) {
        let done = self
            .running
            .lock()
            .unwrap()
            .get(task_id)
            .map(|run| run.done.clone());
        if let Some(mut done) = done {
            let _ = done.wait_for(|done| *done).await;
        }
    }

    /// Cancels the task with `task_id`. With a run in progress, it cancels the
    /// run and waits until it ends. With none, it sets the task's status to
    /// `canceled` and publishes that status. Fails when a task with no run is
    /// not stored, or is in a terminal state.
    pub(crate) async fn cancel(&self, task_id: &str) -> Result<(), Error> {
        let run = self
            .running
            .lock()
            .unwrap()
            .get_mut(task_id)
            .map(|run| run.cancel.take());
        if let Some(cancel) = run {
            if let Some(cancel) = cancel {
                let _ = cancel.send(());
            }
            self.wait(task_id).await;
            return Ok(());
        }

        let mut subscribers = self.lock_subscribers(task_id).await;
        let canceled = match self.tasks.get(task_id).await.map_err(store_error) {
            Ok(Some(task)) if is_terminal(task.status.state) => Err(Error::new(
                code::TASK_NOT_CANCELABLE,
                format!("task {task_id} is in a terminal state"),
            )),
            Ok(Some(task)) => {
                let event = AgentEvent::Status(TaskStatusUpdateEvent {
                    task_id: task.id.clone(),
                    context_id: task.context_id.clone().unwrap_or_default(),
                    status: timestamped(TaskStatus {
                        state: TaskState::Canceled,
                        message: None,
                        timestamp: None,
                    }),
                    metadata: None,
                });
                let response = a2a_event(event.clone());
                let applied = self.tasks.apply(event).await.map_err(store_error);
                if applied.is_ok()
                    && let Ok(response) = response
                {
                    publish(&mut subscribers, response);
                }
                applied
            }
            Ok(None) => Err(task_not_found(task_id)),
            Err(error) => Err(error),
        };
        // The task is canceled, in another terminal state, or not stored,
        // unless the store failed.
        let terminal = !matches!(&canceled, Err(error) if error.code == code::INTERNAL_ERROR);
        self.end_streams(&mut subscribers, task_id, terminal);
        canceled
    }

    // Ends the streams of each task that has subscribers, has no run in
    // progress, and that is no longer in the store, such as a task it evicted.
    // No more responses can be published for such a task.
    async fn end_streams_of_evicted_tasks(&self) {
        let task_ids: Vec<String> = {
            let running = self.running.lock().unwrap();
            let subscribers = self.subscribers.lock().unwrap();
            subscribers
                .keys()
                .filter(|task_id| !running.contains_key(*task_id))
                .cloned()
                .collect()
        };
        for task_id in task_ids {
            let mut subscribers = self.lock_subscribers(&task_id).await;
            let evicted = matches!(self.tasks.get(&task_id).await, Ok(None));
            if evicted {
                // Dropping the subscribers ends their streams.
                subscribers.clear();
            }
            // A task with no subscribers has no entry in the map.
            if subscribers.is_empty() {
                self.subscribers.lock().unwrap().remove(&task_id);
            }
        }
    }

    // Removes the subscribers whose streams end when a run of the task ends.
    // That includes all of them when the task is `terminal`, otherwise only
    // the ones with `Scope::Run`.
    fn end_streams(&self, subscribers: &mut Vec<Subscriber>, task_id: &str, terminal: bool) {
        if terminal {
            subscribers.clear();
        } else {
            subscribers.retain(|subscriber| subscriber.scope == Scope::Task);
        }
        if subscribers.is_empty() {
            self.subscribers.lock().unwrap().remove(task_id);
        }
    }

    // Invokes the agent, then applies the task it accepts, its events and its
    // outcome to the stores. `accepted_tx` is given the accepted task, or the
    // error when the agent accepts none.
    async fn run(
        self: Arc<Self>,
        request: AgentRequest,
        continued: bool,
        subscriber: Option<mpsc::Sender<a2a::StreamResponse>>,
        history_length: Option<usize>,
        accepted_tx: oneshot::Sender<Result<AcceptedTask, Error>>,
    ) {
        let message = request.message.clone();
        // Dropping the sender would cancel the task, so it is kept until the
        // run ends, in the run's entry in `running`, and before the run has
        // one, in `apply`.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let (done_tx, done_rx) = watch::channel(false);
        let mut cancel_tx = Some(cancel_tx);
        // A run that continues a task is added to `running` before the agent
        // is invoked so that canceling the task cancels this run.
        let continued_id = request.task_id.clone().filter(|_| continued);
        if let Some(task_id) = &continued_id {
            let run = Run {
                cancel: cancel_tx.take(),
                done: done_rx.clone(),
            };
            if let Err(error) = self.continue_task(task_id, run).await {
                let _ = accepted_tx.send(Err(error));
                return;
            }
        }
        // The callbacks cannot await the stores, so they send the task and
        // each event to `apply`.
        let (task_tx, task_rx) = oneshot::channel();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let invoke = self.host.invoke(
            &self.agent,
            request,
            async move {
                let _ = cancel_rx.await;
            },
            move |task| {
                let _ = task_tx.send(task);
            },
            move |event| {
                let _ = events_tx.send(event);
            },
        );
        // Stores the accepted task and reports it, then applies each event
        // until the invocation ends. When the agent accepts no task, it
        // returns the senders it did not use.
        let apply = async {
            let Ok(task) = task_rx.await else {
                return Err((accepted_tx, cancel_tx));
            };
            let stored = if continued {
                self.tasks.add_history(&task.task_id, message).await
            } else {
                self.tasks.insert(new_task(&task, message)).await
            };
            let mut stored = stored.map_err(store_error);
            // The store may have evicted a task to add the new one.
            if !continued {
                let runner = self.clone();
                tokio::spawn(async move { runner.end_streams_of_evicted_tasks().await });
            }
            // The request's subscriber is added before any event is applied,
            // so it is sent every event of the run.
            if stored.is_ok()
                && let Some(subscriber) = subscriber
            {
                let mut subscribers = self.lock_subscribers(&task.task_id).await;
                stored = match self.tasks.get(&task.task_id).await.map_err(store_error) {
                    Ok(Some(stored)) => add_subscriber(
                        &mut subscribers,
                        stored,
                        subscriber,
                        Scope::Run,
                        history_length,
                    ),
                    Ok(None) => Err(internal("the task is no longer stored")),
                    Err(error) => Err(error),
                };
            }
            // The run of a new task is added to `running` as soon as the agent
            // accepts the message.
            if let Some(cancel_tx) = cancel_tx {
                self.running.lock().unwrap().insert(
                    task.task_id.clone(),
                    Run {
                        cancel: Some(cancel_tx),
                        done: done_rx,
                    },
                );
            }
            let _ = accepted_tx.send(stored.map(|()| task.clone()));
            while let Some(event) = events_rx.recv().await {
                self.apply_event(&task, event).await;
            }
            Ok(task)
        };
        let (outcome, applied) = tokio::join!(invoke, apply);

        match applied {
            Ok(task) => self.end(&task, outcome).await,
            Err((accepted_tx, _cancel_tx)) => {
                let error = match outcome {
                    Ok(Err(error)) => agent_error(error),
                    Ok(Ok(_)) => internal("the agent returned an outcome for no task"),
                    Err(error) => internal(format!("the agent failed: {error}")),
                };
                // The agent did not accept the message that continues a task,
                // so the task has no run.
                if let Some(task_id) = &continued_id {
                    self.running.lock().unwrap().remove(task_id);
                }
                let _ = accepted_tx.send(Err(error));
            }
        }
        let _ = done_tx.send(true);
    }

    // Adds `run` as the run in progress of the interrupted task with
    // `task_id`, after any run of that task that has not ended yet. Fails when
    // the task is not stored, or is not interrupted.
    async fn continue_task(&self, task_id: &str, run: Run) -> Result<(), Error> {
        let mut run = Some(run);
        loop {
            let subscribers = self.lock_subscribers(task_id).await;
            let state = match self.tasks.get(task_id).await.map_err(store_error) {
                Ok(Some(task)) => Ok(task.status.state),
                Ok(None) => Err(task_not_found(task_id)),
                Err(error) => Err(error),
            };
            // The run that interrupted the task may not have ended yet.
            let earlier = match state {
                Ok(state) if is_interrupted(state) => {
                    let mut running = self.running.lock().unwrap();
                    let earlier = running.get(task_id).map(|earlier| earlier.done.clone());
                    if earlier.is_none()
                        && let Some(run) = run.take()
                    {
                        running.insert(task_id.to_string(), run);
                    }
                    Ok(earlier)
                }
                Ok(state) if is_terminal(state) => Err(Error::new(
                    code::UNSUPPORTED_OPERATION,
                    format!("task {task_id} is in a terminal state and accepts no messages"),
                )),
                Ok(_) => Err(Error::new(
                    code::UNSUPPORTED_OPERATION,
                    format!("task {task_id} is in progress and accepts no messages"),
                )),
                Err(error) => Err(error),
            };
            if subscribers.is_empty() {
                self.subscribers.lock().unwrap().remove(task_id);
            }
            drop(subscribers);
            match earlier {
                Ok(Some(mut done)) => {
                    let _ = done.wait_for(|done| *done).await;
                }
                Ok(None) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    // Applies an event to the run's task and publishes it. A status is given a
    // timestamp. The message of an interrupted or terminal status is added to
    // the task's history, and the message of any other status is not. An
    // event for another task, or one that A2A cannot represent, fails the
    // task and cancels the run.
    async fn apply_event(&self, task: &AcceptedTask, event: AgentEvent) {
        let event = match event {
            AgentEvent::Status(mut event) => {
                event.status = timestamped(event.status);
                AgentEvent::Status(event)
            }
            event => event,
        };
        let (task_id, context_id) = match &event {
            AgentEvent::Status(event) => (&event.task_id, &event.context_id),
            AgentEvent::Artifact(event) => (&event.task_id, &event.context_id),
        };
        let response = if (task_id, context_id) == (&task.task_id, &task.context_id) {
            a2a_event(event.clone())
        } else {
            Err("the agent sent an event for another task".to_string())
        };
        let mut subscribers = self.lock_subscribers(&task.task_id).await;
        let response = match response {
            Ok(response) => response,
            Err(reason) => {
                self.fail(&mut subscribers, task, reason).await;
                if let Some(cancel) = self
                    .running
                    .lock()
                    .unwrap()
                    .get_mut(&task.task_id)
                    .and_then(|run| run.cancel.take())
                {
                    let _ = cancel.send(());
                }
                return;
            }
        };
        if let AgentEvent::Status(event) = &event
            && (is_terminal(event.status.state) || is_interrupted(event.status.state))
            && let Some(message) = &event.status.message
        {
            logged(self.tasks.add_history(&task.task_id, message.clone()).await);
        }
        logged(self.tasks.apply(event).await);
        publish(&mut subscribers, response);
    }

    // Ends a run. Applies its outcome to its task and publishes the final
    // status, then removes the run and ends the streams that end with it.
    async fn end(&self, task: &AcceptedTask, outcome: anyhow::Result<Outcome>) {
        let mut subscribers = self.lock_subscribers(&task.task_id).await;
        self.apply_outcome(&mut subscribers, task, outcome).await;
        self.running.lock().unwrap().remove(&task.task_id);
        let terminal = self.is_terminal(&task.task_id).await;
        self.end_streams(&mut subscribers, &task.task_id, terminal);
    }

    // Applies a run's outcome to its task, including the final status and
    // assembled artifacts, and the invocation's exchanges added to the
    // context. Publishes the final status. A run that failed, or an outcome
    // that A2A cannot represent, fails the task.
    async fn apply_outcome(
        &self,
        subscribers: &mut Vec<Subscriber>,
        task: &AcceptedTask,
        outcome: anyhow::Result<Outcome>,
    ) {
        let response = match outcome {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return self.fail(subscribers, task, error.message).await,
            Err(error) => {
                let reason = format!("the agent failed: {error}");
                return self.fail(subscribers, task, reason).await;
            }
        };
        let AgentResponse {
            status,
            artifacts,
            exchanges,
            ..
        } = response;
        logged(self.exchanges.append(&task.context_id, exchanges).await);
        // An event may already have failed the task, and that status is
        // final.
        if self.is_terminal(&task.task_id).await {
            return;
        }

        let status = timestamped(status);
        let valid = a2a_status(status.clone()).and_then(|final_status| {
            artifacts
                .iter()
                .cloned()
                .try_for_each(|artifact| a2a_artifact(artifact).map(|_| ()))?;
            Ok(final_status)
        });
        let final_status = match valid {
            Ok(final_status) => final_status,
            Err(reason) => return self.fail(subscribers, task, reason).await,
        };
        if let Some(message) = &status.message {
            logged(self.tasks.add_history(&task.task_id, message.clone()).await);
        }
        logged(self.tasks.finish(&task.task_id, status, artifacts).await);
        publish(
            subscribers,
            a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
                task_id: task.task_id.clone(),
                context_id: task.context_id.clone(),
                status: final_status,
                metadata: None,
            }),
        );
    }

    // Whether the stored task with `task_id` is in a terminal state.
    async fn is_terminal(&self, task_id: &str) -> bool {
        matches!(
            self.tasks.get(task_id).await,
            Ok(Some(task)) if is_terminal(task.status.state)
        )
    }

    // Sets the task's status to `failed`, with `reason` as its message, and
    // publishes that status. Does nothing for a task in a terminal state.
    async fn fail(&self, subscribers: &mut Vec<Subscriber>, task: &AcceptedTask, reason: String) {
        if self.is_terminal(&task.task_id).await {
            return;
        }
        let (task_id, context_id) = (&task.task_id, &task.context_id);
        let message = Message {
            message_id: uuid::Uuid::new_v4().to_string(),
            context_id: Some(context_id.to_string()),
            task_id: Some(task_id.to_string()),
            role: Role::Agent,
            parts: vec![Part {
                content: PartContent::Text(reason),
                metadata: None,
                filename: None,
                media_type: None,
            }],
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        };
        logged(self.tasks.add_history(task_id, message.clone()).await);
        let status = timestamped(TaskStatus {
            state: TaskState::Failed,
            message: Some(message),
            timestamp: None,
        });
        let event = AgentEvent::Status(TaskStatusUpdateEvent {
            task_id: task_id.to_string(),
            context_id: context_id.to_string(),
            status,
            metadata: None,
        });
        let response = a2a_event(event.clone());
        logged(self.tasks.apply(event).await);
        if let Ok(response) = response {
            publish(subscribers, response);
        }
    }
}

// Sends `sender` the task, with at most `history_length` messages of its
// history, then adds it to `subscribers` with `scope`.
fn add_subscriber(
    subscribers: &mut Vec<Subscriber>,
    task: Task,
    sender: mpsc::Sender<a2a::StreamResponse>,
    scope: Scope,
    history_length: Option<usize>,
) -> Result<(), Error> {
    let task = a2a_task(task).map_err(|e| Error::new(code::INVALID_AGENT_RESPONSE, e))?;
    let task = trimmed(task, history_length);
    if sender.try_send(a2a::StreamResponse::Task(task)).is_ok() {
        subscribers.push(Subscriber { sender, scope });
    }
    Ok(())
}

// Sends `response` to each subscriber. Removes a subscriber that has stopped
// reading, or whose buffer is full.
fn publish(subscribers: &mut Vec<Subscriber>, response: a2a::StreamResponse) {
    subscribers.retain(|subscriber| subscriber.sender.try_send(response.clone()).is_ok());
}

// The task to store when the agent accepts a request. Its state is
// `submitted` and its history is the client's message.
fn new_task(task: &AcceptedTask, message: Message) -> Task {
    Task {
        id: task.task_id.clone(),
        context_id: Some(task.context_id.clone()),
        status: timestamped(TaskStatus {
            state: TaskState::Submitted,
            message: None,
            timestamp: None,
        }),
        artifacts: Vec::new(),
        history: vec![message],
        metadata: None,
    }
}

// Logs a store's failure during a run. No request is waiting for the run, so
// the failure cannot be returned as an error.
fn logged(result: StoreResult<()>) {
    if let Err(error) = result {
        tracing::warn!("a store failed during a run: {error:?}");
    }
}

/// An `agent-error` as a JSON-RPC error, with its code, message and data.
fn agent_error(error: AgentError) -> Error {
    match error.data.as_deref().map(serde_json::from_str).transpose() {
        Ok(data) => Error {
            code: error.code,
            message: error.message,
            data,
        },
        Err(_) => internal("the agent returned an error with data that is not valid JSON"),
    }
}

/// A store's failure as a JSON-RPC error.
pub(crate) fn store_error(error: StoreError) -> Error {
    match error {
        StoreError::InvalidPageToken => {
            Error::new(code::INVALID_PARAMS, "the page token is not valid")
        }
        StoreError::Failed(reason) => internal(format!("the store failed: {reason}")),
    }
}

pub(crate) fn task_not_found(id: &str) -> Error {
    Error::new(code::TASK_NOT_FOUND, format!("no task with id {id}"))
}

pub(crate) fn internal(message: impl Into<String>) -> Error {
    Error::new(code::INTERNAL_ERROR, message)
}
