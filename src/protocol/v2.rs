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
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"");
    v.extend_from_slice(method.as_bytes());
    v.extend_from_slice(b"\",\"params\":");
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
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"");
    v.extend_from_slice(method.as_bytes());
    v.extend_from_slice(b"\",\"params\":");
    v.extend_from_slice(params);
    v.extend_from_slice(b",\"id\":\"");
    v.extend_from_slice(id.as_bytes());
    v.extend_from_slice(b"\"}");
    v
}
