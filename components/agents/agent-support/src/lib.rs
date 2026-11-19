//! Shared code for `composable:agents/agent` implementations. Includes the
//! agent card, task events and messages, the text artifact, reasoning
//! messages, and the mapping of message parts to model input.
//!
//! An agent reuses this crate's bindings with `wit_bindgen::generate!`'s
//! `with` option:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     path: "wit",
//!     world: "my-agent",
//!     with: {
//!         "wasi:config/store@0.2.0-rc.1": agent_support::config_store,
//!         "wasi:random/random@0.3.0": agent_support::random,
//!         "composable:agents/types@0.1.0": agent_support::types,
//!         "composable:models/types@0.1.0": agent_support::model_types,
//!     },
//!     generate_all,
//! });
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "agent-support",
    generate_all,
    additional_type_attributes: {
        "composable:models/types@0.1.0/token-usage": [#[derive(Default)]],
    },
});

mod artifact;
mod cancel;
mod card;
mod config;
mod error;
mod events;
mod input;
mod reasoning;

pub use composable::agents::types;
pub use composable::models::types as model_types;
pub use wasi::config::store as config_store;
pub use wasi::random::random;

pub use artifact::TextArtifact;
pub use cancel::Cancel;
pub use card::agent_metadata;
pub use config::{INSTRUCTIONS_CONFIG, get_config, parse_config, required_config};
pub use error::{INTERNAL_ERROR, INVALID_PARAMS, session_error};
pub use events::{Events, TaskIds, agent_message, new_id, status_event, text_part};
pub use input::{OCTET_STREAM, model_input, response_text};
pub use reasoning::{REPORT_REASONING_CONFIG, ReasoningRun};
