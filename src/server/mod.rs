//! Server orchestration: WebSocket event loop, reconnection, heartbeat,
//! and server-message dispatch.
//!
//! This is the agent's runtime backbone after config loading.  It runs a
//! single-threaded, never-returning loop that:
//!
//! 1. Initialises TLS (OS-native root certificates via rustls/ring)
//! 2. Uploads basic system info (once, then periodically)
//! 3. Connects to the Komari server via WebSocket
//! 4. Sends heartbeats, reads/dispatches server messages
//! 5. Reconnects on connection loss with protocol FSM + backoff
//!
//! # Architecture (post-refactor)
//!
//! `run()` delegates to [`reconnection::run_reconnection_loop`], which owns
//! the full connect→maintain→reconnect lifecycle driven by:
//! - [`crate::protocol::fsm::ProtocolFsm`] — 3-strike v2→v1 fallback
//! - [`backoff::Backoff`] — exponential retry delays
//! - [`update_basic_info`] — periodic system-info refresh
//! - [`build_static_heartbeat`] — fallback heartbeat (until monitor wired)

pub mod backoff;
pub mod reconnection;
pub mod task;

use crate::config::Config;
use crate::protocol::v2;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
// Logging shim — switch to `log` crate when it lands in Cargo.toml.
// ═══════════════════════════════════════════════════════════════════════════

macro_rules! info {
    ($($arg:tt)*) => (eprintln!("[komari] {}", format!($($arg)*)));
}
macro_rules! warn {
    ($($arg:tt)*) => (eprintln!("[komari] WARN: {}", format!($($arg)*)));
}
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => (eprintln!("[komari] ERROR: {}", format!($($arg)*)));
}
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!("[komari] DEBUG: {}", format!($($arg)*));
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Main entry point for the agent runtime.  **Never returns.**
///
/// Delegates all orchestration to [`reconnection::run_reconnection_loop`].
pub fn run(config: &Config) -> ! {
    reconnection::run_reconnection_loop(config)
}

// ═══════════════════════════════════════════════════════════════════════════
// Heartbeat (fallback — kept until monitor is fully wired)
// ═══════════════════════════════════════════════════════════════════════════

/// Build a minimal JSON-RPC v2 `agent.report` notification for connectivity
/// testing.  Placeholder until real system metrics (via `monitor::generate_report`)
/// are wired in Phase 2.
#[allow(dead_code)]
fn build_static_heartbeat() -> Vec<u8> {
    let params = br#"{"cpu":{"usage":0.0},"ram":{"total":0,"used":0,"swap":0},"disk":[],"network":[],"uptime":0,"os":"","arch":""}"#;
    v2::new_notification(v2::METHOD_AGENT_REPORT, params)
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic info upload (periodic — called from reconnection loop)
// ═══════════════════════════════════════════════════════════════════════════

/// Upload basic system identification info to the Komari server.
///
/// Called at startup and periodically thereafter.  POSTs a JSON-RPC v2
/// `agent.basicInfo` notification or v1 raw JSON.
pub(super) fn update_basic_info(config: &Config, tls_cfg: &Arc<rustls::ClientConfig>) -> Result<(), String> {
    let base = config.endpoint.trim_end_matches('/');
    let encoded_token = crate::ws::url_encode(&config.token);

    let (url, body) = if config.protocol_version >= 2 {
        let url = format!("{}/api/clients/v2/rpc?token={}", base, encoded_token);
        let body = build_basic_info_v2();
        (url, body)
    } else {
        let url = format!(
            "{}/api/clients/uploadBasicInfo?token={}",
            base, encoded_token
        );
        let body = build_basic_info_v1();
        (url, body)
    };

    let cf_headers = build_cf_headers(config);

    match crate::http::http_post(
        &url,
        &body,
        "application/json",
        None,
        cf_headers,
        tls_cfg,
    ) {
        Ok(resp) if resp.status_code == 200 => {
            info!("Basic info uploaded successfully");
            Ok(())
        }
        Ok(resp) => {
            let msg = format!("Basic info upload returned HTTP {}", resp.status_code);
            warn!("{}", msg);
            Err(msg)
        }
        Err(e) => {
            let msg = format!("Basic info upload failed: {}", e);
            warn!("{}", msg);
            Err(msg)
        }
    }
}

fn build_basic_info_v2() -> Vec<u8> {
    let info = br#"{"cpu_name":"","cpu_cores":0,"cpu_physical_cores":0,"arch":"","os":"","kernel_version":"","ipv4":"","ipv6":"","mem_total":0,"swap_total":0,"disk_total":0,"gpu_name":"","virtualization":"","version":"0.1.0"}"#;
    let params = {
        let mut v = Vec::with_capacity(info.len() + 20);
        v.extend_from_slice(b"{\"info\":");
        v.extend_from_slice(info);
        v.push(b'}');
        v
    };
    v2::new_notification(v2::METHOD_AGENT_BASIC_INFO, &params)
}

fn build_basic_info_v1() -> Vec<u8> {
    br#"{"cpu_name":"","cpu_cores":0,"cpu_physical_cores":0,"arch":"","os":"","kernel_version":"","ipv4":"","ipv6":"","mem_total":0,"swap_total":0,"disk_total":0,"gpu_name":"","virtualization":"","version":"0.1.0"}"#.to_vec()
}

fn build_cf_headers(config: &Config) -> Option<(&str, &str)> {
    if config.cf_access_client_id.is_empty() || config.cf_access_client_secret.is_empty() {
        None
    } else {
        Some((&config.cf_access_client_id, &config.cf_access_client_secret))
    }
}
