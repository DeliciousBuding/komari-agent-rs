//! Reconnection loop with protocol FSM and exponential backoff.
//!
//! Reference: Go agent `EstablishWebSocketConnection()` in `server/websocket.go`.
//!
//! # Flow
//! ```text
//! loop {
//!     fsm.on_reconnect() → connect_with_fsm()
//!       → Ok(conn) → fsm.on_success() → run_tick_loop()
//!       → Err      → fsm.on_failure() → backoff → retry
//!     tick_loop returns → fsm.on_failure() → backoff → reconnect
//! }
//! ```
//!
//! Uses [`crate::protocol::fsm::ProtocolFsm`] for the 3-strike fallback
//! rule: 3 consecutive v2 failures trigger a downgrade
//! (WsV2 → HttpV2 → HttpV1).

use super::backoff::Backoff;
use crate::arena::ScratchArena;
use crate::config::Config;
use crate::http::{HttpErr, http_post};
use crate::monitor::{Monitor, generate_report};
use crate::protocol::fsm::{FailureKind, ProtocolFsm, ProtocolMode};
use crate::protocol::v2;
use crate::server::cf_access::CfAccess;
use crate::ws::{WsConnection, WsErr, WsMessage};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ═══════════════════════════════════════════════════════════════════════════
// Connection handle
// ═══════════════════════════════════════════════════════════════════════════

enum Connection {
    Ws(Box<WsConnection>),
    Http,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tick error
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug)]
#[allow(dead_code)]
enum TickErr {
    Ws(WsErr),
    Http(HttpErr),
    Other(String),
}

impl From<WsErr> for TickErr {
    fn from(e: WsErr) -> Self {
        TickErr::Ws(e)
    }
}

