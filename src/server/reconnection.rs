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
use crate::http::{HttpErr, http_get, http_post, http_post_timeout};
use crate::monitor::{Monitor, generate_report};
use crate::protocol::fsm::{FailureKind, ProtocolFsm, ProtocolMode};
use crate::protocol::v2;
use crate::ws::{WsConnection, WsErr, WsMessage};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bumped whenever the v2 HTTP pull session changes ownership; stale pull
/// threads exit on their next iteration.
static HTTP_V2_GENERATION: AtomicU64 = AtomicU64::new(0);

/// A session shorter than this does not prove the current mode is healthy
/// (connect succeeded but the tick loop died immediately — e.g. server or
/// middlebox closed the WS right after upgrade).
const MIN_HEALTHY_SESSION: Duration = Duration::from_secs(30);

/// v2 event bookkeeping shared between the tick loop (report piggyback) and
/// the pull thread (long-poll). Events can arrive twice until the server
/// receives our ack, so dispatches are deduped by event id while acks are
/// re-sent idempotently on every sighting.
#[derive(Default)]
struct V2EventBus {
    pending_acks: Vec<String>,
    seen: Vec<String>,
}

impl V2EventBus {
    const SEEN_CAP: usize = 256;

    /// Record `id`; returns true when the event was already dispatched.
    fn mark_seen(&mut self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        if self.seen.iter().any(|s| s == id) {
            return true;
        }
        if self.seen.len() >= Self::SEEN_CAP {
            self.seen.remove(0);
        }
        self.seen.push(id.to_string());
        false
    }

    fn drain_acks(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_acks)
    }
}

