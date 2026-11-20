//! Runs of the agent. Each message the agent accepts runs as its own spawned
//! task, which applies the agent's events and outcome to the stores. A
//! request observes a run, by waiting for it to end or by canceling it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use a2a_json::jsonrpc::{Error, code};
use composable_agents::types::{
    AgentError, AgentEvent, AgentRequest, AgentResponse, Message, Part, PartContent, Role, Task,
    TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use composable_agents::{AcceptedTask, AgentHost, Outcome};
use tokio::sync::{mpsc, oneshot, watch};

use crate::convert::{a2a_artifact, a2a_event, a2a_status, timestamped};
use crate::store::{
    ExchangeStore, StoreError, StoreResult, TaskStore, is_interrupted, is_terminal,
};

/// Runs one agent, with the stores its runs update.
pub(crate) struct Runner<T, E> {
    host: Arc<AgentHost>,
    agent: String,
    pub(crate) tasks: T,
    pub(crate) exchanges: E,
    // The runs in progress, by task id.
    running: Mutex<HashMap<String, Run>>,
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
        }
    }

    /// Starts a run of the agent with `request`, and returns once the agent
    /// accepts the request as a task. The task is then in the store either as
    /// a new task, or with `continued` the task the request names, with the
    /// request's message added to its history. Fails with the agent's error
    /// when it does not accept the request, and then no task is stored.
    pub(crate) async fn start(
        self: &Arc<Self>,
        request: AgentRequest,
        continued: bool,
    ) -> Result<AcceptedTask, Error> {
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let runner = self.clone();
        tokio::spawn(async move { runner.run(request, continued, accepted_tx).await });
        accepted_rx
            .await
            .unwrap_or_else(|_| Err(internal("the agent's run stopped before it accepted")))
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

    /// Cancels the run of the task with `task_id` and waits until it ends.
    /// Returns `false` when no run of that task is in progress.
    pub(crate) async fn cancel(&self, task_id: &str) -> bool {
        let cancel = match self.running.lock().unwrap().get_mut(task_id) {
            Some(run) => run.cancel.take(),
            None => return false,
        };
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
        self.wait(task_id).await;
        true
    }

    // Invokes the agent, then applies the task it accepts, its events and its
    // outcome to the stores. `accepted_tx` is given the accepted task, or the
    // error when the agent accepts none.
    async fn run(
        self: Arc<Self>,
        request: AgentRequest,
        continued: bool,
        accepted_tx: oneshot::Sender<Result<AcceptedTask, Error>>,
    ) {
        let message = request.message.clone();
        // Dropping the sender would cancel the task, so it is kept until the
        // run ends, in `apply` first and then in the run's entry in `running`.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let (done_tx, done_rx) = watch::channel(false);
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
            self.running.lock().unwrap().insert(
                task.task_id.clone(),
                Run {
                    cancel: Some(cancel_tx),
                    done: done_rx,
                },
            );
            let _ = accepted_tx.send(stored.map(|()| task.clone()).map_err(store_error));
            while let Some(event) = events_rx.recv().await {
                self.apply_event(event).await;
            }
            Ok(task)
        };
        let (outcome, applied) = tokio::join!(invoke, apply);

        match applied {
            Ok(task) => {
                self.end(&task, outcome).await;
                self.running.lock().unwrap().remove(&task.task_id);
            }
            Err((accepted_tx, _cancel_tx)) => {
                let error = match outcome {
                    Ok(Err(error)) => agent_error(error),
                    Ok(Ok(_)) => internal("the agent returned an outcome for no task"),
                    Err(error) => internal(format!("the agent failed: {error}")),
                };
                let _ = accepted_tx.send(Err(error));
            }
        }
        let _ = done_tx.send(true);
    }

    // Applies an event to its task. A status is given a timestamp. The message
    // of an interrupted or terminal status is added to the task's history,
    // and the message of any other status is not. An event that A2A cannot
    // represent fails the task and cancels the run.
    async fn apply_event(&self, event: AgentEvent) {
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
        if let Err(reason) = a2a_event(event.clone()) {
            self.fail(task_id, context_id, reason).await;
            if let Some(cancel) = self
                .running
                .lock()
                .unwrap()
                .get_mut(task_id)
                .and_then(|run| run.cancel.take())
            {
                let _ = cancel.send(());
            }
            return;
        }
        if let AgentEvent::Status(event) = &event
            && (is_terminal(event.status.state) || is_interrupted(event.status.state))
            && let Some(message) = &event.status.message
        {
            logged(self.tasks.add_history(task_id, message.clone()).await);
        }
        logged(self.tasks.apply(event).await);
    }

    // Applies a run's outcome to its task, including the final status and
    // assembled artifacts, and the invocation's exchanges added to the
    // context. A run that failed, or an outcome that A2A cannot represent,
    // fails the task.
    async fn end(&self, task: &AcceptedTask, outcome: anyhow::Result<Outcome>) {
        let response = match outcome {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return self
                    .fail(&task.task_id, &task.context_id, error.message)
                    .await;
            }
            Err(error) => {
                let reason = format!("the agent failed: {error}");
                return self.fail(&task.task_id, &task.context_id, reason).await;
            }
        };
        let AgentResponse {
            status,
            artifacts,
            exchanges,
            ..
        } = response;
        logged(self.exchanges.append(&task.context_id, exchanges).await);

        let status = timestamped(status);
        let valid = a2a_status(status.clone()).and_then(|_| {
            artifacts
                .iter()
                .cloned()
                .try_for_each(|artifact| a2a_artifact(artifact).map(|_| ()))
        });
        if let Err(reason) = valid {
            return self.fail(&task.task_id, &task.context_id, reason).await;
        }
        if let Some(message) = &status.message {
            logged(self.tasks.add_history(&task.task_id, message.clone()).await);
        }
        logged(self.tasks.finish(&task.task_id, status, artifacts).await);
    }

    // Sets the task's status to `failed`, with `reason` as its message.
    async fn fail(&self, task_id: &str, context_id: &str, reason: String) {
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
        logged(self.tasks.apply(event).await);
    }
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

pub(crate) fn internal(message: impl Into<String>) -> Error {
    Error::new(code::INTERNAL_ERROR, message)
}
