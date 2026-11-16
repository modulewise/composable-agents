//! The agent's errors, with their JSON-RPC codes.

use crate::composable::agents::types::AgentError;
use crate::composable::models::types::SessionError;

/// The JSON-RPC error code for a request the agent cannot accept.
pub const INVALID_PARAMS: i32 = -32602;

/// The JSON-RPC error code for a failure of the agent itself.
pub const INTERNAL_ERROR: i32 = -32603;

/// The agent's error for a model session that could not be created. History
/// the caller sent is an invalid request, and the agent's settings or the
/// model's config are a failure of the agent itself.
pub fn session_error(error: SessionError) -> AgentError {
    let (code, message) = match error {
        SessionError::History(error) => (
            INVALID_PARAMS,
            format!("invalid history: {}", error.message),
        ),
        SessionError::Settings(error) | SessionError::Config(error) => (
            INTERNAL_ERROR,
            format!("the model session could not be created: {}", error.message),
        ),
    };
    AgentError {
        code,
        message,
        data: None,
    }
}
