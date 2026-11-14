use crate::composable::models::types::ModelError;

impl ModelError {
    /// An error with `message`, and no code or status.
    pub fn new(message: impl Into<String>) -> Self {
        ModelError {
            message: message.into(),
            code: None,
            status: None,
        }
    }
}
