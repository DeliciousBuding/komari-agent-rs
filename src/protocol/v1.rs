//! Protocol v1: flat JSON reports for backward compatibility.
//!
//! v1 predates the JSON-RPC 2.0 envelope.  Reports are raw JSON objects
//! sent directly over WebSocket text frames (`/api/clients/report`) or
//! HTTP POST body — no `"jsonrpc"`, `"method"`, or `"id"` wrapper.
//!
//! Wire format:
//! ```json
//! {"cpu":{"percent":12.5},"ram":{"total":8192,...},...}
//! ```
//!
//! See Go reference: `protocol/v1/report.go`  (type ReportPayload = []byte)
//!
//! The v1 `encode_report` builder and `ReportPayload` alias are part of the
//! backward-compatibility parity surface but not all are wired into the live
//! agent. Allow dead_code for the parity surface.
#![allow(dead_code)]

/// Raw v1 report payload — flat JSON bytes, no JSON-RPC envelope.
///
/// Equivalent to Go's `type ReportPayload = []byte`.
/// The monitor produces flat JSON suitable for direct wire transmission;
/// v2 wraps the same data in a JSON-RPC notification, v1 sends it as-is.
pub type ReportPayload = Vec<u8>;

/// Wrap pre-encoded monitoring JSON as a v1 `ReportPayload`.
///
/// v1 is the identity protocol: the monitor already produces flat JSON.
/// This gives the wire layer a typed handle.
#[inline]
pub fn encode_report(json_bytes: &[u8]) -> ReportPayload {
    json_bytes.to_vec()
}
