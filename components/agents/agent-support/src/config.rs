//! An agent's config, read from `wasi:config/store`.

use std::str::FromStr;

/// The config key for the model's instructions.
pub const INSTRUCTIONS_CONFIG: &str = "instructions";

/// The config value for `key`, if set.
pub fn get_config(key: &str) -> Option<String> {
    crate::wasi::config::store::get(key).ok().flatten()
}

/// The config value for a required `key`.
pub fn required_config(key: &str) -> Result<String, String> {
    get_config(key).ok_or_else(|| format!("no '{key}' configured"))
}

/// `value` parsed as `T`, or `default` if it's `None`. `expected` describes a
/// valid value for the error, e.g. "true or false".
pub fn parse_config<T: FromStr>(
    key: &str,
    value: Option<String>,
    default: T,
    expected: &str,
) -> Result<T, String> {
    match value {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| format!("'{key}' must be {expected}, got '{value}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_value_parses_or_defaults() {
        assert_eq!(parse_config("limit", None, 8u32, "a number").unwrap(), 8);
        assert_eq!(
            parse_config("limit", Some("3".to_string()), 8u32, "a number").unwrap(),
            3
        );
        assert_eq!(
            parse_config("limit", Some("x".to_string()), 8u32, "a number").unwrap_err(),
            "'limit' must be a number, got 'x'"
        );
    }
}
