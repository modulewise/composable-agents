//! An A2A server for agents that export `composable:agents/agent`.
//!
//! [`RequestHandler`] has A2A's operations for one agent, independent of any
//! protocol binding. It converts between A2A's JSON types (`a2a-json`) and
//! the agent interface's types, which mirror A2A's.
//!
//! [`A2aServer`] serves one agent over HTTP, with its card and A2A's JSON-RPC
//! binding. [`A2aService`] is the runtime service that starts a server for
//! each `[server.*]` definition where `type = "a2a"`.
//!
//! The server keeps its tasks in a [`TaskStore`] and each context's model
//! exchanges in an [`ExchangeStore`]. By default both are in memory.

mod card;
mod config;
mod convert;
mod handler;
mod jsonrpc;
mod memory;
mod runner;
mod server;
mod service;
mod store;

pub use config::A2aServerConfig;
pub use handler::{CallContext, RequestHandler};
pub use memory::{
    DEFAULT_CONTEXT_CAPACITY, DEFAULT_TASK_CAPACITY, InMemoryExchangeStore, InMemoryTaskStore,
};
pub use server::{A2aServer, AGENT_CARD_PATH};
pub use service::A2aService;
pub use store::{
    ExchangeStore, StoreError, StoreResult, TaskPage, TaskQuery, TaskStore, is_interrupted,
    is_terminal,
};
