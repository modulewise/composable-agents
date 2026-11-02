use composable_runtime::{ConfigHandler, Service};

use crate::config::AgentConfigHandler;

/// Contributes the `[model]` and `[agent]` config handlers to any host: a CLI,
/// an A2A server, or a custom agent runtime.
#[derive(Default)]
pub struct AgentService;

impl Service for AgentService {
    fn config_handler(&self) -> Option<Box<dyn ConfigHandler>> {
        Some(Box::new(AgentConfigHandler::default()))
    }
}
