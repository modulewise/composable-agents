//! The stores for a server's tasks and for each context's model exchanges.
//! Each is a trait, so that a server can be given an implementation. Their
//! methods take and return values of the agent interface's types, as a WIT
//! interface would, and are async because an implementation may call a
//! component or a service.

use std::future::Future;

use composable_agents::model_types::ModelExchange;
use composable_agents::types::{AgentEvent, Artifact, Message, Task, TaskState, TaskStatus};

/// Why a store's operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A page token that the store did not return, or can no longer use.
    InvalidPageToken,
    /// The store could not complete the operation.
    Failed(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Which tasks to list, and which page of them.
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    /// Only tasks in this context.
    pub context_id: Option<String>,
    /// Only tasks in this state.
    pub state: Option<TaskState>,
    /// Only tasks whose status is as recent as this RFC 3339 timestamp.
    pub status_timestamp_after: Option<String>,
    /// The most tasks to return.
    pub page_size: u32,
    /// The `next_page_token` of the page before the one to return, or `None`
    /// for the first page.
    pub page_token: Option<String>,
}

/// One page of the tasks a query matches.
pub struct TaskPage {
    pub tasks: Vec<Task>,
    /// The token for the next page, or `None` when this is the last.
    pub next_page_token: Option<String>,
    /// The number of tasks the query matches, in all pages.
    pub total_size: u32,
}

/// The tasks a server has accepted.
pub trait TaskStore: Send + Sync + 'static {
    /// Adds a task, replacing any task with the same id.
    fn insert(&self, task: Task) -> impl Future<Output = StoreResult<()>> + Send;

    /// The task with `id`, if the store has it.
    fn get(&self, id: &str) -> impl Future<Output = StoreResult<Option<Task>>> + Send;

    /// Applies an event to its task. A status replaces the task's status. An
    /// artifact is added to the task's artifacts, replacing one with the same
    /// id, or with `append` its parts are added to that one. Does nothing for
    /// a task the store doesn't have, or one in a terminal state.
    fn apply(&self, event: AgentEvent) -> impl Future<Output = StoreResult<()>> + Send;

    /// Adds a message to the history of the task with `id`.
    fn add_history(
        &self,
        id: &str,
        message: Message,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Sets the final status of the task with `id`, and its artifacts as the
    /// agent assembled them. Does nothing for a task in a terminal state.
    fn finish(
        &self,
        id: &str,
        status: TaskStatus,
        artifacts: Vec<Artifact>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// One page of the tasks `query` matches, ordered by the time of each
    /// task's status, the most recent first.
    fn list(&self, query: TaskQuery) -> impl Future<Output = StoreResult<TaskPage>> + Send;
}

/// The model exchanges of each context. An invocation in a context is given
/// them as its history, and adds its own when it ends.
pub trait ExchangeStore: Send + Sync + 'static {
    /// The exchanges of the context with `context_id`, oldest first. Empty
    /// for a context the store doesn't have.
    fn history(
        &self,
        context_id: &str,
    ) -> impl Future<Output = StoreResult<Vec<ModelExchange>>> + Send;

    /// Adds an invocation's exchanges to the context with `context_id`.
    fn append(
        &self,
        context_id: &str,
        exchanges: Vec<ModelExchange>,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}

/// Whether `state` is terminal: `completed`, `failed`, `canceled`, or
/// `rejected`.
pub fn is_terminal(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
}

/// Whether `state` is interrupted: `input-required` or `auth-required`.
pub fn is_interrupted(state: TaskState) -> bool {
    matches!(state, TaskState::InputRequired | TaskState::AuthRequired)
}
