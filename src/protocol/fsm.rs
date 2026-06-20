//! Protocol Finite State Machine for connection lifecycle management.
//!
//! Tracks protocol mode (v1/v2) and classifies connection failures
//! to inform the reconnection strategy in `server::reconnection`.

/// Which protocol version the agent is currently speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMode {
    /// JSON-RPC 2.0 (v2 endpoint: `/api/clients/v2/rpc`).
    V2,
    /// Flat JSON (v1 backward-compat endpoint: `/api/clients/report`).
    V1,
}

impl ProtocolMode {
    /// Derive from the config `protocol_version` field.
    pub fn from_config(version: u8) -> Self {
        if version >= 2 {
            Self::V2
        } else {
            Self::V1
        }
    }
}

/// Categorised reason a WebSocket connection failed or was lost.
///
/// Used by `server::reconnection::run_reconnection_loop` to decide
/// whether to reset the backoff (transient) or escalate (fatal/auth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// DNS resolution failed, TCP connect refused, or network unreachable.
    Network,
    /// TLS handshake error (certificate, protocol version mismatch).
    Tls,
    /// HTTP upgrade was rejected (4xx/5xx) — likely auth or server config.
    UpgradeRejected,
    /// Connection dropped mid-session (I/O error, EOF, close frame).
    Disconnect,
    /// Server sent an explicit close frame (orderly shutdown).
    ServerClose,
    /// The agent gave up after exhausting max_retries.
    MaxRetriesExhausted,
}

/// Protocol state machine — lightweight tracker for connection lifecycle.
///
/// Currently stateless: `mode` is derived from config and does not change
/// at runtime.  State transitions (v1↔v2 negotiation, pull-mode support)
/// will be added as the protocol layer matures.
#[derive(Debug, Clone)]
pub struct ProtocolFsm {
    pub mode: ProtocolMode,
}

impl ProtocolFsm {
    pub fn new(mode: ProtocolMode) -> Self {
        Self { mode }
    }

    /// Returns the current protocol mode.
    pub fn mode(&self) -> ProtocolMode {
        self.mode
    }
}