#[cfg(feature = "terminal")]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Cap concurrent interactive terminal sessions (PTY threads).
#[cfg(feature = "terminal")]
static TERMINAL_SESSIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "terminal")]
const MAX_TERMINAL_SESSIONS: usize = 2;

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
    // Owned runtime config so we can auto-disable WS compression after a
    // permessage-deflate inflate failure (reconnect without compression).
    let mut runtime_cfg = config.clone();

    // Step 1: Initialise TLS config (fatal on failure).
    let tls_cfg = match crate::tls::make_tls_config(&runtime_cfg) {
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
    let dial = crate::proxy::Dialer::from_config(&runtime_cfg);

    let mut fsm = ProtocolFsm::new(runtime_cfg.protocol_version, runtime_cfg.http_only);
    let mut backoff = Backoff::new(runtime_cfg.max_retries, runtime_cfg.reconnect_interval);
    let mut monitor = Monitor::new_with_config(&runtime_cfg);
    let mut arena = ScratchArena::new();
    let mut last_info_refresh = Instant::now();
    let info_interval = Duration::from_secs(runtime_cfg.info_report_interval * 60);
    let bus = Arc::new(Mutex::new(V2EventBus::default()));
    // Periodic re-probe of the initial (preferred) protocol mode so a
    // historical downgrade does not pin the agent to a fallback forever.
    let mut last_reprobe = Instant::now();

    eprintln!(
        "[komari] capabilities: {}",
        agent_capabilities(&runtime_cfg).join(",")
    );

    loop {
        // Periodic basic info refresh.
        if last_info_refresh.elapsed() >= info_interval {
            if let Err(e) = super::update_basic_info(&runtime_cfg, &tls_cfg, &dial) {
                eprintln!("[komari] WARN: periodic basic info refresh failed: {}", e);
            }
            last_info_refresh = Instant::now();
        }

        // Do NOT on_reconnect() here — connect failures must accumulate to
        // trigger the 3-strike downgrade (WsV2 → WsV1 → HttpV2 → HttpV1).
        // Exception: every 10 min in a fallback mode, re-probe the preferred
        // mode once (self-heal after server upgrade / network repair).
        if fsm.mode() != fsm.initial_mode() && last_reprobe.elapsed() >= Duration::from_secs(600) {
            eprintln!(
                "[komari] re-probing preferred protocol mode {:?} (currently {:?})",
                fsm.initial_mode(),
                fsm.mode()
            );
            fsm.on_reconnect();
            last_reprobe = Instant::now();
        }
        let conn = match connect_with_fsm(&fsm, &runtime_cfg, &tls_cfg, &dial) {
            Ok(conn) => {
                // Do NOT reset FSM failure counters here: a connection that
                // dies within seconds of the handshake (e.g. middlebox kills
                // the WS upgrade) must still accumulate strikes, otherwise
                // the agent flaps forever in a mode that can connect but
                // cannot hold a session. Counters reset only after a session
                // survives MIN_HEALTHY_SESSION (see below).
                backoff.reset();
                // Client is now registered server-side — upload basic info.
                // Non-fatal: the periodic refresh retries on failure.
                if let Err(e) = super::update_basic_info(&runtime_cfg, &tls_cfg, &dial) {
                    eprintln!(
                        "[komari] WARN: post-connect basic info upload failed: {}",
                        e
                    );
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
                        "[komari] WARN: max retries ({}) exhausted -- continue retry loop (Go parity: agent never exits)",
                        backoff.max_retries
                    );
                }
                std::thread::sleep(backoff.next_delay());
                continue;
            }
        };

        let session_start = Instant::now();
        if let Err(e) = run_tick_loop(
            conn,
            &mut fsm,
            &mut monitor,
            &mut arena,
            &runtime_cfg,
            &tls_cfg,
            &dial,
            Arc::clone(&bus),
        ) {
            // Auto-degrade: permessage-deflate inflate bugs force a reconnect
            // without compression instead of flapping forever.
            if is_deflate_failure(&e) && !runtime_cfg.disable_compression {
                eprintln!(
                    "[komari] WARN: permessage-deflate failed — disabling compression for subsequent reconnects"
                );
                runtime_cfg.disable_compression = true;
            }
            let kind = classify_tick_failure(&e);
            let downgraded = fsm.on_failure(kind);
            eprintln!(
                "[komari] ERROR: tick loop error ({:?}/{:?}{}): {:?}",
                fsm.mode(),
                kind,
                if downgraded { " -- DOWNSHIFTED" } else { "" },
                e
            );
            if session_start.elapsed() >= MIN_HEALTHY_SESSION {
                // The session held long enough to prove this mode works.
                fsm.on_success();
            }
            // Terminal-mode escalation: when the v1 endpoint is gone for good
            // (server upgraded past v1 — Komari 1.5.0 removed /api/clients/report),
            // climbing back to v2 is the only way to recover. Without this the
            // agent retries a dead endpoint forever (2026-09-18 fleet blackout).
            if fsm.is_terminal() && fsm.consecutive_failures() >= ProtocolFsm::FALLBACK_THRESHOLD {
                eprintln!(
                    "[komari] HttpV1 unreachable — escalating back to {:?}",
                    fsm.initial_mode()
                );
                fsm.on_reconnect();
            }
        }

        if backoff.exhausted() {
            eprintln!(
                "[komari] WARN: max retries ({}) exhausted -- continue retry loop (Go parity: agent never exits)",
                backoff.max_retries
            );
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
    match fsm.mode() {
        ProtocolMode::WsV2 | ProtocolMode::WsV1 => {
            let ws_path = match fsm.mode() {
                ProtocolMode::WsV2 => "/api/clients/v2/rpc",
                ProtocolMode::WsV1 => "/api/clients/report",
                _ => unreachable!(),
            };
            let conn = WsConnection::connect(
                &config.endpoint,
                ws_path,
                &config.token,
                Arc::clone(tls_cfg),
                Duration::from_secs(30),
                &[],
                dial,
                !config.disable_compression,
            )?;
            Ok(Connection::Ws(Box::new(conn)))
        }
        ProtocolMode::HttpV2 | ProtocolMode::HttpV1 => {
            let url = build_http_url(config, fsm.mode());
            http_post(&url, b"{}", "application/json", None, &[], tls_cfg, dial)
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpPingTask {
    id: u64,
    ping_type: String,
    target: String,
    interval_secs: u64,
}

fn poll_http_ping_tasks(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    last_run: &mut HashMap<u64, Instant>,
) {
    let base = config.endpoint.trim_end_matches('/');
    let token = crate::ws::url_encode(&config.token);
    let url = format!("{base}/api/clients/ping/tasks?token={token}");
    let resp = match http_get(&url, &[], tls_cfg, dial) {
        Ok(resp) if resp.status_code == 200 => resp,
        Ok(resp) => {
            eprintln!(
                "[komari] WARN: ping task poll returned HTTP {}",
                resp.status_code
            );
            return;
        }
        Err(e) => {
            eprintln!("[komari] WARN: ping task poll failed: {e}");
            return;
        }
    };

    let tasks = parse_http_ping_tasks(&resp.body);
    let now = Instant::now();
    for task in tasks {
        let due = match last_run.get(&task.id) {
            Some(prev) => prev.elapsed() >= Duration::from_secs(task.interval_secs.max(1)),
            None => true,
        };
        if !due {
            continue;
        }
        last_run.insert(task.id, now);
        upload_http_ping_result(config, dial, tls_cfg, &task);
    }
}

fn upload_http_ping_result(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    task: &HttpPingTask,
) {
    run_ping_and_upload_v2(
        config,
        dial,
        tls_cfg,
        task.id as i64,
        &task.ping_type,
        &task.target,
    );
}

/// Execute one ping task and upload the result as a v2 `agent.pingResult`
/// notification (shared by the legacy HTTP poll path and v2 event dispatch).
fn run_ping_and_upload_v2(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    task_id: i64,
    ping_type: &str,
    target: &str,
) {
    eprintln!("[komari] ping task {task_id}: {ping_type} -> {target}");
    let result = super::task::handle_ping(ping_type, target, None);
    let id = if task_id < 0 { 0 } else { task_id as u64 };
    let params = result.build_payload(id, 2);
    let payload = v2::new_notification(v2::METHOD_AGENT_PING_RESULT, &params);
    match http_post(
        &build_http_url(config, ProtocolMode::HttpV2),
        &payload,
        "application/json",
        None,
        &[],
        tls_cfg,
        dial,
    ) {
        Ok(resp) if resp.status_code == 200 => {}
        Ok(resp) => eprintln!(
            "[komari] WARN: ping result upload returned HTTP {}",
            resp.status_code
        ),
        Err(e) => eprintln!("[komari] WARN: ping result upload failed: {e}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// v2 event intake (report piggyback + pull long-poll)
// ═══════════════════════════════════════════════════════════════════════════

/// Wrap a flat monitoring report in the v2 `agent.report` params envelope:
/// `{"report": <report>, "ack_event_ids": [...]}`.
fn wrap_report_params(report: &[u8], ack_ids: &[String]) -> Vec<u8> {
    let mut v = Vec::with_capacity(report.len() + 48 + ack_ids.len() * 36);
    v.extend_from_slice(b"{\"report\":");
    v.extend_from_slice(report);
    if !ack_ids.is_empty() {
        v.extend_from_slice(b",\"ack_event_ids\":[");
        for (i, id) in ack_ids.iter().enumerate() {
            if i > 0 {
                v.push(b',');
            }
            v.push(b'"');
            v.extend_from_slice(id.as_bytes());
            v.push(b'"');
        }
        v.push(b']');
    }
    v.push(b'}');
    v
}

/// Build `agent.pull` params: capabilities + drained ack ids.
fn build_pull_params(caps: &[&str], ack_ids: &[String]) -> Vec<u8> {
    let mut v = Vec::with_capacity(96 + caps.len() * 12 + ack_ids.len() * 36);
    v.extend_from_slice(b"{\"capabilities\":[");
    for (i, c) in caps.iter().enumerate() {
        if i > 0 {
            v.push(b',');
        }
        v.push(b'"');
        v.extend_from_slice(c.as_bytes());
        v.push(b'"');
    }
    v.push(b']');
    if !ack_ids.is_empty() {
        v.extend_from_slice(b",\"ack_event_ids\":[");
        for (i, id) in ack_ids.iter().enumerate() {
            if i > 0 {
                v.push(b',');
            }
            v.push(b'"');
            v.extend_from_slice(id.as_bytes());
            v.push(b'"');
        }
        v.push(b']');
    }
    v.push(b'}');
    v
}

/// Split the top-level objects of the `"events"` array in a v2 RPC response.
/// Tolerates the absent/empty array; string-aware so braces inside string
/// values do not break the split.
fn extract_event_objects(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(pos) = body.find("\"events\"") else {
        return out;
    };
    let Some(arr) = body[pos..].find('[').map(|i| pos + i) else {
        return out;
    };
    let mut depth = 0i32;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in body[arr..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(arr + idx);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = start.take()
                {
                    out.push(body[s..=arr + idx].to_string());
                }
            }
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    out
}

/// Handle the events array of a v2 RPC response: ack bookkeeping + dedup,
/// then dispatch each fresh event.
fn process_v2_response_events(
    body: &[u8],
    bus: &Arc<Mutex<V2EventBus>>,
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
) {
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(_) => return,
    };
    let events = extract_event_objects(text);
    if events.is_empty() {
        return;
    }
    let mut fresh = Vec::new();
    if let Ok(mut b) = bus.lock() {
        for ev in &events {
            let id = super::task::extract_json_string(ev.as_bytes(), "id").unwrap_or_default();
            if !id.is_empty() {
                // Re-ack on every sighting: the server re-sends until acked.
                b.pending_acks.push(id.clone());
            }
            if b.mark_seen(&id) {
                continue;
            }
            fresh.push(ev.clone());
        }
    }
    for ev in fresh {
        dispatch_v2_event(&ev, config, dial, tls_cfg);
    }
}

/// Dispatch one v2 event object (`{"id","method","params",...}`).
fn dispatch_v2_event(
    event_json: &str,
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
) {
    let data = event_json.as_bytes();
    let method = super::task::extract_json_string(data, "method").unwrap_or_default();
    match method.as_str() {
        "agent.ping" => {
            if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                run_ping_and_upload_v2(config, dial, tls_cfg, tid, &pt, &tgt);
            }
        }
        "agent.exec" => {
            let task_id = super::task::extract_json_string(data, "task_id").unwrap_or_default();
            let command = super::task::extract_json_string(data, "command").unwrap_or_default();
            handle_exec_task(config, dial, tls_cfg, &task_id, &command, true);
        }
        "agent.terminal.request" => {
            let request_id =
                super::task::extract_json_string(data, "request_id").unwrap_or_default();
            handle_terminal_request(config, dial, tls_cfg, &request_id);
        }
        "agent.message" | "agent.event" => {
            eprintln!("[komari] server message/event: {}", abbreviate(event_json));
        }
        other => {
            eprintln!("[komari] unhandled v2 event method '{other}'");
        }
    }
}

/// Long-poll loop for HTTP v2 mode: `POST agent.pull` blocks server-side up
/// to 25 s and returns queued events (ping/exec/terminal/message). Mirrors
/// the Go agent's `runV2PullLoop` goroutine — the single-threaded tick loop
/// cannot afford a 25 s blocking call, so this runs on a dedicated thread.
fn spawn_pull_thread(
    config: &Config,
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
    bus: Arc<Mutex<V2EventBus>>,
) {
    let generation = HTTP_V2_GENERATION
        .fetch_add(1, AtomicOrdering::SeqCst)
        .wrapping_add(1);
    eprintln!("[komari] v2 pull thread spawning (gen {generation})");
    let url = build_http_url(config, ProtocolMode::HttpV2);
    let caps: Vec<String> = agent_capabilities(config)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let config = config.clone();
    let dial = dial.clone();
    let tls_cfg = Arc::clone(tls_cfg);
    let spawn = std::thread::Builder::new()
        .name("v2-pull".into())
        .spawn(move || {
            let caps: Vec<&str> = caps.iter().map(String::as_str).collect();
            loop {
                if HTTP_V2_GENERATION.load(AtomicOrdering::SeqCst) != generation {
                    return; // a newer pull thread owns the session
                }
                let acks = match bus.lock() {
                    Ok(mut b) => b.drain_acks(),
                    Err(_) => Vec::new(),
                };
                let params = build_pull_params(&caps, &acks);
                let req = v2::new_request("pull", v2::METHOD_AGENT_PULL, &params);
                match http_post_timeout(
                    &url,
                    &req,
                    "application/json",
                    None,
                    &[],
                    &tls_cfg,
                    &dial,
                    Duration::from_secs(35),
                ) {
                    Ok(resp) if resp.status_code == 200 => {
                        process_v2_response_events(&resp.body, &bus, &config, &dial, &tls_cfg);
                    }
                    Ok(resp) => {
                        eprintln!("[komari] WARN: v2 pull HTTP {}", resp.status_code);
                        std::thread::sleep(Duration::from_secs(config.reconnect_interval.max(1)));
                    }
                    Err(e) => {
                        eprintln!("[komari] WARN: v2 pull failed: {e}");
                        std::thread::sleep(Duration::from_secs(config.reconnect_interval.max(1)));
                    }
                }
            }
        });
    if let Err(e) = spawn {
        eprintln!("[komari] WARN: failed to spawn v2 pull thread: {e}");
    }
}

fn parse_http_ping_tasks(body: &[u8]) -> Vec<HttpPingTask> {
    let text = match std::str::from_utf8(body) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let mut tasks = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = start.take()
                    && let Some(task) = parse_http_ping_task_object(&text[s..=idx])
                {
                    tasks.push(task);
                }
            }
            _ => {}
        }
    }
    tasks
}

fn parse_http_ping_task_object(obj: &str) -> Option<HttpPingTask> {
    let raw_id = super::task::extract_json_number(obj.as_bytes(), "id")?;
    if raw_id < 0 {
        return None;
    }
    let id = raw_id as u64;
    let ping_type = super::task::extract_json_string(obj.as_bytes(), "type")?;
    let target = super::task::extract_json_string(obj.as_bytes(), "target")?;
    let interval_secs = super::task::extract_json_number(obj.as_bytes(), "interval")
        .unwrap_or(60)
        .max(1) as u64;
    Some(HttpPingTask {
        id,
        ping_type,
        target,
        interval_secs,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// run_tick_loop — main 1-second monitoring loop
// ═══════════════════════════════════════════════════════════════════════════

#[allow(clippy::too_many_arguments)]
fn run_tick_loop(
    mut conn: Connection,
    fsm: &mut ProtocolFsm,
    monitor: &mut Monitor,
    arena: &mut ScratchArena,
    config: &Config,
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
    bus: Arc<Mutex<V2EventBus>>,
) -> Result<(), TickErr> {
    let mut last_heartbeat = Instant::now();
    let mut last_http_ping_poll = Instant::now() - Duration::from_secs(10);
    let mut http_ping_last_run: HashMap<u64, Instant> = HashMap::new();

    // HTTP v2 mode needs the long-poll loop to receive server-pushed events
    // (ping tasks have a 3 s TTL server-side — report-piggyback alone would
    // miss nearly all of them). WS modes get pushes on the socket instead.
    if matches!(fsm.mode(), ProtocolMode::HttpV2) {
        spawn_pull_thread(config, tls_cfg, dial, Arc::clone(&bus));
    } else {
        // Kill any stale pull thread from a previous HttpV2 stint.
        HTTP_V2_GENERATION.fetch_add(1, AtomicOrdering::SeqCst);
    }

    loop {
        // 1. Collect metrics.
        let report = generate_report(monitor, arena, config);

        // 2. Send report.
        match (&mut conn, fsm.mode()) {
            (Connection::Ws(ws), ProtocolMode::WsV2) => {
                // v2 params MUST wrap the flat report in {"report": ...} —
                // the server binds params into ReportParams{report}; sending
                // the bare report returns success but ingests nothing
                // (silent data loss pre-0.4.0).
                let params = wrap_report_params(report, &[]);
                let notif = v2::new_notification(v2::METHOD_AGENT_REPORT, &params);
                ws.send_text(&notif)?;
            }
            (Connection::Ws(ws), ProtocolMode::WsV1) => {
                // v1: flat JSON report, no JSON-RPC wrapper.
                if config.debug_log {
                    eprintln!(
                        "[komari] DEBUG v1 report ({}B): {}",
                        report.len(),
                        std::str::from_utf8(report).unwrap_or("<non-utf8>")
                    );
                }
                ws.send_text(report)?;
            }
            (Connection::Http, ProtocolMode::HttpV2) => {
                let acks = match bus.lock() {
                    Ok(mut b) => b.drain_acks(),
                    Err(_) => Vec::new(),
                };
                let params = wrap_report_params(report, &acks);
                let req = v2::new_request("report", v2::METHOD_AGENT_REPORT, &params);
                let (body, encoding) = gzip_if_enabled(&req, config);
                let resp = http_post(
                    &build_http_url(config, ProtocolMode::HttpV2),
                    &body,
                    "application/json",
                    encoding,
                    &[],
                    tls_cfg,
                    dial,
                )?;
                // Status discipline: pre-0.4.0 ignored the status code, so a
                // rejected report (400/401/404) was indistinguishable from
                // success and the FSM never reacted.
                match resp.status_code {
                    200 => {}
                    404 => {
                        return Err(TickErr::Other("v2 rpc endpoint missing (HTTP 404)".into()));
                    }
                    code => {
                        return Err(TickErr::Other(format!("v2 report HTTP {code}")));
                    }
                }
                // The report response piggybacks queued server events.
                process_v2_response_events(&resp.body, &bus, config, dial, tls_cfg);
            }
            (Connection::Http, ProtocolMode::HttpV1) => {
                let resp = http_post(
                    &build_http_url(config, ProtocolMode::HttpV1),
                    report,
                    "application/json",
                    None,
                    &[],
                    tls_cfg,
                    dial,
                )?;
                if resp.status_code == 404 {
                    // v1 endpoint removed (Komari >= 1.5.0) — fail the tick so
                    // the FSM escalation logic can climb back to v2.
                    return Err(TickErr::Other(
                        "v1 report endpoint gone (HTTP 404); server likely upgraded".into(),
                    ));
                }
                // The v1 report response is normally a bare ack like
                // {"status":"success"}; only dispatch if it looks like a real
                // server-pushed message (task/exec/ping carry a "method" or
                // "id" field), to avoid log noise from the ack every tick.
                let body_str = std::str::from_utf8(&resp.body).unwrap_or("");
                if !body_str.is_empty()
                    && (body_str.contains("\"method\"") || body_str.contains("\"id\""))
                {
                    dispatch_server_message(&resp.body, config, dial, tls_cfg, None, fsm.mode());
                }
            }
            _ => return Err(TickErr::Other("mode/connection mismatch".into())),
        }

        // Legacy ping-task polling only makes sense against pre-1.5.0 servers
        // (HttpV1); v2 modes receive ping tasks as events (pull / piggyback).
        if matches!(conn, Connection::Http)
            && fsm.mode() == ProtocolMode::HttpV1
            && last_http_ping_poll.elapsed() >= Duration::from_secs(5)
        {
            poll_http_ping_tasks(config, dial, tls_cfg, &mut http_ping_last_run);
            last_http_ping_poll = Instant::now();
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
                let task_id = super::task::extract_json_string(data, "task_id").unwrap_or_default();
                let command = super::task::extract_json_string(data, "command").unwrap_or_default();
                handle_exec_task(config, dial, tls_cfg, &task_id, &command, true);
            }
            "agent.ping" => {
                if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                    handle_ping_task(ws, mode, tid, &pt, &tgt);
                }
            }
            "agent.terminal.request" => {
                let request_id =
                    super::task::extract_json_string(data, "request_id").unwrap_or_default();
                handle_terminal_request(config, dial, tls_cfg, &request_id);
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
                let task_id = super::task::extract_json_string(data, "task_id").unwrap_or_default();
                let command = super::task::extract_json_string(data, "command").unwrap_or_default();
                handle_exec_task(config, dial, tls_cfg, &task_id, &command, false);
            }
            "ping" => {
                if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                    handle_ping_task(ws, mode, tid, &pt, &tgt);
                }
            }
            "terminal" => {
                let request_id =
                    super::task::extract_json_string(data, "request_id").unwrap_or_default();
                handle_terminal_request(config, dial, tls_cfg, &request_id);
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

/// Handle a server-initiated interactive terminal request.
///
/// Mirrors Go `establishTerminalConnection` + `terminal.StartTerminal`:
///   1. Reject empty session id.
///   2. If the `terminal` feature is not compiled in, log and return.
///   3. If `disable_web_ssh` is set, dial the session WS only long enough to
///      tell the browser the axe is locked, then close.
///   4. Otherwise dial `/api/clients/terminal?id=<session>` on a **detached
///      thread** (PTY loop is blocking; must not stall the single-threaded
///      monitor tick) and run `terminal::start_terminal`.
///
/// Terminal requires a live agent control plane that can receive the push
/// (`agent.terminal.request` / v1 `"terminal"`). Pure `--http-only` agents
/// never get that push, so this path is only reachable over WS modes.
fn handle_terminal_request(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    request_id: &str,
) {
    if request_id.is_empty() {
        eprintln!("[komari] terminal request without request_id, ignoring");
        return;
    }

    #[cfg(not(feature = "terminal"))]
    {
        let _ = (config, dial, tls_cfg);
        eprintln!(
            "[komari] terminal request {request_id}: binary built without 'terminal' feature"
        );
    }

    #[cfg(feature = "terminal")]
    {
        let request_id = request_id.to_string();
        let endpoint = config.endpoint.clone();
        let token = config.token.clone();
        let disable_web_ssh = config.disable_web_ssh;
        let disable_compression = config.disable_compression;
        let dial = dial.clone();
        let tls_cfg = Arc::clone(tls_cfg);

        // Concurrency gate: refuse rather than unbounded PTY fork.
        let prev = TERMINAL_SESSIONS.fetch_add(1, Ordering::SeqCst);
        if prev >= MAX_TERMINAL_SESSIONS {
            TERMINAL_SESSIONS.fetch_sub(1, Ordering::SeqCst);
            eprintln!(
                "[komari] terminal request {request_id}: refused (max {MAX_TERMINAL_SESSIONS} concurrent sessions)"
            );
            return;
        }

        // Detach so the monitor tick keeps running while the PTY session lives.
        // The server holds the browser WS open for ~30s waiting for this dial.
        let spawn_result = std::thread::Builder::new()
            .name(format!("terminal-{request_id}"))
            .spawn(move || {
                let _guard = TerminalSessionGuard;
                establish_terminal_session(
                    &endpoint,
                    &token,
                    &request_id,
                    disable_web_ssh,
                    disable_compression,
                    &dial,
                    &tls_cfg,
                    &[],
                );
            });
        if let Err(e) = spawn_result {
            TERMINAL_SESSIONS.fetch_sub(1, Ordering::SeqCst);
            eprintln!("[komari] failed to spawn terminal thread: {e}");
        }
    }
}

/// Decrements [`TERMINAL_SESSIONS`] when a terminal worker exits.
#[cfg(feature = "terminal")]
struct TerminalSessionGuard;
#[cfg(feature = "terminal")]
impl Drop for TerminalSessionGuard {
    fn drop(&mut self) {
        TERMINAL_SESSIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Dial the terminal bridge and run (or refuse) the PTY session.
///
/// Path shape matches Go:
///   `ws(s)://<endpoint>/api/clients/terminal?token=<token>&id=<id>`
/// Our WS layer always appends `token=`; we only put `id` on the path.
#[cfg(feature = "terminal")]
#[allow(clippy::too_many_arguments)]
fn establish_terminal_session(
    endpoint: &str,
    token: &str,
    request_id: &str,
    disable_web_ssh: bool,
    disable_compression: bool,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    extra_headers: &[(String, String)],
) {
    // Encode session id for the query string (defence against odd chars).
    let path = format!(
        "/api/clients/terminal?id={}",
        crate::ws::url_encode(request_id)
    );
    eprintln!("[komari] terminal session {request_id}: dialing {path}");

    let mut ws = match WsConnection::connect(
        endpoint,
        &path,
        token,
        Arc::clone(tls_cfg),
        Duration::from_secs(30),
        extra_headers,
        dial,
        !disable_compression,
    ) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[komari] terminal session {request_id}: dial failed: {e}");
            return;
        }
    };

    if disable_web_ssh {
        // Parity with Go terminal.StartTerminal when DisableWebSsh is set:
        // surface a clear message to the browser, then close.
        let msg = b"\n\nWeb SSH is disabled. Enable it by running without --disable-web-ssh \
(and build with --features terminal). Remote exec is gated by the same flag.\n";
        let _ = ws.send_text(msg);
        let _ = ws.close();
        eprintln!("[komari] terminal session {request_id}: refused (disable_web_ssh)");
        return;
    }

    match crate::terminal::start_terminal(&mut ws) {
        Ok(()) => eprintln!("[komari] terminal session {request_id}: closed cleanly"),
        Err(e) => eprintln!("[komari] terminal session {request_id}: error: {e}"),
    }
}

/// Execute a remote command and upload its result via HTTP POST to
/// `/api/clients/task/result` (parity with Go `executeCommand` + result upload).
fn handle_exec_task(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    task_id: &str,
    command: &str,
    v2_mode: bool,
) {
    if task_id.is_empty() {
        eprintln!("[komari] exec request without task_id, ignoring");
        return;
    }
    eprintln!("[komari] exec task {task_id}: {}", abbreviate(command));
    let result = super::task::execute_exec(command, config.disable_exec);
    let body = if v2_mode {
        super::task::build_task_result_v2(task_id, &result.output, result.exit_code)
    } else {
        super::task::build_task_result(task_id, &result.output, result.exit_code)
    };
    if let Err(e) = upload_task_result(config, dial, tls_cfg, &body, v2_mode) {
        eprintln!("[komari] WARN: task result upload failed: {e}");
    }
}

/// POST a task result — v1 flat body to `/api/clients/task/result`, or a v2
/// `agent.taskResult` JSON-RPC notification to `/api/clients/v2/rpc`
/// (the v1 endpoint was removed in Komari 1.5.0).
fn upload_task_result(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    body: &[u8],
    v2_mode: bool,
) -> Result<(), String> {
    let url = if v2_mode {
        build_http_url(config, ProtocolMode::HttpV2)
    } else {
        let base = config.endpoint.trim_end_matches('/');
        let token = crate::ws::url_encode(&config.token);
        format!("{base}/api/clients/task/result?token={token}")
    };
    match http_post(&url, body, "application/json", None, &[], tls_cfg, dial) {
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
    let id = if task_id < 0 { 0 } else { task_id as u64 };
    let params = result.build_payload(id, if is_v2 { 2 } else { 1 });
    // v2 requires the JSON-RPC envelope — bare params parse as method=""
    // server-side and are dropped (method not found).
    let payload = if is_v2 {
        v2::new_notification(v2::METHOD_AGENT_PING_RESULT, &params)
    } else {
        params
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_report_params_wraps_flat_report() {
        let out = wrap_report_params(br#"{"cpu":{"usage":1.0}}"#, &[]);
        assert_eq!(out, br#"{"report":{"cpu":{"usage":1.0}}}"#.to_vec());
    }

    #[test]
    fn wrap_report_params_appends_ack_ids() {
        let out = wrap_report_params(b"{}", &["a1".to_string(), "b2".to_string()]);
        assert_eq!(
            out,
            br#"{"report":{},"ack_event_ids":["a1","b2"]}"#.to_vec()
        );
    }

    #[test]
    fn extract_event_objects_splits_array() {
        let body = r#"{"jsonrpc":"2.0","id":"report","result":{"status":"success","events":[{"id":"e1","method":"agent.ping","params":{"ping_task_id":1,"ping_type":"icmp","ping_target":"1.1.1.1"}},{"id":"e2","method":"agent.message","params":{"content":"hi } ] \"}"}}]}}"#;
        let events = extract_event_objects(body);
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("\"e1\""));
        assert!(events[1].contains("\"e2\""));
    }

    #[test]
    fn extract_event_objects_empty_or_missing() {
        assert!(extract_event_objects(r#"{"result":{"events":[]}}"#).is_empty());
        assert!(extract_event_objects(r#"{"result":{"status":"success"}}"#).is_empty());
    }

    #[test]
    fn build_pull_params_includes_caps_and_acks() {
        let out = build_pull_params(&["ping", "exec"], &["e1".to_string()]);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            r#"{"capabilities":["ping","exec"],"ack_event_ids":["e1"]}"#
        );
    }

    #[test]
    fn event_bus_dedups_and_drains() {
        let mut bus = V2EventBus::default();
        assert!(!bus.mark_seen("e1"));
        assert!(bus.mark_seen("e1"));
        bus.pending_acks.push("e1".to_string());
        assert_eq!(bus.drain_acks(), vec!["e1".to_string()]);
        assert!(bus.pending_acks.is_empty());
    }

    #[test]
    fn parses_http_ping_tasks_from_komari_response() {
        let body = br#"[{"id":1,"weight":1,"name":"fleet","clients":["u1"],"default_on":true,"type":"icmp","target":"1.1.1.1","interval":60}]"#;
        let tasks = parse_http_ping_tasks(body);
        assert_eq!(
            tasks,
            vec![HttpPingTask {
                id: 1,
                ping_type: "icmp".to_string(),
                target: "1.1.1.1".to_string(),
                interval_secs: 60,
            }]
        );
    }

    #[test]
    fn parse_http_ping_tasks_ignores_invalid_objects() {
        let body =
            br#"[{"name":"missing-id"},{"id":2,"type":"tcp","target":"example.com","interval":0}]"#;
        let tasks = parse_http_ping_tasks(body);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 2);
        assert_eq!(tasks[0].interval_secs, 1);
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

/// True when the tick failed because permessage-deflate inflate could not
/// decode a server frame (known class of interoperability bugs).
fn is_deflate_failure(e: &TickErr) -> bool {
    match e {
        TickErr::Ws(WsErr::Protocol(s)) => {
            let s = s.to_ascii_lowercase();
            s.contains("permessage-deflate") || s.contains("inflate")
        }
        _ => false,
    }
}

/// Capability strings advertised at startup (and for future agent.pull).
///
/// Always includes control-plane basics; `exec` / `terminal` are gated by
/// config (+ compile feature for terminal).
pub(crate) fn agent_capabilities(config: &Config) -> Vec<&'static str> {
    let mut caps = vec!["message", "event"];
    if cfg!(feature = "ping") {
        caps.push("ping");
    }
    if !config.disable_exec {
        caps.push("exec");
    }
    if cfg!(feature = "terminal") && !config.disable_web_ssh && !config.http_only {
        caps.push("terminal");
    }
    caps
}

fn is_timeout(e: &WsErr) -> bool {
    matches!(e, WsErr::Io(s) if {
        let s = s.to_lowercase();
        s.contains("timed out")
            || s.contains("would block")
            || s.contains("temporarily unavailable") // EAGAIN/EWOULDBLOCK on Linux
    })
}

#[cfg(test)]
mod p8_tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn deflate_failure_detected_from_protocol_message() {
        let e = TickErr::Ws(WsErr::Protocol(
            "permessage-deflate inflate error: UnexpectedEof".into(),
        ));
        assert!(is_deflate_failure(&e));
        let e2 = TickErr::Ws(WsErr::Protocol("other protocol error".into()));
        assert!(!is_deflate_failure(&e2));
    }

    #[test]
    fn capabilities_hide_terminal_when_http_only_or_disabled() {
        let mut c = Config::default();
        c.http_only = true;
        c.disable_web_ssh = false;
        c.disable_exec = false;
        let caps = agent_capabilities(&c);
        assert!(caps.contains(&"message"));
        assert!(caps.contains(&"exec"));
        assert!(!caps.contains(&"terminal"), "http_only must hide terminal");

        c.http_only = false;
        c.disable_web_ssh = true;
        let caps = agent_capabilities(&c);
        assert!(!caps.contains(&"terminal"));
    }

    #[test]
    fn capabilities_include_terminal_when_enabled() {
        let mut c = Config::default();
        c.http_only = false;
        c.disable_web_ssh = false;
        c.disable_exec = true;
        let caps = agent_capabilities(&c);
        assert!(!caps.contains(&"exec"));
        if cfg!(feature = "terminal") {
            assert!(caps.contains(&"terminal"));
        } else {
            assert!(!caps.contains(&"terminal"));
        }
    }
}
