//! Protocol negotiation and connection lifecycle state machines.
//!
//! Two independent, zero-allocation FSMs:
//! - [`ProtocolFsm`] — v1/v2 protocol version with 3-strike fallback.
//! - [`ConnectionFsm`] — TCP → TLS → WS connection lifecycle.
//!
//! ## Fallback rules (match Go `protocol_fallback.go`)
//!
//! 1. Start at `WsV2`.
//! 2. WS connect fails → retry WsV2 up to `max_retries` (caller-driven).
//! 3. After 3 consecutive v2 failures → auto-downgrade to next mode.
//! 4. WsV2 → HttpV2 → HttpV1 (terminal).
//! 5. On any successful connection → `on_success()` resets all counters.
//! 6. On reconnect after disconnect → `on_reconnect()` retries from WsV2.

/// Which protocol version and transport is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMode {
    /// WebSocket with v2 JSON-RPC 2.0 protocol.
    WsV2,
    /// HTTP POST fallback with v2 JSON-RPC 2.0 protocol.
    HttpV2,
    /// HTTP POST with v1 flat-report protocol (terminal fallback).
    HttpV1,
}

/// What kind of failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// WebSocket TCP/TLS connect failed.
    WsConnect,
    /// WebSocket write/send failed.
    WsWrite,
    /// HTTP POST transport failed.
    HttpPost,
    /// HTTP POST returned a non-success status code.
    HttpStatus(u16),
}

/// Protocol-version state machine with 3-strike fallback.
///
/// All counters are `u8` — zero heap allocation. `Copy`-able.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolFsm {
    mode: ProtocolMode,
    consecutive_v2_failures: u8,
    ws_v2_failures: u8,
    http_v2_failures: u8,
}

impl ProtocolFsm {
    pub const FALLBACK_THRESHOLD: u8 = 3;

    /// Create a new FSM starting at `WsV2`.
    pub fn new() -> Self {
        Self {
            mode: ProtocolMode::WsV2,
            consecutive_v2_failures: 0,
            ws_v2_failures: 0,
            http_v2_failures: 0,
        }
    }

    #[inline]
    pub fn mode(&self) -> ProtocolMode {
        self.mode
    }

    #[inline]
    pub fn is_terminal(&self) -> bool {
        self.mode == ProtocolMode::HttpV1
    }

    /// Reset all failure counters on success.
    #[inline]
    pub fn on_success(&mut self) {
        self.consecutive_v2_failures = 0;
        self.ws_v2_failures = 0;
        self.http_v2_failures = 0;
    }

    /// Record a failure. Returns `true` if mode was downgraded.
    pub fn on_failure(&mut self, kind: FailureKind) -> bool {
        if self.mode == ProtocolMode::HttpV1 {
            self.consecutive_v2_failures = self.consecutive_v2_failures.saturating_add(1);
            match kind {
                FailureKind::WsConnect | FailureKind::WsWrite => {
                    self.ws_v2_failures = self.ws_v2_failures.saturating_add(1);
                }
                FailureKind::HttpPost | FailureKind::HttpStatus(_) => {
                    self.http_v2_failures = self.http_v2_failures.saturating_add(1);
                }
            }
            return false;
        }

        self.consecutive_v2_failures = self.consecutive_v2_failures.saturating_add(1);
        match kind {
            FailureKind::WsConnect | FailureKind::WsWrite => {
                self.ws_v2_failures = self.ws_v2_failures.saturating_add(1);
            }
            FailureKind::HttpPost | FailureKind::HttpStatus(_) => {
                self.http_v2_failures = self.http_v2_failures.saturating_add(1);
            }
        }

        if self.consecutive_v2_failures >= Self::FALLBACK_THRESHOLD {
            let old = self.mode;
            self.mode = match self.mode {
                ProtocolMode::WsV2 => ProtocolMode::HttpV2,
                ProtocolMode::HttpV2 => ProtocolMode::HttpV1,
                ProtocolMode::HttpV1 => unreachable!(),
            };
            self.consecutive_v2_failures = 0;
            self.ws_v2_failures = 0;
            self.http_v2_failures = 0;
            return self.mode != old;
        }

        false
    }

