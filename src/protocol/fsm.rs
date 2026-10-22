//! Protocol negotiation and connection lifecycle state machines.
//!
//! Two independent, zero-allocation FSMs:
//! - [`ProtocolFsm`] — v2 transport selection with 3-strike WS fallback.
//! - [`ConnectionFsm`] — TCP → TLS → WS connection lifecycle.
//!
//! ## Fallback rules
//!
//! 1. Start at `WsV2`.
//! 2. After 3 consecutive WS failures, use `HttpV2` indefinitely.
//! 3. Periodically re-probe WS unless HTTP-only was explicitly selected.
//! 4. Never change wire protocol: Komari >= 1.5 removed v1 endpoints.
//!
//! `ProtocolFsm` drives the live reconnection loop. `ConnectionFsm` (TCP → TLS →
//! WS lifecycle) plus a few `FailureKind` variants are kept as a complete
//! parity/observability surface but not yet wired into the agent. Allow
//! dead_code for that surface.
#![allow(dead_code)]
//! 5. On any successful connection → `on_success()` resets all counters.
//! 6. On preferred-transport re-probe → `on_reconnect()` clears counters.

/// Which v2 transport is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMode {
    /// WebSocket with v2 JSON-RPC 2.0 protocol (/api/clients/v2/rpc).
    WsV2,
    /// HTTP POST fallback with v2 JSON-RPC 2.0 protocol.
    HttpV2,
}

/// Both transports use the same v2 endpoint; no legacy route is supported.
pub const RPC_PATH: &str = "/api/clients/v2/rpc";

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

/// Transport state machine with 3-strike WS-to-HTTP fallback.
///
/// All counters are `u8` — zero heap allocation. `Copy`-able.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolFsm {
    mode: ProtocolMode,
    initial_mode: ProtocolMode,
    consecutive_failures: u8,
}

impl ProtocolFsm {
    pub const FALLBACK_THRESHOLD: u8 = 3;

    /// Starts at `WsV2`, or permanently `HttpV2` when `http_only` is set.
    /// Unsupported protocol-version settings are rejected by config validation.
    pub fn new(http_only: bool) -> Self {
        let initial = if http_only {
            ProtocolMode::HttpV2
        } else {
            ProtocolMode::WsV2
        };
        Self {
            mode: initial,
            initial_mode: initial,
            consecutive_failures: 0,
        }
    }

    #[inline]
    pub fn mode(&self) -> ProtocolMode {
        self.mode
    }

    /// The mode the FSM started in (re-probe target).
    #[inline]
    pub fn initial_mode(&self) -> ProtocolMode {
        self.initial_mode
    }

    /// Consecutive failures since the last success or transport switch.
    #[inline]
    pub fn consecutive_failures(&self) -> u8 {
        self.consecutive_failures
    }

    /// Reset all failure counters on success.
    #[inline]
    pub fn on_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Record a failure. Returns true only when switching WS to HTTP.
    /// HTTP failures keep retrying v2; they never terminate or downgrade.
    pub fn on_failure(&mut self, _kind: FailureKind) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.mode == ProtocolMode::WsV2 && self.consecutive_failures >= Self::FALLBACK_THRESHOLD
        {
            self.mode = ProtocolMode::HttpV2;
            self.consecutive_failures = 0;
            return true;
        }

        false
    }

    /// Re-probe the preferred transport. HTTP-only can never select WS.
    pub fn on_reconnect(&mut self) {
        self.mode = self.initial_mode;
        self.consecutive_failures = 0;
    }
}

impl Default for ProtocolFsm {
    fn default() -> Self {
        Self::new(false)
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
        let fsm = ProtocolFsm::new(false);
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
    }

    #[test]
    fn http_only_starts_at_http_v2() {
        let fsm = ProtocolFsm::new(true);
        assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
    }

    #[test]
    fn three_strikes_ws_v2_to_http_v2() {
        let mut fsm = ProtocolFsm::new(false);
        assert!(!fsm.on_failure(FailureKind::WsConnect));
        assert!(!fsm.on_failure(FailureKind::WsConnect));
        assert!(fsm.on_failure(FailureKind::WsConnect));
        assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
    }

    #[test]
    fn http_failures_never_leave_v2() {
        let mut fsm = ProtocolFsm::new(false);
        for _ in 0..3 {
            fsm.on_failure(FailureKind::WsConnect);
        }
        assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
        for _ in 0..1000 {
            assert!(!fsm.on_failure(FailureKind::HttpPost));
            assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
        }
        assert_eq!(fsm.consecutive_failures(), u8::MAX);
    }

    #[test]
    fn reconnect_resets_to_ws_v2() {
        let mut fsm = ProtocolFsm::new(false);
        for _ in 0..3 {
            fsm.on_failure(FailureKind::HttpPost);
        }
        assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
        fsm.on_reconnect();
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
        assert_eq!(fsm.consecutive_failures(), 0);
        fsm.on_success();
        assert!(!fsm.on_failure(FailureKind::WsConnect));
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
    }

    #[test]
    fn http_only_never_probes_ws_even_after_failures() {
        let mut fsm = ProtocolFsm::new(true);
        for _ in 0..1000 {
            assert!(!fsm.on_failure(FailureKind::HttpStatus(503)));
            fsm.on_reconnect();
            assert_eq!(fsm.mode(), ProtocolMode::HttpV2);
        }
    }

    #[test]
    fn successful_session_clears_ws_failure_streak() {
        let mut fsm = ProtocolFsm::new(false);
        fsm.on_failure(FailureKind::WsWrite);
        fsm.on_failure(FailureKind::WsWrite);
        fsm.on_success();
        assert!(!fsm.on_failure(FailureKind::WsWrite));
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
    }

    #[test]
    fn connection_fsm_happy_path() {
        let mut fsm = ConnectionFsm::new();
        assert_eq!(
            fsm.transition(ConnEvent::ResolveStarted),
            ConnState::Resolving
        );
        assert_eq!(
            fsm.transition(ConnEvent::ResolveOk),
            ConnState::TcpConnecting
        );
        assert_eq!(
            fsm.transition(ConnEvent::TcpConnected),
            ConnState::TlsHandshaking
        );
        assert_eq!(fsm.transition(ConnEvent::TlsOk), ConnState::WsHandshaking);
        assert_eq!(fsm.transition(ConnEvent::WsUpgradeOk), ConnState::Online);
        assert!(fsm.is_online());
    }
}
