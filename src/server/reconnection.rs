//! Reconnection loop: the top-level orchestration that owns the
//! connect -> maintain -> reconnect cycle with backoff and FSM integration.
//!
//! Replaces the old inline `run()` logic with a dedicated, testable
//! function that delegates to:
//! - `crate::protocol::fsm::ProtocolFsm` for protocol mode tracking
//! - `super::backoff::Backoff` for retry delays
//! - `super::connect_ws` / `super::tick_loop` for the actual I/O

use crate::config::Config;
use crate::protocol::fsm::{FailureKind, ProtocolFsm, ProtocolMode};
use crate::server::backoff::Backoff;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    connect_ws, debug, error, info, tick_loop, update_basic_info, warn, WsErr,
};

/// Run the full agent reconnection loop.  **Never returns.**
///
/// This is the new top-level function called from `super::run()`.
/// It initialises TLS, uploads basic info, then enters the
/// connect->maintain->reconnect cycle driven by a `Backoff` and
/// informed by `ProtocolFsm`.
///
/// # Periodic basic info refresh
///
/// Every `config.info_report_interval` minutes, `update_basic_info()`
/// is called to refresh server-side system info (non-fatal on failure).
pub fn run_reconnection_loop(config: &Config) -> ! {
    // Step 1: Initialise TLS config (fatal on failure).
    let tls_cfg = match crate::tls::make_tls_config(config) {
        Ok(cfg) => Arc::new(cfg),
        Err(e) => {
            error!("TLS config init failed: {}", e);
            std::process::exit(1);
        }
    };

    // Step 2: Initial basic info upload (non-fatal).
    if let Err(e) = update_basic_info(config, &tls_cfg) {
        warn!("initial basic info upload failed (continuing): {}", e);
    }

    // Step 3: Protocol FSM (drives mode selection for URL / dispatch).
    let fsm = ProtocolFsm::new(ProtocolMode::from_config(config.protocol_version));

    // Step 4: Backoff tracker for reconnection delays.
    let mut backoff = Backoff::new(config.max_retries, config.reconnect_interval);

    // Step 5: Track last basic info refresh for periodic re-upload.
    let mut last_info_refresh = Instant::now();
    let info_interval = Duration::from_secs(config.info_report_interval * 60);

    // =================================================================
    // Main loop — never exits unless max_retries is exhausted
    // =================================================================
    loop {
        // Periodic basic info refresh.
        if last_info_refresh.elapsed() >= info_interval {
            debug!("running periodic basic info refresh");
            if let Err(e) = update_basic_info(config, &tls_cfg) {
                warn!("periodic basic info refresh failed: {}", e);
            }
            last_info_refresh = Instant::now();
        }

        // Attempt connection.
        match connect_ws(config, &tls_cfg) {
            Ok(conn) => {
                info!(
                    "WebSocket connected (mode: {:?}), entering tick loop",
                    fsm.mode()
                );
                backoff.reset();
                tick_loop(conn, config);
                // tick_loop returned -> connection lost.
                debug!("tick_loop exited — connection lost");
            }
            Err(WsErr::Other(ref msg)) => {
                let kind = classify_connect_error(msg);
                error!(
                    "WebSocket connection failed ({:?}): {}",
                    kind, msg
                );
            }
            Err(WsErr::Timeout) => {
                error!("WebSocket connection timed out");
            }
        }

        // --- Reconnection backoff ---
        if backoff.exhausted() {
            error!(
                "Max retries ({}) exhausted — exiting",
                backoff.max_retries
            );
            std::process::exit(1);
        }

        let delay = backoff.next_delay();
        info!(
            "Reconnecting in {:.1}s (failure #{} of {})...",
            delay.as_secs_f64(),
            backoff.failures,
            if backoff.max_retries == 0 {
                "unlimited".to_string()
            } else {
                backoff.max_retries.to_string()
            }
        );
        std::thread::sleep(delay);
    }
}

/// Classify a connection error message into a `FailureKind`.
///
/// String-based heuristic matching the Go error patterns.
fn classify_connect_error(msg: &str) -> FailureKind {
    let lower = msg.to_lowercase();
    if lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("refused")
        || lower.contains("unreachable")
    {
        FailureKind::Network
    } else if lower.contains("tls")
        || lower.contains("certificate")
        || lower.contains("handshake")
    {
        FailureKind::Tls
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("upgrade")
    {
        FailureKind::UpgradeRejected
    } else {
        FailureKind::Disconnect
    }
}