    /// Reset mode to WsV2 and clear all counters (called on reconnect).
    pub fn on_reconnect(&mut self) {
        self.mode = ProtocolMode::WsV2;
        self.consecutive_v2_failures = 0;
        self.ws_v2_failures = 0;
        self.http_v2_failures = 0;
    }
}

impl Default for ProtocolFsm {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ConnectionFsm — TCP → TLS → WS lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Resolving,
    TcpConnecting,
    TlsHandshaking,
    WsHandshaking,
    Online,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    ResolveStarted,
    ResolveOk,
    ResolveFailed,
    TcpConnectStarted,
    TcpConnected,
    TcpFailed,
    TlsStarted,
    TlsOk,
    TlsFailed,
    WsUpgradeOk,
    WsUpgradeFailed,
    Disconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionFsm {
    state: ConnState,
}

impl ConnectionFsm {
    pub fn new() -> Self {
        Self {
            state: ConnState::Disconnected,
        }
    }

    #[inline]
    pub fn state(&self) -> ConnState {
        self.state
    }

    #[inline]
    pub fn is_online(&self) -> bool {
        self.state == ConnState::Online
    }

    pub fn transition(&mut self, event: ConnEvent) -> ConnState {
        use ConnEvent::*;

        // Disconnect event — global reset from any state.
        if event == Disconnect {
            self.state = ConnState::Disconnected;
            return self.state;
        }

        self.state = match (self.state, event) {
            (ConnState::Disconnected, ResolveStarted) => ConnState::Resolving,
            (ConnState::Disconnected, TcpConnectStarted) => ConnState::TcpConnecting,
            (ConnState::Resolving, ResolveOk) => ConnState::TcpConnecting,
            (ConnState::Resolving, ResolveFailed) => ConnState::Disconnected,
            (ConnState::Resolving, TcpConnectStarted) => ConnState::TcpConnecting,
            (ConnState::TcpConnecting, TcpConnected) => ConnState::TlsHandshaking,
            (ConnState::TcpConnecting, TcpFailed) => ConnState::Disconnected,
            (ConnState::TcpConnecting, TlsStarted) => ConnState::TlsHandshaking,
            (ConnState::TlsHandshaking, TlsOk) => ConnState::WsHandshaking,
            (ConnState::TlsHandshaking, TlsFailed) => ConnState::Disconnected,
            (ConnState::WsHandshaking, WsUpgradeOk) => ConnState::Online,
            (ConnState::WsHandshaking, WsUpgradeFailed) => ConnState::Disconnected,
            _ => self.state,
        };

        self.state
    }
}

impl Default for ConnectionFsm {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_ws_v2() {
        let fsm = ProtocolFsm::new();
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
    }

    #[test]
    fn three_strikes_ws_to_http_v2() {
        let mut fsm = ProtocolFsm::new();
        assert!(!fsm.on_failure(FailureKind::WsConnect));
        assert!(!fsm.on_failure(FailureKind::WsConnect));
        assert!(fsm.on_failure(FailureKind::WsConnect));
        assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
    }

    #[test]
    fn reconnect_resets_to_ws_v2() {
        let mut fsm = ProtocolFsm::new();
        for _ in 0..6 {
            fsm.on_failure(FailureKind::HttpPost);
        }
        assert_eq!(fsm.mode(), ProtocolMode::HttpV1);
        fsm.on_reconnect();
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
    }

    #[test]
    fn connection_fsm_happy_path() {
        let mut fsm = ConnectionFsm::new();
        assert_eq!(fsm.transition(ConnEvent::ResolveStarted), ConnState::Resolving);
        assert_eq!(fsm.transition(ConnEvent::ResolveOk), ConnState::TcpConnecting);
        assert_eq!(fsm.transition(ConnEvent::TcpConnected), ConnState::TlsHandshaking);
        assert_eq!(fsm.transition(ConnEvent::TlsOk), ConnState::WsHandshaking);
        assert_eq!(fsm.transition(ConnEvent::WsUpgradeOk), ConnState::Online);
        assert!(fsm.is_online());
    }
}
