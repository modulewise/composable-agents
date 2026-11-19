//! An A2A server for agents that export `composable:agents/agent`. It
//! implements A2A's JSON-RPC binding, converting between A2A's JSON
//! (`a2a-json`) and the agent interface's types, which mirror A2A's.
//!
//! The server keeps its tasks in a [`TaskStore`] and each context's model
//! exchanges in an [`ExchangeStore`]. By default both are in memory.

mod convert;
mod memory;
mod store;

pub use memory::{
    DEFAULT_CONTEXT_CAPACITY, DEFAULT_TASK_CAPACITY, InMemoryExchangeStore, InMemoryTaskStore,
};
pub use store::{
    ExchangeStore, StoreError, StoreResult, TaskPage, TaskQuery, TaskStore, is_interrupted,
    is_terminal,
};
