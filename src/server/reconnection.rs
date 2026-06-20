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

    // Step 2: Build the network dialer.
    //
    // Basic info is uploaded AFTER the first successful connection — the
    // server must register this client (via the WS handshake) before it can
    // accept basicInfo updates, so uploading eagerly here yields HTTP 500.
    let dial = crate::proxy::Dialer::from_config(config);

    let mut fsm = ProtocolFsm::new(config.protocol_version);
    let mut backoff = Backoff::new(config.max_retries, config.reconnect_interval);
    let mut monitor = Monitor::new_with_config(config);
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
                // Client is now registered server-side — upload basic info.
                // Non-fatal: the periodic refresh retries on failure.
                if let Err(e) = super::update_basic_info(config, &tls_cfg, &dial) {
                    eprintln!("[komari] WARN: post-connect basic info upload failed: {}", e);
                }
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
                let (body, encoding) = gzip_if_enabled(&req, config);
                let resp = http_post(
                    &build_http_url(config, ProtocolMode::HttpV2),
                    &body,
                    "application/json",
                    encoding,
                    &build_http_cf_headers(config),
                    tls_cfg,
                    dial,
                )?;
                if !resp.body.is_empty() {
                    dispatch_server_message(&resp.body, config, dial, tls_cfg, None, fsm.mode());
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
                    dispatch_server_message(&resp.body, config, dial, tls_cfg, None, fsm.mode());
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
                Ok(Some(WsMessage::Text(data))) => {
                    dispatch_server_message(&data, config, dial, tls_cfg, Some(ws), fsm.mode())
                }
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

fn dispatch_server_message(
    data: &[u8],
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    ws: Option<&mut WsConnection>,
    mode: ProtocolMode,
) {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[komari] WARN: non-UTF8 server message, ignoring");
            return;
        }
    };

    if text.contains("\"jsonrpc\"") {
        // v2 JSON-RPC 2.0
        let method = super::task::extract_json_method(data).unwrap_or_default();
        match method.as_str() {
            "agent.exec" => {
                let task_id =
                    super::task::extract_json_string(data, "task_id").unwrap_or_default();
                let command =
                    super::task::extract_json_string(data, "command").unwrap_or_default();
                handle_exec_task(config, dial, tls_cfg, &task_id, &command);
            }
            "agent.ping" => {
                if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                    handle_ping_task(ws, mode, tid, &pt, &tgt);
                }
            }
            "agent.terminal.request" => {
                eprintln!(
                    "[komari] terminal request received (enable 'terminal' feature to handle)"
                );
            }
            "agent.message" | "agent.event" => {
                eprintln!("[komari] server message/event: {}", abbreviate(text));
            }
            "" => {
                eprintln!("[komari] v2 message with no method: {}", abbreviate(text));
            }
            other => {
                eprintln!("[komari] unhandled v2 method '{other}'");
            }
        }
    } else {
        // v1 flat JSON: {"message":"exec", ...}
        let msg = super::task::extract_json_string(data, "message").unwrap_or_default();
        match msg.as_str() {
            "exec" => {
                let task_id =
                    super::task::extract_json_string(data, "task_id").unwrap_or_default();
                let command =
                    super::task::extract_json_string(data, "command").unwrap_or_default();
                handle_exec_task(config, dial, tls_cfg, &task_id, &command);
            }
            "ping" => {
                if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                    handle_ping_task(ws, mode, tid, &pt, &tgt);
                }
            }
            "terminal" => {
                eprintln!(
                    "[komari] terminal request received (enable 'terminal' feature to handle)"
                );
            }
            _ if !text.trim().is_empty() => {
                eprintln!("[komari] unhandled v1 message: {}", abbreviate(text));
            }
            _ => {}
        }
    }
}

