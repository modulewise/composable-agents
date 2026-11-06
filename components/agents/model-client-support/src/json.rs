//! Describes where a provider's JSON failed to parse, without showing the JSON.

/// Where JSON failed to parse, as "line L, column C", or "" without a
/// position. serde's message isn't used because it can quote the value.
pub fn json_error_position(error: &serde_json::Error) -> String {
    if error.line() == 0 {
        String::new()
    } else {
        format!("line {}, column {}", error.line(), error.column())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_names_where_and_never_what() {
        let error = serde_json::from_str::<serde_json::Value>(r#"{"secret": "x" "#).unwrap_err();
        assert_eq!(json_error_position(&error), "line 1, column 15");

        let error = serde_json::from_str::<u32>(r#""secret""#).unwrap_err();
        assert!(
            error.to_string().contains("secret"),
            "serde quotes it: {error}"
        );
        assert_eq!(json_error_position(&error), "line 1, column 8");

        let error = serde_json::from_value::<u32>(serde_json::json!("secret")).unwrap_err();
        assert_eq!(json_error_position(&error), "");
    }
}
