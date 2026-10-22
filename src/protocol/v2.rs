//! JSON-RPC 2.0 types, method constants, and wire-format builders.
//!
//! Wire format must match Go `protocol/v2/jsonrpc.go` byte-for-byte.
//! No serde — type definitions use raw JSON bytes (`Vec<u8>`) for
//! dynamically-typed fields (`params`, `result`, `data`), and a
//! simple `Id` enum for the `id` field.
//!
//! Encoding is delegated to `crate::json` (hand-rolled, zero-alloc).
//! The builder functions below are convenience helpers that allocate
//! (cold path, called once per message) and are expected to be replaced
//! by `JsonBuf`-based builders in a future refactor.
//!
//! The full JSON-RPC 2.0 type surface (method constants, request/response/error/
//! event structs) is part of the wire-format parity contract and is referenced by
//! tests and future code paths; not every entry point is wired into the live agent
//! today. Allow dead_code for the parity surface.
#![allow(dead_code)]

#[allow(unused_imports)]
use crate::json::{EncodeJson, JsonBuf, JsonErr};

// ── Method constants ─────────────────────────────────────────────────────

pub const VERSION: &str = "2.0";

pub const METHOD_AGENT_REPORT: &str = "agent.report";
pub const METHOD_AGENT_BASIC_INFO: &str = "agent.basicInfo";
pub const METHOD_AGENT_PING_RESULT: &str = "agent.pingResult";
pub const METHOD_AGENT_TASK_RESULT: &str = "agent.taskResult";
pub const METHOD_AGENT_EXEC: &str = "agent.exec";
pub const METHOD_AGENT_PING: &str = "agent.ping";
pub const METHOD_AGENT_MESSAGE: &str = "agent.message";
pub const METHOD_AGENT_EVENT: &str = "agent.event";
pub const METHOD_AGENT_TERMINAL: &str = "agent.terminal.request";
pub const METHOD_AGENT_PULL: &str = "agent.pull";

// Server-initiated agent-control events (upstream server `main` after
// 1.5.0-fix1; no-op on servers <= 1.5.0-fix1, which never emit these).
pub const METHOD_AGENT_STARTUP_CONFIG: &str = "agent.startupConfig";
pub const METHOD_AGENT_STARTUP_CONFIG_RESULT: &str = "agent.startupConfig.result";
pub const METHOD_AGENT_SWITCH_VERSION: &str = "agent.switchVersion";

// ── JSON-RPC 2.0 request/response id ─────────────────────────────────────

/// A JSON-RPC 2.0 `id` value (string, integer, or null).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    Str(String),
    Num(i64),
    Null,
}

// ── JSON-RPC 2.0 structs ─────────────────────────────────────────────────
//
// `params`, `result`, and `data` carry raw, pre-encoded JSON bytes so we
// never pay for intermediate DOM deserialisation.  The builder functions
// (`new_notification`, `new_request`) already accept `&[u8]` for params.
// For receiving messages the bytes are parsed lazily by the caller.

/// JSON-RPC 2.0 request object.
#[derive(Debug, Clone)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    /// Raw JSON bytes for the `params` field (may be `[]`, `{}`, `null`, etc.).
    pub params: Option<Vec<u8>>,
    pub id: Option<Id>,
}

/// JSON-RPC 2.0 response object.
#[derive(Debug, Clone)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<Id>,
    /// Raw JSON bytes for the `result` field.
    pub result: Option<Vec<u8>>,
    pub error: Option<RpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    /// Raw JSON bytes for the optional `data` field.
    pub data: Option<Vec<u8>>,
}

/// Agent event (pull-based delivery).
#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub method: String,
    pub params: Option<Vec<u8>>,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
}

/// Result of `agent.pull`.
#[derive(Debug, Clone)]
pub struct EventResult {
    pub status: Option<String>,
    pub events: Vec<Event>,
}

// ── Builder functions ────────────────────────────────────────────────────
//
// These produce `Vec<u8>` directly (heap allocation, cold path only).
// The hot path should use `JsonBuf` from `crate::json` to avoid allocation.

/// Build a JSON-RPC 2.0 notification bytes (no `"id"` field).
///
/// Wire: `{"jsonrpc":"2.0","method":"<method>","params":<params>}`
#[inline]
pub fn new_notification(method: &str, params: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(64 + method.len() + params.len());
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":");
    crate::json::write_json_string(&mut v, method);
    v.extend_from_slice(b",\"params\":");
    v.extend_from_slice(params);
    v.push(b'}');
    v
}

