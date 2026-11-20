//! A2A's operations for one agent, independent of any protocol binding.

use std::sync::Arc;

use a2a_json as a2a;
use a2a_json::jsonrpc::{Error, code};
use composable_agents::types::{
    AgentEvent, AgentRequest, Task, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use composable_agents::{AcceptedTask, AgentHost};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;

use crate::convert::{a2a_task, agent_message, agent_state, json_text, timestamped, trimmed};
use crate::runner::{Runner, SUBSCRIBER_BUFFER, internal, store_error};
use crate::store::{ExchangeStore, TaskQuery, TaskStore, is_interrupted, is_terminal};

// The page size of `ListTasks` when the request does not specify one, and the
// maximum page size a request may specify.
const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: i32 = 100;

/// A call's service parameters, which a binding reads from the request and
/// writes to the response.
#[derive(Debug, Clone, Default)]
pub struct CallContext {
    /// The extension URIs the client requested.
    pub requested_extensions: Vec<String>,
    /// The requested extensions that the agent activated, set by the
    /// operation.
    pub activated_extensions: Vec<String>,
}

/// Handles A2A's operations for one agent. Each method is one operation.
pub struct RequestHandler<T, E> {
    runner: Arc<Runner<T, E>>,
}

impl<T: TaskStore, E: ExchangeStore> RequestHandler<T, E> {
    /// A handler for `agent`, one of `host`'s agents, with its stores.
    pub fn new(host: Arc<AgentHost>, agent: impl Into<String>, tasks: T, exchanges: E) -> Self {
        RequestHandler {
            runner: Arc::new(Runner::new(host, agent.into(), tasks, exchanges)),
        }
    }

    /// Starts a run of the agent with the message, and returns the task. It
    /// returns once the task is terminal or interrupted, or with
    /// `returnImmediately` as soon as the agent has accepted the message.
    pub async fn send_message(
        &self,
        request: a2a::SendMessageRequest,
        context: &mut CallContext,
    ) -> Result<a2a::SendMessageResponse, Error> {
        let configuration = request.configuration.as_ref();
        let return_immediately = configuration.is_some_and(|c| c.return_immediately);
        let history_length = history_length(configuration.and_then(|c| c.history_length))?;
        let accepted = self.start(request, context, None, history_length).await?;
        if !return_immediately {
            self.runner.wait(&accepted.task_id).await;
        }
        let task = self
            .stored_task(&accepted.task_id)
            .await?
            .ok_or_else(|| internal("the task is no longer stored"))?;
        Ok(a2a::SendMessageResponse::Task(a2a_json_task(
            task,
            history_length,
        )?))
    }

    /// Starts a run of the agent with the message, and returns the stream of
    /// its responses. The stream begins with the task, then has each status
    /// and artifact update, and ends when the task is terminal or
    /// interrupted. `returnImmediately` has no effect.
    pub async fn send_streaming_message(
        &self,
        request: a2a::SendMessageRequest,
        context: &mut CallContext,
    ) -> Result<mpsc::Receiver<a2a::StreamResponse>, Error> {
        let configuration = request.configuration.as_ref();
        let history_length = history_length(configuration.and_then(|c| c.history_length))?;
        let (subscriber, responses) = mpsc::channel(SUBSCRIBER_BUFFER);
        self.start(request, context, Some(subscriber), history_length)
            .await?;
        Ok(responses)
    }

    /// Returns the stream of responses for a task that is not terminal. The
    /// stream begins with the task as stored. For a task with a run in
    /// progress, it then has each later status and artifact update, and ends
    /// when the task is terminal or interrupted. An interrupted task has no
    /// run, so its stream ends after the task.
    pub async fn subscribe_to_task(
        &self,
        request: a2a::SubscribeToTaskRequest,
        _context: &CallContext,
    ) -> Result<mpsc::Receiver<a2a::StreamResponse>, Error> {
        let task = self
            .stored_task(&request.id)
            .await?
            .ok_or_else(|| task_not_found(&request.id))?;
        if is_terminal(task.status.state) {
            return Err(Error::new(
                code::UNSUPPORTED_OPERATION,
                format!("task {} is in a terminal state", request.id),
            ));
        }
        let (subscriber, responses) = mpsc::channel(SUBSCRIBER_BUFFER);
        if !self
            .runner
            .subscribe(&request.id, subscriber.clone())
            .await?
        {
            let task = self
                .stored_task(&request.id)
                .await?
                .ok_or_else(|| task_not_found(&request.id))?;
            let task = a2a_json_task(task, None)?;
            let _ = subscriber.try_send(a2a::StreamResponse::Task(task));
        }
        Ok(responses)
    }

    // Starts a run of the agent with the request's message, and sets the
    // context's activated extensions. A `subscriber` is sent the task, with
    // at most `history_length` messages of its history, and then the run's
    // responses.
    async fn start(
        &self,
        request: a2a::SendMessageRequest,
        context: &mut CallContext,
        subscriber: Option<mpsc::Sender<a2a::StreamResponse>>,
        history_length: Option<usize>,
    ) -> Result<AcceptedTask, Error> {
        let configuration = request.configuration.unwrap_or_default();
        if configuration.task_push_notification_config.is_some() {
            return Err(push_notifications_not_supported());
        }

        let continued = self.continued_task(&request.message).await?;
        let context_id = match &continued {
            Some(task) => task.context_id.clone(),
            None => request.message.context_id.clone(),
        };
        let history = match &context_id {
            Some(context_id) => self
                .runner
                .exchanges
                .history(context_id)
                .await
                .map_err(store_error)?,
            None => Vec::new(),
        };
        // The tasks the message references, leaving out any the store
        // does not have.
        let mut related_tasks = Vec::new();
        for id in &request.message.reference_task_ids {
            related_tasks.extend(self.stored_task(id).await?);
        }
        let agent_request = AgentRequest {
            message: agent_message(request.message),
            task_id: continued.as_ref().map(|task| task.id.clone()),
            context_id,
            accepted_output_modes: configuration.accepted_output_modes,
            requested_extensions: context.requested_extensions.clone(),
            metadata: json_text(request.metadata),
            task_status: continued.as_ref().map(|task| task.status.clone()),
            related_tasks,
            history,
        };

        let accepted = self
            .runner
            .start(
                agent_request,
                continued.is_some(),
                subscriber,
                history_length,
            )
            .await?;
        context.activated_extensions = accepted.activated_extensions.clone();
        Ok(accepted)
    }

    pub async fn get_task(
        &self,
        request: a2a::GetTaskRequest,
        _context: &CallContext,
    ) -> Result<a2a::Task, Error> {
        let history_length = history_length(request.history_length)?;
        let task = self
            .stored_task(&request.id)
            .await?
            .ok_or_else(|| task_not_found(&request.id))?;
        a2a_json_task(task, history_length)
    }

    pub async fn list_tasks(
        &self,
        request: a2a::ListTasksRequest,
        _context: &CallContext,
    ) -> Result<a2a::ListTasksResponse, Error> {
        let page_size = match request.page_size {
            None => DEFAULT_PAGE_SIZE,
            Some(size @ 1..=MAX_PAGE_SIZE) => size as u32,
            Some(_) => {
                return Err(Error::new(
                    code::INVALID_PARAMS,
                    format!("pageSize must be from 1 to {MAX_PAGE_SIZE}"),
                ));
            }
        };
        let history_length = history_length(request.history_length)?;
        if let Some(timestamp) = &request.status_timestamp_after
            && OffsetDateTime::parse(timestamp, &Rfc3339).is_err()
        {
            return Err(Error::new(
                code::INVALID_PARAMS,
                "statusTimestampAfter is not an RFC 3339 timestamp",
            ));
        }
        let query = TaskQuery {
            context_id: request.context_id,
            // An unspecified status is no filter.
            state: request
                .status
                .filter(|status| *status != a2a::TaskState::Unspecified)
                .map(agent_state),
            status_timestamp_after: request.status_timestamp_after,
            page_size,
            page_token: request.page_token.filter(|token| !token.is_empty()),
        };
        let page = self.runner.tasks.list(query).await.map_err(store_error)?;

        let include_artifacts = request.include_artifacts.unwrap_or(false);
        let mut tasks = Vec::with_capacity(page.tasks.len());
        for task in page.tasks {
            let mut task = a2a_json_task(task, history_length)?;
            if !include_artifacts {
                task.artifacts.clear();
            }
            tasks.push(task);
        }
        Ok(a2a::ListTasksResponse {
            tasks,
            next_page_token: page.next_page_token.unwrap_or_default(),
            page_size: page_size as i32,
            total_size: page.total_size as i32,
        })
    }

    /// Cancels the task and returns it once it is canceled. A task with a run
    /// in progress is canceled by its agent. An interrupted task has no run,
    /// so its status is set to `canceled` here.
    pub async fn cancel_task(
        &self,
        request: a2a::CancelTaskRequest,
        _context: &CallContext,
    ) -> Result<a2a::Task, Error> {
        let task = self
            .stored_task(&request.id)
            .await?
            .ok_or_else(|| task_not_found(&request.id))?;
        if is_terminal(task.status.state) {
            return Err(Error::new(
                code::TASK_NOT_CANCELABLE,
                format!("task {} is in a terminal state", request.id),
            ));
        }
        if !self.runner.cancel(&request.id).await {
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
            self.runner.tasks.apply(event).await.map_err(store_error)?;
        }
        let task = self
            .stored_task(&request.id)
            .await?
            .ok_or_else(|| task_not_found(&request.id))?;
        a2a_json_task(task, None)
    }

    /// Not supported.
    pub async fn create_task_push_notification_config(
        &self,
        _request: a2a::TaskPushNotificationConfig,
        _context: &CallContext,
    ) -> Result<a2a::TaskPushNotificationConfig, Error> {
        Err(push_notifications_not_supported())
    }

    /// Not supported.
    pub async fn get_task_push_notification_config(
        &self,
        _request: a2a::GetTaskPushNotificationConfigRequest,
        _context: &CallContext,
    ) -> Result<a2a::TaskPushNotificationConfig, Error> {
        Err(push_notifications_not_supported())
    }

    /// Not supported.
    pub async fn list_task_push_notification_configs(
        &self,
        _request: a2a::ListTaskPushNotificationConfigsRequest,
        _context: &CallContext,
    ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, Error> {
        Err(push_notifications_not_supported())
    }

    /// Not supported.
    pub async fn delete_task_push_notification_config(
        &self,
        _request: a2a::DeleteTaskPushNotificationConfigRequest,
        _context: &CallContext,
    ) -> Result<(), Error> {
        Err(push_notifications_not_supported())
    }

    /// Not supported.
    pub async fn get_extended_agent_card(
        &self,
        _request: a2a::GetExtendedAgentCardRequest,
        _context: &CallContext,
    ) -> Result<a2a::AgentCard, Error> {
        Err(Error::new(
            code::UNSUPPORTED_OPERATION,
            "this agent has no extended agent card",
        ))
    }

    // The interrupted task that `message` continues, or `None` when the
    // message names no task. Fails when the task it names is not stored, is
    // in another context, or is not interrupted.
    async fn continued_task(&self, message: &a2a::Message) -> Result<Option<Task>, Error> {
        let Some(task_id) = &message.task_id else {
            return Ok(None);
        };
        let task = self
            .stored_task(task_id)
            .await?
            .ok_or_else(|| task_not_found(task_id))?;
        if let Some(context_id) = &message.context_id
            && task.context_id.as_ref() != Some(context_id)
        {
            return Err(Error::new(
                code::INVALID_PARAMS,
                format!("the message's contextId is not the context of task {task_id}"),
            ));
        }
        let state = task.status.state;
        if is_terminal(state) {
            return Err(Error::new(
                code::UNSUPPORTED_OPERATION,
                format!("task {task_id} is in a terminal state and accepts no messages"),
            ));
        }
        if !is_interrupted(state) {
            return Err(Error::new(
                code::UNSUPPORTED_OPERATION,
                format!("task {task_id} is in progress and accepts no messages"),
            ));
        }
        Ok(Some(task))
    }

    async fn stored_task(&self, id: &str) -> Result<Option<Task>, Error> {
        self.runner.tasks.get(id).await.map_err(store_error)
    }
}

// A request's `historyLength`. Fails on a negative value.
fn history_length(value: Option<i32>) -> Result<Option<usize>, Error> {
    value
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| Error::new(code::INVALID_PARAMS, "historyLength must not be negative"))
        })
        .transpose()
}

// A stored task as A2A's JSON, with at most `history_length` messages of its
// history.
fn a2a_json_task(task: Task, history_length: Option<usize>) -> Result<a2a::Task, Error> {
    let task = a2a_task(task).map_err(|e| Error::new(code::INVALID_AGENT_RESPONSE, e))?;
    Ok(trimmed(task, history_length))
}

fn task_not_found(id: &str) -> Error {
    Error::new(code::TASK_NOT_FOUND, format!("no task with id {id}"))
}

fn push_notifications_not_supported() -> Error {
    Error::new(
        code::PUSH_NOTIFICATION_NOT_SUPPORTED,
        "this agent does not support push notifications",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negative_history_length_is_not_valid() {
        assert_eq!(history_length(None), Ok(None));
        assert_eq!(history_length(Some(0)), Ok(Some(0)));
        let error = history_length(Some(-1)).unwrap_err();
        assert_eq!(error.code, code::INVALID_PARAMS);
    }
}
