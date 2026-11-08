//! Shared code for `composable:agents/model-client` implementations: the
//! session, an SSE parser, tool-name mapping, and history checks.
//!
//! A client reuses this crate's bindings with `wit_bindgen::generate!`'s
//! `with` option:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     path: "wit",
//!     world: "my-client",
//!     with: {
//!         "wasi:config/store@0.2.0-rc.1": model_client_support::config_store,
//!         "componentized:http/client@0.1.0": model_client_support::http_client,
//!         "composable:agents/types@0.1.0": model_client_support::types,
//!     },
//!     generate_all,
//! });
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "model-client-support",
    generate_all,
    // Every count `none`: no counts reported.
    additional_type_attributes: {
        "composable:agents/types@0.1.0/token-usage": [#[derive(Default)]],
    },
});

mod config;
mod history;
mod http;
mod json;
mod session;
pub mod sse;
mod tool_names;

pub use componentized::http::client as http_client;
pub use composable::agents::types;
pub use wasi::config::store as config_store;

pub use config::{CommonConfig, get_config, parsed_config, required_config};
pub use history::{
    history_input_note, media_detail, output_media_note, require_answered_tool_calls,
    tool_result_media_note, unsendable_input_error,
};
pub use http::send_request;
pub use json::json_error_position;
pub use session::{Client, Deltas, Outcome, Session};