/// Build a JSON-RPC 2.0 request bytes (includes `"id"`).
///
/// Wire: `{"jsonrpc":"2.0","method":"<method>","params":<params>,"id":"<id>"}`
#[inline]
pub fn new_request(id: &str, method: &str, params: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(80 + method.len() + params.len() + id.len());
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":");
    crate::json::write_json_string(&mut v, method);
    v.extend_from_slice(b",\"params\":");
    v.extend_from_slice(params);
    v.extend_from_slice(b",\"id\":");
    crate::json::write_json_string(&mut v, id);
    v.push(b'}');
    v
}

/// Build `agent.startupConfig.result` params:
/// `{"request_id":"<id>","config":{...}}`.
///
/// `config_json` must be a complete JSON object (see
/// `Config::startup_config_json`). The result travels as a **request** whose
/// `id` echoes `request_id`, matching the upstream Go agent; the server
/// binds the answer to the authenticated connection UUID, never to a
/// body-supplied id.
pub fn build_startup_config_params(request_id: &str, config_json: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(config_json.len() + 40);
    v.extend_from_slice(b"{\"request_id\":");
    crate::json::write_json_string(&mut v, request_id);
    v.extend_from_slice(b",\"config\":");
    v.extend_from_slice(config_json);
    v.push(b'}');
    v
}

/// Validate the JSON-RPC envelope separately from HTTP status. A 200 with
/// an RPC error (or an empty/non-RPC proxy response) is not a successful call.
/// Never return server-controlled error text: it may contain sensitive data.
pub fn validate_response(body: &[u8]) -> Result<(), &'static str> {
    if response_has_error(body) {
        return Err("server returned a JSON-RPC error");
    }
    if crate::json::object_field(body, "jsonrpc") != Some(b"\"2.0\"".as_slice())
        || crate::json::object_field(body, "result").is_none_or(|v| v == b"null")
    {
        return Err("invalid JSON-RPC response envelope");
    }
    Ok(())
}

/// Only the envelope's error field matters, not strings or nested event data.
pub fn response_has_error(body: &[u8]) -> bool {
    crate::json::object_field(body, "error").is_some_and(|v| v != b"null")
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn request_ids_and_methods_round_trip_without_json_injection() {
        use crate::json::{decode_json_string, object_field};
        let id = "quote\"slash\\\n世界";
        let method = "method\"\\\n";
        let request = new_request(id, method, b"{}");
        assert_eq!(
            decode_json_string(object_field(&request, "id").unwrap()).as_deref(),
            Some(id)
        );
        assert_eq!(
            decode_json_string(object_field(&request, "method").unwrap()).as_deref(),
            Some(method)
        );
        let notification = new_notification(method, b"{}");
        assert_eq!(
            decode_json_string(object_field(&notification, "method").unwrap()).as_deref(),
            Some(method)
        );
    }

    #[test]
    fn malformed_or_duplicate_envelopes_are_not_successes() {
        for body in [
            br#"{"jsonrpc":"2.0","result":{} garbage }"#.as_slice(),
            br#"{"jsonrpc":"2.0","result":{},"result":null}"#,
            br#"{"jsonrpc":"2.0","jsonrpc":"wrong","result":{}}"#,
            br#"{"jsonrpc":"2.0","result":{},"error":null,"error":{"code":-1}}"#,
            br#"{"jsonrpc":"2.0","result":{},"error":null,"\u0065rror":{"code":-1}}"#,
            br#"{"jsonrpc":"2.0","result":{},"\u0065rror":{"code":-1}}"#,
        ] {
            assert!(validate_response(body).is_err());
        }
    }

    #[test]
    fn rejects_rpc_errors_even_when_http_succeeded() {
        let body = br#"{"jsonrpc":"2.0","id":"report","error":{"code":-32000,"message":"private details"}}"#;
        assert!(response_has_error(body));
        assert_eq!(
            validate_response(body),
            Err("server returned a JSON-RPC error")
        );
        assert!(validate_response(br#"{}"#).is_err());
        assert!(validate_response(b"OK").is_err());
    }

    #[test]
    fn accepts_result_without_confusing_event_data_for_envelope_error() {
        let body = br#"{ "jsonrpc" : "2.0", "id":"report", "result":{"events":[{"params":{"error":"quoted } , \"error\": text"}}]}, "error":null }"#;
        assert!(!response_has_error(body));
        assert!(validate_response(body).is_ok());
        assert!(validate_response(br#"{"jsonrpc":"2.0","result":null}"#).is_err());
    }
}
