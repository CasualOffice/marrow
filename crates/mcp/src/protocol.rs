//! JSON-RPC 2.0 over stdio.
//!
//! # Which protocol
//!
//! Part 2 §52 records that MCP 2026-07-28 removed the `initialize` handshake in
//! favour of `server/discover`. That is what the specification says; it is not
//! what deployed clients speak, and this server exists to be used by the client
//! actually on this machine.
//!
//! So both are handled: `initialize` (universal) and `server/discover` (the
//! newer spelling) resolve to the same descriptor. [`PROTOCOL_VERSION`] is a
//! constant precisely because it will need changing, and because a version this
//! server has never been tested against is not something to claim compliance
//! with.
//!
//! # Framing
//!
//! Line-delimited JSON on stdin and stdout. **stdout carries protocol traffic
//! and nothing else** — a stray `println!` corrupts the stream in a way that
//! looks like a client bug. All narration goes to stderr.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The protocol version this server reports.
///
/// Deliberately a constant: see the module note. Changing it is a decision, not
/// a detail.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

pub const SERVER_NAME: &str = "marrow";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A JSON-RPC request. `id` is absent for notifications.
#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// A notification expects no reply — replying to one is a protocol error.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// JSON-RPC error codes. The 4 standard ones plus our own range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcError {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Internal,
}

impl RpcError {
    pub fn code(self) -> i32 {
        match self {
            RpcError::ParseError => -32700,
            RpcError::InvalidRequest => -32600,
            RpcError::MethodNotFound => -32601,
            RpcError::InvalidParams => -32602,
            RpcError::Internal => -32603,
        }
    }
}

/// Build a success response.
pub fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build an error response.
pub fn err(id: Value, e: RpcError, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": e.code(), "message": message }
    })
}

/// A tool failure, as distinct from a protocol failure.
///
/// MCP reports these as a **successful** JSON-RPC response carrying
/// `isError: true`, so the model sees the message and can react. Returning a
/// JSON-RPC error instead makes a bad argument look like a broken server.
pub fn tool_error(id: Value, message: &str) -> Value {
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }),
    )
}

/// A tool success carrying structured JSON.
///
/// The text block holds the same JSON the structured field does. Clients differ
/// in which they read, and a tool that returns prose to one and data to the
/// other has two output formats to keep in step.
pub fn tool_ok(id: Value, value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": value,
            "isError": false
        }),
    )
}

/// The `initialize` / `server/discover` result.
#[derive(Debug, Serialize)]
pub struct ServerDescriptor {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: Value,
    #[serde(rename = "serverInfo")]
    pub server_info: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl Default for ServerDescriptor {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            // Tools only. No resources, no prompts, no sampling — advertising a
            // capability we do not implement makes a client's first call fail.
            capabilities: json!({ "tools": { "listChanged": false } }),
            server_info: json!({ "name": SERVER_NAME, "version": SERVER_VERSION }),
            instructions: Some(
                "Marrow indexes local files and answers with citations to exact \
                 locations. Every result carries a path, a line span, and a \
                 provenance class. Results marked `origin: self_written` were \
                 produced by an agent and must not be cited as independent \
                 evidence."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_an_id_is_a_notification() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(r.is_notification());
    }

    #[test]
    fn a_request_with_an_id_expects_a_reply() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert!(!r.is_notification());
    }

    #[test]
    fn missing_params_default_to_null_rather_than_failing() {
        // Clients omit `params` for no-argument methods.
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(r.params.is_null());
    }

    #[test]
    fn a_tool_failure_is_a_successful_response_carrying_is_error() {
        // The distinction that matters: a bad argument must reach the model as
        // a message, not as a broken server.
        let v = tool_error(json!(1), "no such workspace");
        assert!(v.get("error").is_none(), "must not be a JSON-RPC error");
        assert_eq!(v["result"]["isError"], json!(true));
        assert_eq!(
            v["result"]["content"][0]["text"],
            json!("no such workspace")
        );
    }

    #[test]
    fn a_tool_success_carries_the_same_data_in_both_shapes() {
        let payload = json!({ "hits": 3 });
        let v = tool_ok(json!(1), &payload);
        assert_eq!(v["result"]["structuredContent"], payload);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            payload,
            "text and structured content must not diverge"
        );
    }

    #[test]
    fn the_descriptor_advertises_only_what_is_implemented() {
        let d = ServerDescriptor::default();
        let caps = serde_json::to_value(&d).unwrap()["capabilities"].clone();
        assert!(caps.get("tools").is_some());
        for unimplemented in ["resources", "prompts", "sampling", "logging"] {
            assert!(
                caps.get(unimplemented).is_none(),
                "advertised {unimplemented} without implementing it"
            );
        }
    }

    #[test]
    fn rpc_error_codes_are_the_standard_ones() {
        assert_eq!(RpcError::ParseError.code(), -32700);
        assert_eq!(RpcError::MethodNotFound.code(), -32601);
        assert_eq!(RpcError::InvalidParams.code(), -32602);
    }
}