impl From<HttpErr> for TickErr {
    fn from(e: HttpErr) -> Self {
        TickErr::Http(e)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Never-returning reconnection loop.
///
/// 1. Initialises TLS (fatal on failure, exit 1).
/// 2. Uploads basic system info (non-fatal).
/// 3. Enters the connect → tick → reconnect cycle, driven by
///    [`ProtocolFsm`] and exponential [`Backoff`].
///    Periodic basic-info refresh runs every
///    `config.info_report_interval` minutes via [`super::update_basic_info`].
pub fn run_reconnection_loop(config: &Config) -> ! {
    // Step 1: Initialise TLS config (fatal on failure).
    let tls_cfg = match crate::tls::make_tls_config(config) {
        Ok(cfg) => Arc::new(cfg),
        Err(e) => {
            eprintln!("[komari] ERROR: TLS config init failed: {}", e);
            std::process::exit(1);
        }
    };

    // Step 2: Upload basic system info at startup (non-fatal).
    let dial = crate::proxy::Dialer::from_config(config);
    if let Err(e) = super::update_basic_info(config, &tls_cfg, &dial) {
        eprintln!("[komari] WARN: initial basic info upload failed: {}", e);
    }

    let mut fsm = ProtocolFsm::new(config.protocol_version);
    let mut backoff = Backoff::new(config.max_retries, config.reconnect_interval);
    let mut monitor = Monitor::new();
    let mut arena = ScratchArena::new();
    let mut last_info_refresh = Instant::now();
    let info_interval = Duration::from_secs(config.info_report_interval * 60);

    loop {
        // Periodic basic info refresh.
        if last_info_refresh.elapsed() >= info_interval {
            if let Err(e) = super::update_basic_info(config, &tls_cfg, &dial) {
                eprintln!("[komari] WARN: periodic basic info refresh failed: {}", e);
            }
            last_info_refresh = Instant::now();
        }

        // Do NOT on_reconnect() here — connect failures must accumulate to
        // trigger the 3-strike downgrade (WsV2 → WsV1 → HttpV2 → HttpV1).
        let conn = match connect_with_fsm(&fsm, config, &tls_cfg, &dial) {
            Ok(conn) => {
                fsm.on_success();
                backoff.reset();
                conn
            }
            Err(e) => {
                let kind = classify_ws_failure(&e);
                let downgraded = fsm.on_failure(kind);
                eprintln!(
                    "[komari] ERROR: connect failed ({:?}/{:?}{}): {:?}",
                    fsm.mode(),
                    kind,
                    if downgraded { " -- DOWNSHIFTED" } else { "" },
                    e
                );
                if backoff.exhausted() {
                    eprintln!(
                        "[komari] ERROR: max retries ({}) exhausted -- exiting",
                        backoff.max_retries
                    );
                    std::process::exit(1);
                }
                std::thread::sleep(backoff.next_delay());
                continue;
            }
        };

        if let Err(e) = run_tick_loop(conn, &mut fsm, &mut monitor, &mut arena, config, &tls_cfg, &dial) {
            let kind = classify_tick_failure(&e);
            let downgraded = fsm.on_failure(kind);
            eprintln!(
                "[komari] ERROR: tick loop error ({:?}/{:?}{}): {:?}",
                fsm.mode(),
                kind,
                if downgraded { " -- DOWNSHIFTED" } else { "" },
                e
            );
        }

        if backoff.exhausted() {
            eprintln!(
                "[komari] ERROR: max retries ({}) exhausted -- exiting",
                backoff.max_retries
            );
            std::process::exit(1);
        }
        std::thread::sleep(backoff.next_delay());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// connect_with_fsm
// ═══════════════════════════════════════════════════════════════════════════

fn connect_with_fsm(
    fsm: &ProtocolFsm,
    config: &Config,
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
) -> Result<Connection, WsErr> {
    let cf_access = CfAccess::from_config(config);

    match fsm.mode() {
        ProtocolMode::WsV2 | ProtocolMode::WsV1 => {
            let ws_path = match fsm.mode() {
                ProtocolMode::WsV2 => "/api/clients/v2/rpc",
                ProtocolMode::WsV1 => "/api/clients/report",
                _ => unreachable!(),
            };
            let mut ws_headers: Vec<(String, String)> = Vec::new();
            if let Some(ref cf) = cf_access {
                cf.inject_ws_headers(&mut ws_headers);
            }
            let conn = WsConnection::connect(
                &config.endpoint,
                ws_path,
                &config.token,
                Arc::clone(tls_cfg),
                Duration::from_secs(30),
                &ws_headers,
                dial,
            )?;
            Ok(Connection::Ws(Box::new(conn)))
        }
        ProtocolMode::HttpV2 | ProtocolMode::HttpV1 => {
            let url = build_http_url(config, fsm.mode());
            let mut http_headers: Vec<(String, String)> = Vec::new();
            if let Some(ref cf) = cf_access {
                cf.inject_http_headers(&mut http_headers);
            }
            http_post(
                &url,
                b"{}",
                "application/json",
                None,
                &http_headers,
                tls_cfg,
                dial,
            )
            .map_err(|e| WsErr::Io(format!("HTTP probe failed: {}", e)))?;
            Ok(Connection::Http)
        }
    }
}

fn build_http_url(config: &Config, mode: ProtocolMode) -> String {
    let base = config.endpoint.trim_end_matches('/');
    let path = match mode {
        ProtocolMode::HttpV2 => "/api/clients/v2/rpc",
        ProtocolMode::HttpV1 => "/api/clients/report",
        _ => unreachable!(),
    };
    format!(
        "{}{}?token={}",
        base,
        path,
        crate::ws::url_encode(&config.token)
    )
}

/// Build the CF Access extra headers Vec for HTTP requests.
/// Returns empty Vec when CF Access is not configured.
fn build_http_cf_headers(config: &Config) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(ref cf) = CfAccess::from_config(config) {
        cf.inject_http_headers(&mut headers);
    }
    headers
}

// ═══════════════════════════════════════════════════════════════════════════
// run_tick_loop — main 1-second monitoring loop
// ═══════════════════════════════════════════════════════════════════════════

fn run_tick_loop(
    mut conn: Connection,
    fsm: &mut ProtocolFsm,
    monitor: &mut Monitor,
    arena: &mut ScratchArena,
    config: &Config,
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
) -> Result<(), TickErr> {
    let mut last_heartbeat = Instant::now();

    loop {
        // 1. Collect metrics.
        let report = generate_report(monitor, arena, config);

        // 2. Send report.
        match (&mut conn, fsm.mode()) {
            (Connection::Ws(ws), ProtocolMode::WsV2) => {
                let notif = v2::new_notification(v2::METHOD_AGENT_REPORT, report);
                ws.send_text(&notif)?;
            }
            (Connection::Ws(ws), ProtocolMode::WsV1) => {
                // v1: flat JSON report, no JSON-RPC wrapper.
                ws.send_text(report)?;
            }
            (Connection::Http, ProtocolMode::HttpV2) => {
                let req = v2::new_request("0", v2::METHOD_AGENT_REPORT, report);
                let resp = http_post(
                    &build_http_url(config, ProtocolMode::HttpV2),
                    &req,
                    "application/json",
                    None,
                    &build_http_cf_headers(config),
                    tls_cfg,
                    dial,
                )?;
                if !resp.body.is_empty() {
                    dispatch_server_message(&resp.body, config);
                }
            }
            (Connection::Http, ProtocolMode::HttpV1) => {
                let resp = http_post(
                    &build_http_url(config, ProtocolMode::HttpV1),
                    report,
                    "application/json",
                    None,
                    &build_http_cf_headers(config),
                    tls_cfg,
                    dial,
                )?;
                if !resp.body.is_empty() {
                    dispatch_server_message(&resp.body, config);
                }
            }
            _ => return Err(TickErr::Other("mode/connection mismatch".into())),
        }

        // 3. Read server messages (WS: non-blocking poll).
        if let Connection::Ws(ws) = &mut conn {
            let _ = ws
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(100)));
            match ws.read_message() {
                Ok(Some(WsMessage::Text(data))) => dispatch_server_message(&data, config),
                Ok(Some(WsMessage::Ping(data))) => {
                    ws.send_pong(&data)?;
                }
                Ok(Some(WsMessage::Close)) => {
                    eprintln!("[komari] server sent close frame");
                    return Ok(());
                }
                Ok(Some(WsMessage::Binary(_))) | Ok(Some(WsMessage::Pong(_))) => {}
                Ok(None) => return Err(TickErr::Other("connection closed by server".into())),
                Err(e) => {
                    if !is_timeout(&e) {
                        return Err(e.into());
                    }
                }
            }
            let _ = ws.get_ref().set_read_timeout(Some(Duration::from_secs(30)));
        }

        // 4. Heartbeat every 30 s.
        if last_heartbeat.elapsed() >= Duration::from_secs(30) {
            if let Connection::Ws(ws) = &mut conn {
                ws.send_ping()?;
            }
            last_heartbeat = Instant::now();
        }

        // 5. Sleep until next tick.
        std::thread::sleep(Duration::from_secs_f64(config.interval));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Server message dispatch
// ═══════════════════════════════════════════════════════════════════════════

fn dispatch_server_message(data: &[u8], config: &Config) {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[komari] WARN: non-UTF8 server message, ignoring");
            return;
        }
    };
    if text.contains("\"jsonrpc\":\"2.0\"") || text.contains("\"jsonrpc\": \"2.0\"") {
        dispatch_v2_event(data, config);
    } else if text.contains("\"message\":\"exec\"") || text.contains("\"task_id\"") {
        eprintln!("[komari] [stub] exec request: {}", text);
    } else if text.contains("\"message\":\"ping\"") || text.contains("\"ping_task_id\"") {
        eprintln!("[komari] [stub] ping request: {}", text);
    } else if text.contains("\"message\":\"terminal\"") || text.contains("\"request_id\"") {
        eprintln!("[komari] [stub] terminal request: {}", text);
    }
}

fn dispatch_v2_event(data: &[u8], _config: &Config) {
    let text = std::str::from_utf8(data).unwrap_or("");
    if text.contains("\"agent.exec\"") {
        eprintln!("[komari] [stub] v2 exec event received");
    } else if text.contains("\"agent.ping\"") {
        eprintln!("[komari] [stub] v2 ping event received");
    } else if text.contains("\"agent.terminal") {
        eprintln!("[komari] [stub] v2 terminal event received");
    } else if text.contains("\"agent.message\"") || text.contains("\"agent.event\"") {
        eprintln!("[komari] [stub] v2 message/event: {}", text);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Failure classification
// ═══════════════════════════════════════════════════════════════════════════

fn classify_ws_failure(e: &WsErr) -> FailureKind {
    match e {
        WsErr::Handshake(s) if s.contains("401") || s.contains("403") => {
            FailureKind::HttpStatus(401)
        }
        WsErr::Tls(_) => FailureKind::WsConnect,
        WsErr::Handshake(s) if s.contains("404") || s.contains("405") => {
            FailureKind::HttpStatus(404)
        }
        WsErr::Io(_) | WsErr::Dns(_) => FailureKind::WsConnect,
        _ => FailureKind::WsConnect,
    }
}

fn classify_tick_failure(e: &TickErr) -> FailureKind {
    match e {
        TickErr::Ws(e) => classify_ws_failure(e),
        TickErr::Http(HttpErr::Parse(_)) | TickErr::Http(HttpErr::Tls(_)) => FailureKind::HttpPost,
        TickErr::Http(_) | TickErr::Other(_) => FailureKind::HttpPost,
    }
}

fn is_timeout(e: &WsErr) -> bool {
    matches!(e, WsErr::Io(s) if {
        let s = s.to_lowercase();
        s.contains("timed out")
            || s.contains("would block")
            || s.contains("temporarily unavailable") // EAGAIN/EWOULDBLOCK on Linux
    })
}
