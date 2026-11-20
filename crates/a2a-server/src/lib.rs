//! An A2A server for agents that export `composable:agents/agent`.
//!
//! [`RequestHandler`] has A2A's operations for one agent, independent of any
//! protocol binding. It converts between A2A's JSON types (`a2a-json`) and
//! the agent interface's types, which mirror A2A's.
//!
//! The server keeps its tasks in a [`TaskStore`] and each context's model
//! exchanges in an [`ExchangeStore`]. By default both are in memory.

mod convert;
mod handler;
mod memory;
mod runner;
mod store;

pub use handler::{CallContext, RequestHandler};
pub use memory::{
    DEFAULT_CONTEXT_CAPACITY, DEFAULT_TASK_CAPACITY, InMemoryExchangeStore, InMemoryTaskStore,
};
pub use store::{
    ExchangeStore, StoreError, StoreResult, TaskPage, TaskQuery, TaskStore, is_interrupted,
    is_terminal,
};
