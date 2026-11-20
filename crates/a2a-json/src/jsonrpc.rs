//! The JSON-RPC 2.0 envelope of A2A's JSON-RPC binding, and the error codes
//! that binding uses.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{one_of, present};

/// The JSON-RPC version of every request and response.
pub const VERSION: &str = "2.0";

/// The error codes of JSON-RPC 2.0, and those A2A assigns to its errors.
pub mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const TASK_NOT_FOUND: i32 = -32001;
    pub const TASK_NOT_CANCELABLE: i32 = -32002;
    pub const PUSH_NOTIFICATION_NOT_SUPPORTED: i32 = -32003;
    pub const UNSUPPORTED_OPERATION: i32 = -32004;
    pub const CONTENT_TYPE_NOT_SUPPORTED: i32 = -32005;
    pub const INVALID_AGENT_RESPONSE: i32 = -32006;
    pub const EXTENDED_AGENT_CARD_NOT_CONFIGURED: i32 = -32007;
    pub const EXTENSION_SUPPORT_REQUIRED: i32 = -32008;
    pub const VERSION_NOT_SUPPORTED: i32 = -32009;

    /// The `reason` of the `google.rpc.ErrorInfo` for an A2A error's code, or
    /// `None` for a code that isn't one of A2A's.
    pub fn reason(code: i32) -> Option<&'static str> {
        Some(match code {
            TASK_NOT_FOUND => "TASK_NOT_FOUND",
            TASK_NOT_CANCELABLE => "TASK_NOT_CANCELABLE",
            PUSH_NOTIFICATION_NOT_SUPPORTED => "PUSH_NOTIFICATION_NOT_SUPPORTED",
            UNSUPPORTED_OPERATION => "UNSUPPORTED_OPERATION",
            CONTENT_TYPE_NOT_SUPPORTED => "CONTENT_TYPE_NOT_SUPPORTED",
            INVALID_AGENT_RESPONSE => "INVALID_AGENT_RESPONSE",
            EXTENDED_AGENT_CARD_NOT_CONFIGURED => "EXTENDED_AGENT_CARD_NOT_CONFIGURED",
            EXTENSION_SUPPORT_REQUIRED => "EXTENSION_SUPPORT_REQUIRED",
            VERSION_NOT_SUPPORTED => "VERSION_NOT_SUPPORTED",
            _ => return None,
        })
    }
}

/// The `@type` of a `google.rpc.ErrorInfo` in an error's `data`.
pub const ERROR_INFO_TYPE: &str = "type.googleapis.com/google.rpc.ErrorInfo";

/// The `domain` of the `ErrorInfo` for an A2A error.
pub const ERROR_INFO_DOMAIN: &str = "a2a-protocol.org";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    /// A string, a number or `null`. `None` for a notification, which has no
    /// response.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: Value, method: &str, params: Value) -> Self {
        Request {
            jsonrpc: VERSION.to_string(),
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
        }
    }
}

/// A response, which has exactly one of a result and an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ResponseFields", into = "ResponseFields")]
pub struct Response {
    /// The request's id, or `null` when the request's id could not be read.
    pub id: Value,
    pub result: Result<Value, Error>,
}

impl Response {
    pub fn result(id: Value, result: Value) -> Self {
        Response {
            id,
            result: Ok(result),
        }
    }

    pub fn error(id: Value, error: Error) -> Self {
        Response {
            id,
            result: Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Error {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    /// An error with `code` and `message`. For one of A2A's codes, its `data`
    /// has a `google.rpc.ErrorInfo` with the error's reason and A2A's domain.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
            data: code::reason(code).map(|reason| {
                json!([{
                    "@type": ERROR_INFO_TYPE,
                    "reason": reason,
                    "domain": ERROR_INFO_DOMAIN,
                }])
            }),
        }
    }
}

// A response as its JSON fields. A `null` result is a result, and a `null`
// error is no error.
#[derive(Serialize, Deserialize)]
struct ResponseFields {
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Error>,
}

impl TryFrom<ResponseFields> for Response {
    type Error = String;

    fn try_from(fields: ResponseFields) -> Result<Self, String> {
        if fields.jsonrpc != VERSION {
            return Err(format!("a response's jsonrpc is not \"{VERSION}\""));
        }
        let result = one_of([fields.result.map(Ok), fields.error.map(Err)], "a response")?;
        Ok(Response {
            id: fields.id,
            result,
        })
    }
}

impl From<Response> for ResponseFields {
    fn from(response: Response) -> Self {
        let (result, error) = match response.result {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        ResponseFields {
            jsonrpc: VERSION.to_string(),
            id: response.id,
            result,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    #[test]
    fn a_request_is_written_and_read() {
        let request = Request::new(json!(1), "GetTask", json!({ "id": "t1" }));
        let json = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "GetTask",
            "params": { "id": "t1" },
        });
        assert_eq!(json!(request), json);
        assert_eq!(read::<Request>(json), Ok(request));
    }

    #[test]
    fn a_request_without_an_id_is_a_notification() {
        let request: Request = read(json!({ "jsonrpc": "2.0", "method": "GetTask" })).unwrap();
        assert_eq!(request.id, None);
        assert_eq!(request.params, None);

        let request: Request =
            read(json!({ "jsonrpc": "2.0", "id": null, "method": "GetTask" })).unwrap();
        assert_eq!(request.id, Some(Value::Null));
    }

    #[test]
    fn a_response_has_a_result_or_an_error() {
        let response = Response::result(json!("a"), json!({ "id": "t1" }));
        let json = json!({ "jsonrpc": "2.0", "id": "a", "result": { "id": "t1" } });
        assert_eq!(json!(response), json);
        assert_eq!(read::<Response>(json), Ok(response));

        let error = Error {
            data: Some(json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo" }])),
            ..Error::new(code::TASK_NOT_FOUND, "no task with id t1")
        };
        let response = Response::error(json!(2), error);
        let json = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32001,
                "message": "no task with id t1",
                "data": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo" }],
            },
        });
        assert_eq!(json!(response), json);
        assert_eq!(read::<Response>(json), Ok(response));
    }

    #[test]
    fn an_a2a_error_has_an_error_info_and_a_json_rpc_error_has_none() {
        assert_eq!(
            json!(Error::new(
                code::TASK_NOT_CANCELABLE,
                "task t1 is in a terminal state"
            )),
            json!({
                "code": -32002,
                "message": "task t1 is in a terminal state",
                "data": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "TASK_NOT_CANCELABLE",
                    "domain": "a2a-protocol.org",
                }],
            })
        );
        assert_eq!(Error::new(code::INVALID_PARAMS, "not valid").data, None);
    }

    #[test]
    fn a_null_result_is_a_result_and_a_null_error_is_no_error() {
        let response: Response =
            read(json!({ "jsonrpc": "2.0", "id": 1, "result": null, "error": null })).unwrap();
        assert_eq!(response.result, Ok(Value::Null));
    }

    #[test]
    fn a_response_with_neither_or_both_or_another_version_is_not_read() {
        assert_eq!(
            read::<Response>(json!({ "jsonrpc": "2.0", "id": 1 })).unwrap_err(),
            "a response has none of its possible fields"
        );
        assert_eq!(
            read::<Response>(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {},
                "error": { "code": -32603, "message": "failed" },
            }))
            .unwrap_err(),
            "a response has more than one of its possible fields"
        );
        assert_eq!(
            read::<Response>(json!({ "jsonrpc": "1.0", "id": 1, "result": {} })).unwrap_err(),
            "a response's jsonrpc is not \"2.0\""
        );
    }
}
