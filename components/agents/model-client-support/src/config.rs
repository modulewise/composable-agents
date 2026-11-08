//! A client's config, read from `wasi:config/store`.

use std::str::FromStr;

use crate::composable::agents::types::{ModelError, TokenUsage};

const API_KEY_CONFIG: &str = "api-key";
const BASE_URL_CONFIG: &str = "base-url";
const MAX_TOKENS_CONFIG: &str = "max-tokens";
const MODEL_CONFIG: &str = "model";
const PROVIDER_CONFIG: &str = "provider";

/// The config every client reads: model, provider, base URL, API key and
/// default `max-tokens`.
pub struct CommonConfig {
    pub model: String,
    pub provider: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
}

impl CommonConfig {
    /// Reads the config once per session. A missing or invalid value fails the
    /// session's creation.
    pub fn load(default_base_url: &str) -> Result<Self, ModelError> {
        Ok(CommonConfig {
            model: required_config(MODEL_CONFIG)?,
            provider: required_config(PROVIDER_CONFIG)?,
            base_url: get_config(BASE_URL_CONFIG).unwrap_or_else(|| default_base_url.to_string()),
            api_key: get_config(API_KEY_CONFIG),
            max_tokens: parsed_config(MAX_TOKENS_CONFIG, "a number")?,
        })
    }

    /// The URL of the endpoint at `path` under the base URL.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base_url.trim_end_matches('/'))
    }
}

/// The config value for `key`, if set.
pub fn get_config(key: &str) -> Option<String> {
    crate::wasi::config::store::get(key).ok().flatten()
}

/// The config value for a required `key`.
pub fn required_config(key: &str) -> Result<String, ModelError> {
    get_config(key).ok_or_else(|| ModelError {
        message: format!("no '{key}' configured"),
        status: None,
        usage: TokenUsage::default(),
    })
}

/// The config value for `key` parsed as `T`, if set. `expected` describes a
/// valid value for the error, e.g. "a number".
pub fn parsed_config<T: FromStr>(key: &str, expected: &str) -> Result<Option<T>, ModelError> {
    get_config(key)
        .map(|value| {
            value.parse().map_err(|_| ModelError {
                message: format!("'{key}' must be {expected}, got '{value}'"),
                status: None,
                usage: TokenUsage::default(),
            })
        })
        .transpose()
}
