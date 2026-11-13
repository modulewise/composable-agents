//! A client's config, read from `wasi:config/store`.

use std::str::FromStr;

use url::{Host, ParseError, Url};

use crate::composable::models::types::{ModelError, Server, TokenUsage};

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
    pub base_url: Url,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
}

impl CommonConfig {
    /// Reads the config once per session. A missing or invalid value fails the
    /// session's creation.
    pub fn load(default_base_url: &str) -> Result<Self, ModelError> {
        let base_url = get_config(BASE_URL_CONFIG).unwrap_or_else(|| default_base_url.to_string());
        Ok(CommonConfig {
            model: required_config(MODEL_CONFIG)?,
            provider: required_config(PROVIDER_CONFIG)?,
            base_url: Url::parse(&base_url)
                .map_err(|e| base_url_error("is not a valid URL", Some(e)))?,
            api_key: get_config(API_KEY_CONFIG),
            max_tokens: parsed_config(MAX_TOKENS_CONFIG, "a number")?,
        })
    }

    /// The URL of the endpoint at `path` under the base URL.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base_url.as_str().trim_end_matches('/'))
    }

    /// The base URL's host, and its port or the scheme's default. Fails when
    /// the URL has no host or no known port.
    pub fn server(&self) -> Result<Server, ModelError> {
        let address = match self.base_url.host() {
            Some(Host::Domain(domain)) => domain.to_string(),
            Some(Host::Ipv4(ip)) => ip.to_string(),
            Some(Host::Ipv6(ip)) => ip.to_string(),
            None => return Err(base_url_error("has no host", None)),
        };
        let port = self
            .base_url
            .port_or_known_default()
            .ok_or_else(|| base_url_error("has no port", None))?;
        Ok(Server { address, port })
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

// An error about `base-url`. Its message has only `reason` and the parse
// error's description.
fn base_url_error(reason: &'static str, cause: Option<ParseError>) -> ModelError {
    let message = match cause {
        Some(cause) => format!("'{BASE_URL_CONFIG}' {reason}: {cause}"),
        None => format!("'{BASE_URL_CONFIG}' {reason}"),
    };
    ModelError {
        message,
        status: None,
        usage: TokenUsage::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base_url: &str) -> CommonConfig {
        CommonConfig {
            model: "model".to_string(),
            provider: "provider".to_string(),
            base_url: Url::parse(base_url).unwrap(),
            api_key: None,
            max_tokens: None,
        }
    }

    #[test]
    fn the_server_is_the_host_and_the_port_or_the_schemes_default() {
        for (base_url, address, port) in [
            ("https://api.openai.com/v1", "api.openai.com", 443),
            ("http://localhost:11434/v1", "localhost", 11434),
            ("http://127.0.0.1/v1", "127.0.0.1", 80),
            ("http://[::1]:8080", "::1", 8080),
        ] {
            let server = config(base_url).server().unwrap();
            assert_eq!(
                (server.address.as_str(), server.port),
                (address, port),
                "{base_url}"
            );
        }
    }

    #[test]
    fn a_base_url_without_a_host_has_no_server() {
        let error = config("unix:/tmp/socket").server().err().unwrap();
        assert_eq!(error.message, "'base-url' has no host");
    }

    #[test]
    fn the_endpoint_is_the_path_under_the_base_url() {
        for base_url in ["https://api.openai.com/v1", "https://api.openai.com/v1/"] {
            assert_eq!(
                config(base_url).endpoint("/chat/completions"),
                "https://api.openai.com/v1/chat/completions"
            );
        }
        assert_eq!(
            config("http://localhost:11434").endpoint("/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
    }
}