/// Extract `(task_id, ping_type, target)` from a ping task message, accepting
/// both v1 (`ping_task_id`/`ping_type`/`ping_target`) and v2
/// (`taskId`/`pingType`/`target`) field names.
fn extract_ping_fields(data: &[u8]) -> Option<(i64, String, String)> {
    let tid = super::task::extract_json_number(data, "taskId")
        .or_else(|| super::task::extract_json_number(data, "task_id"))
        .or_else(|| super::task::extract_json_number(data, "ping_task_id"))?;
    let ping_type = super::task::extract_json_string(data, "pingType")
        .or_else(|| super::task::extract_json_string(data, "ping_type"))?;
    let target = super::task::extract_json_string(data, "target")
        .or_else(|| super::task::extract_json_string(data, "ping_target"))?;
    Some((tid, ping_type, target))
}

/// Execute a remote command and upload its result via HTTP POST to
/// `/api/clients/task/result` (parity with Go `executeCommand` + result upload).
fn handle_exec_task(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    task_id: &str,
    command: &str,
) {
    if task_id.is_empty() {
        eprintln!("[komari] exec request without task_id, ignoring");
        return;
    }
    eprintln!("[komari] exec task {task_id}: {}", abbreviate(command));
    let result = super::task::execute_exec(command, config.disable_web_ssh);
    let body = super::task::build_task_result(task_id, &result.output, result.exit_code);
    if let Err(e) = upload_task_result(config, dial, tls_cfg, &body) {
        eprintln!("[komari] WARN: task/result upload failed: {e}");
    }
}

/// POST a task result body to the task/result endpoint.
fn upload_task_result(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    body: &[u8],
) -> Result<(), String> {
    let base = config.endpoint.trim_end_matches('/');
    let token = crate::ws::url_encode(&config.token);
    let url = format!("{base}/api/clients/task/result?token={token}");
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(ref cf) = CfAccess::from_config(config) {
        cf.inject_http_headers(&mut headers);
    }
    match http_post(&url, body, "application/json", None, &headers, tls_cfg, dial) {
        Ok(r) if r.status_code == 200 => Ok(()),
        Ok(r) => Err(format!("task/result returned HTTP {}", r.status_code)),
        Err(e) => Err(format!("task/result upload error: {e}")),
    }
}

/// Run a server-requested ping and send the result back over the WebSocket.
/// When no WS is available (HTTP fallback transport), the result is logged
/// and dropped — Go behaves the same (ping results travel over WS).
fn handle_ping_task(
    ws: Option<&mut WsConnection>,
    mode: ProtocolMode,
    task_id: i64,
    ping_type: &str,
    target: &str,
) {
    eprintln!("[komari] ping task {task_id}: {ping_type} -> {target}");
    let result = super::task::handle_ping(ping_type, target, None);
    let is_v2 = matches!(mode, ProtocolMode::WsV2 | ProtocolMode::HttpV2);
    let payload = result.build_payload(task_id as u64, if is_v2 { 2 } else { 1 });
    if let Some(ws) = ws {
        if let Err(e) = ws.send_text(&payload) {
            eprintln!("[komari] WARN: failed to send ping result: {e:?}");
        }
    } else {
        eprintln!("[komari] ping result produced but no WS available (HTTP mode)");
    }
}

/// Optionally gzip-compress a payload for the v2 HTTP POST path.
///
/// Returns `(body, encoding)` where `encoding` is `Some("gzip")` when
/// compression is enabled, the payload is large enough to be worth it, and
/// compression succeeded. Small payloads (< 64 B) skip compression — the gzip
/// overhead would exceed any savings.
fn gzip_if_enabled(body: &[u8], config: &Config) -> (Vec<u8>, Option<&'static str>) {
    if config.disable_compression || body.len() < 64 {
        return (body.to_vec(), None);
    }
    match crate::gzip::gzip_compress(body) {
        Ok(compressed) => (compressed, Some("gzip")),
        Err(_) => (body.to_vec(), None),
    }
}

/// Truncate a string for logging, never splitting a UTF-8 codepoint.
fn abbreviate(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
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
