//! Agents as components, for any host: a CLI, an A2A server, or a custom agent
//! runtime. `AgentService` contributes the `[model]` and `[agent]` config
//! categories, which expand into the components that implement them, and
//! `AgentHost` lists and invokes the agent components of a host.
//! `usage_by_model` sums an invocation's token usage per provider and model.

mod agents;
mod bindings;
mod config;
mod service;
mod usage;

pub use agents::{AGENT_EXPORT, AGENT_METADATA, AgentHost, Outcome, exports_agent};
pub use bindings::composable::agents::types;
pub use bindings::composable::models::types as model_types;
pub use service::AgentService;
pub use usage::{ModelUsage, usage_by_model};
