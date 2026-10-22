//! Reconnection loop with protocol FSM and exponential backoff.
//!
//! Reference: Go agent `EstablishWebSocketConnection()` in `server/websocket.go`.
//!
//! # Flow
//! ```text
//! loop {
//!     connect_with_fsm()
//!       → Ok(conn) → run_tick_loop() (success only after healthy session)
//!       → Err      → fsm.on_failure() → backoff → retry
//!     tick error → fsm.on_failure() → backoff → reconnect
//!     healthy HTTP re-probe → fsm.on_reconnect() → WS
//! }
//! ```
//!
//! Uses [`crate::protocol::fsm::ProtocolFsm`] for the 3-strike fallback
//! rule: 3 consecutive WS failures select HTTP; both transports remain v2.

use super::backoff::Backoff;
use crate::arena::ScratchArena;
use crate::config::Config;
use crate::http::{HttpErr, http_post, http_post_timeout};
use crate::monitor::{Monitor, generate_report};
use crate::protocol::fsm::{FailureKind, ProtocolFsm, ProtocolMode, RPC_PATH};
use crate::protocol::v2;
use crate::ws::{WsConnection, WsErr, WsMessage};
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
const REPROBE_INTERVAL: Duration = Duration::from_secs(600);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const CONTROL_POLL_INTERVAL: Duration = Duration::from_secs(1);

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
    // Basic info is uploaded once per session and periodically thereafter.
    // Both transports authenticate using the existing registration token.
    let dial = crate::proxy::Dialer::from_config(&runtime_cfg);

    let mut fsm = ProtocolFsm::new(runtime_cfg.http_only);
    let mut backoff = Backoff::new(runtime_cfg.max_retries, runtime_cfg.reconnect_interval);
    let mut monitor = Monitor::new_with_config(&runtime_cfg);
    let mut arena = ScratchArena::new();
    let bus = Arc::new(Mutex::new(V2EventBus::default()));
    // Periodic re-probe of the initial (preferred) protocol mode so a
    // historical downgrade does not pin the agent to a fallback forever.
    let mut last_reprobe = Instant::now();

    eprintln!(
        "[komari] capabilities: {}",
        agent_capabilities(&runtime_cfg).join(",")
    );

    loop {
        // Failures must accumulate across short sessions. A healthy HTTP
        // fallback periodically yields to retry WS; HTTP-only never does.
        if reprobe_due(&fsm, last_reprobe.elapsed()) {
            eprintln!("[komari] re-probing preferred WebSocket transport");
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
                // Upload basic info without resetting failure/backoff streaks:
                // a successful upgrade alone does not prove session health.
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
        let result = run_tick_loop(
            conn,
            &mut fsm,
            &mut monitor,
            &mut arena,
            &runtime_cfg,
            &tls_cfg,
            &dial,
            Arc::clone(&bus),
            last_reprobe,
        );
        if session_start.elapsed() >= MIN_HEALTHY_SESSION {
            // Clear the old streak before counting this session's failure.
            fsm.on_success();
            backoff.reset();
        }
        if let Err(e) = result {
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
        } else {
            // The healthy HTTP loop yielded specifically for a WS re-probe.
            // Do not charge a failure or sleep through a healthy transition.
            continue;
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
        ProtocolMode::WsV2 => {
            let conn = WsConnection::connect(
                &config.endpoint,
                RPC_PATH,
                &config.token,
                Arc::clone(tls_cfg),
                Duration::from_secs(30),
                &[],
                dial,
                !config.disable_compression,
            )?;
            Ok(Connection::Ws(Box::new(conn)))
        }
        // The first actual report authenticates and validates the endpoint.
        // An empty probe is not a valid JSON-RPC request.
        ProtocolMode::HttpV2 => Ok(Connection::Http),
    }
}

fn build_http_url(config: &Config) -> String {
    format!(
        "{}{}?token={}",
        config.endpoint.trim_end_matches('/'),
        RPC_PATH,
        crate::ws::url_encode(&config.token)
    )
}

fn reprobe_due(fsm: &ProtocolFsm, elapsed: Duration) -> bool {
    fsm.mode() != fsm.initial_mode() && elapsed >= REPROBE_INTERVAL
}

/// Execute one ping task and upload the result as a v2 `agent.pingResult`
/// notification for HTTP-delivered events.
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
        &build_http_url(config),
        &payload,
        "application/json",
        None,
        &[],
        tls_cfg,
        dial,
    ) {
        Ok(resp) if resp.status_code == 200 => {
            if let Err(e) = v2::validate_response(&resp.body) {
                eprintln!("[komari] WARN: ping result: {e}");
            }
        }
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

/// Answer an `agent.startupConfig` event: POST the redacted startup-config
/// snapshot back as an `agent.startupConfig.result` **request** whose id
/// echoes the server-assigned request id. Mirrors the upstream Go agent,
/// which uses the authenticated POST transport for both WS and pull
/// delivery; the server binds the result to the authenticated UUID.
/// The payload is never logged (it carries operator-identifying fields).
fn handle_startup_config_request(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    request_id: &str,
) {
    if request_id.is_empty() {
        eprintln!("[komari] WARN: agent.startupConfig without request_id, ignoring");
        return;
    }
    let params = v2::build_startup_config_params(request_id, &config.startup_config_json());
    let payload = v2::new_request(request_id, v2::METHOD_AGENT_STARTUP_CONFIG_RESULT, &params);
    let (body, encoding) = gzip_if_enabled(&payload, config);
    match http_post(
        &build_http_url(config),
        &body,
        "application/json",
        encoding,
        &[],
        tls_cfg,
        dial,
    ) {
        Ok(resp) if resp.status_code == 200 => match v2::validate_response(&resp.body) {
            Ok(()) => {
                eprintln!("[komari] startup config returned to server (request {request_id})")
            }
            Err(e) => eprintln!("[komari] WARN: startup config result: {e}"),
        },
        Ok(resp) => eprintln!(
            "[komari] WARN: startup config result returned HTTP {}",
            resp.status_code
        ),
        Err(e) => eprintln!("[komari] WARN: startup config result upload failed: {e}"),
    }
}

/// `agent.switchVersion` is permanently unsupported by fleet policy: remote
/// version switching conflicts with `--disable-auto-update` plus the
/// SHA-256-pinned manual rollout SOP. The event is fire-and-forget (the
/// server has already answered `queued: true`), so we only log — loudly and
/// specifically, not the generic unhandled-method line — and deliberately do
/// not advertise the `switch_version` capability.
fn log_switch_version_policy() {
    eprintln!(
        "[komari] policy: agent.switchVersion ignored (remote version switching is \
         unsupported; fleet updates are manual and hash-pinned)"
    );
}
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
    dispatch_server_message(event_json.as_bytes(), config, dial, tls_cfg, None);
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
    let url = build_http_url(config);
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
                let response = http_post_timeout(
                    &url,
                    &req,
                    "application/json",
                    None,
                    &[],
                    &tls_cfg,
                    &dial,
                    Duration::from_secs(35),
                );
                // A response from an obsolete session must not execute tasks
                // after the main loop has recovered WS or reconnected HTTP.
                if HTTP_V2_GENERATION.load(AtomicOrdering::SeqCst) != generation {
                    return;
                }
                match response {
                    Ok(resp) if resp.status_code == 200 => {
                        if let Err(e) = v2::validate_response(&resp.body) {
                            eprintln!("[komari] WARN: v2 pull: {e}");
                            std::thread::sleep(Duration::from_secs(
                                config.reconnect_interval.max(1),
                            ));
                        } else {
                            process_v2_response_events(&resp.body, &bus, &config, &dial, &tls_cfg);
                        }
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

// ═══════════════════════════════════════════════════════════════════════════
// run_tick_loop — report cadence independent of control-plane polling
// ═══════════════════════════════════════════════════════════════════════════

/// Cancels a pull worker when its HTTP session ends, including error returns.
struct PullSessionGuard;

impl Drop for PullSessionGuard {
    fn drop(&mut self) {
        HTTP_V2_GENERATION.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

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
    last_reprobe: Instant,
) -> Result<(), TickErr> {
    let mut last_heartbeat = Instant::now();
    let mut next_report = Instant::now();
    let report_interval = Duration::from_secs_f64(config.interval);
    let mut last_info_refresh = Instant::now();
    let info_interval = Duration::from_secs(config.info_report_interval.saturating_mul(60));

    // HTTP receives events on its existing bounded long-poll worker; WS
    // receives them on the socket. Never retain an old pull session.
    HTTP_V2_GENERATION.fetch_add(1, AtomicOrdering::SeqCst);
    let _pull_session = PullSessionGuard;
    let mut pull_started = false;

    loop {
        if reprobe_due(fsm, last_reprobe.elapsed()) {
            return Ok(());
        }
        if next_report <= Instant::now() {
            let report = generate_report(monitor, arena, config);
            match &mut conn {
                Connection::Ws(ws) => {
                    let params = wrap_report_params(report, &[]);
                    let notif = v2::new_notification(v2::METHOD_AGENT_REPORT, &params);
                    ws.send_text(&notif)?;
                }
                Connection::Http => {
                    let acks = match bus.lock() {
                        Ok(mut b) => b.drain_acks(),
                        Err(_) => Vec::new(),
                    };
                    let params = wrap_report_params(report, &acks);
                    let req = v2::new_request("report", v2::METHOD_AGENT_REPORT, &params);
                    let (body, encoding) = gzip_if_enabled(&req, config);
                    let resp = http_post(
                        &build_http_url(config),
                        &body,
                        "application/json",
                        encoding,
                        &[],
                        tls_cfg,
                        dial,
                    )?;
                    if resp.status_code != 200 {
                        return Err(TickErr::Other(format!(
                            "v2 report HTTP {}",
                            resp.status_code
                        )));
                    }
                    v2::validate_response(&resp.body).map_err(|e| TickErr::Other(e.to_string()))?;
                    // Do not spawn workers while the report endpoint is down.
                    // A successful authenticated report establishes HTTP health.
                    if !pull_started {
                        spawn_pull_thread(config, tls_cfg, dial, Arc::clone(&bus));
                        pull_started = true;
                    }
                    process_v2_response_events(&resp.body, &bus, config, dial, tls_cfg);
                }
            }
            next_report = Instant::now() + report_interval;
        }

        // Read even when the next metrics report is minutes away. This also
        // answers server ping frames promptly without async or new threads.
        if let Connection::Ws(ws) = &mut conn {
            let _ = ws
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(100)));
            match ws.read_message() {
                Ok(Some(WsMessage::Text(data))) => {
                    if v2::response_has_error(&data) {
                        return Err(TickErr::Other("server returned a JSON-RPC error".into()));
                    }
                    dispatch_server_message(&data, config, dial, tls_cfg, Some(ws));
                }
                Ok(Some(WsMessage::Ping(data))) => ws.send_pong(&data)?,
                Ok(Some(WsMessage::Close)) | Ok(None) => {
                    return Err(TickErr::Other("connection closed by server".into()));
                }
                Ok(Some(WsMessage::Binary(_))) | Ok(Some(WsMessage::Pong(_))) => {}
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e.into()),
            }
            let _ = ws.get_ref().set_read_timeout(Some(Duration::from_secs(30)));
        }
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            if let Connection::Ws(ws) = &mut conn {
                ws.send_ping()?;
            }
            last_heartbeat = Instant::now();
        }

        if last_info_refresh.elapsed() >= info_interval {
            if let Err(e) = super::update_basic_info(config, tls_cfg, dial) {
                eprintln!("[komari] WARN: periodic basic info refresh failed: {e}");
            }
            last_info_refresh = Instant::now();
        }

        // Poll control traffic at least once per second, independently of
        // report cadence. Account for work already done in this iteration.
        std::thread::sleep(control_poll_delay(
            next_report.saturating_duration_since(Instant::now()),
            HEARTBEAT_INTERVAL.saturating_sub(last_heartbeat.elapsed()),
        ));
    }
}

fn control_poll_delay(until_report: Duration, until_heartbeat: Duration) -> Duration {
    CONTROL_POLL_INTERVAL.min(until_report).min(until_heartbeat)
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
) {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[komari] WARN: non-UTF8 server message, ignoring");
            return;
        }
    };

    // Only v2 methods are dispatched; legacy flat messages are ignored.
    let method = super::task::extract_json_method(data).unwrap_or_default();
    match method.as_str() {
        "agent.exec" => {
            let task_id = super::task::extract_json_string(data, "task_id").unwrap_or_default();
            let command = super::task::extract_json_string(data, "command").unwrap_or_default();
            handle_exec_task(config, dial, tls_cfg, &task_id, &command);
        }
        "agent.ping" => {
            if let Some((tid, pt, tgt)) = extract_ping_fields(data) {
                if let Some(ws) = ws {
                    handle_ping_task(ws, tid, &pt, &tgt);
                } else {
                    run_ping_and_upload_v2(config, dial, tls_cfg, tid, &pt, &tgt);
                }
            }
        }
        "agent.terminal.request" => {
            let request_id =
                super::task::extract_json_string(data, "request_id").unwrap_or_default();
            handle_terminal_request(config, dial, tls_cfg, &request_id);
        }
        "agent.startupConfig" => {
            let request_id =
                super::task::extract_json_string(data, "request_id").unwrap_or_default();
            handle_startup_config_request(config, dial, tls_cfg, &request_id);
        }
        "agent.switchVersion" => log_switch_version_policy(),
        "agent.message" | "agent.event" => {
            eprintln!("[komari] server message/event: {}", abbreviate(text));
        }
        "" => {}
        other => eprintln!("[komari] unhandled v2 method '{other}'"),
    }
}

/// Extract `(task_id, ping_type, target)` from a ping task message, accepting
/// server snake_case and camelCase v2 parameter names.
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
/// (`agent.terminal.request`). HTTP-only agents explicitly reject this path.
fn handle_terminal_request(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    request_id: &str,
) {
    if config.http_only {
        eprintln!("[komari] terminal request refused (http_only)");
        return;
    }
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
(and build with --features terminal). Remote exec uses the independent --disable-exec setting.\n";
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
/// v2 `agent.taskResult` (parity with Go `executeCommand` + result upload).
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
    let result = super::task::execute_exec(command, config.disable_exec);
    let body = super::task::build_task_result_v2(task_id, &result.output, result.exit_code);
    if let Err(e) = upload_task_result(config, dial, tls_cfg, &body) {
        eprintln!("[komari] WARN: task result upload failed: {e}");
    }
}

/// POST an `agent.taskResult` JSON-RPC notification on the v2 endpoint.
fn upload_task_result(
    config: &Config,
    dial: &crate::proxy::Dialer,
    tls_cfg: &Arc<rustls::ClientConfig>,
    body: &[u8],
) -> Result<(), String> {
    let url = build_http_url(config);
    match http_post(&url, body, "application/json", None, &[], tls_cfg, dial) {
        Ok(r) if r.status_code == 200 => v2::validate_response(&r.body).map_err(str::to_string),
        Ok(r) => Err(format!("task/result returned HTTP {}", r.status_code)),
        Err(e) => Err(format!("task/result upload error: {e}")),
    }
}

/// Run a WS-delivered ping and send its v2 result on the same socket.
fn handle_ping_task(ws: &mut WsConnection, task_id: i64, ping_type: &str, target: &str) {
    eprintln!("[komari] ping task {task_id}: {ping_type} -> {target}");
    let result = super::task::handle_ping(ping_type, target, None);
    let id = if task_id < 0 { 0 } else { task_id as u64 };
    let params = result.build_payload(id, 2);
    let payload = v2::new_notification(v2::METHOD_AGENT_PING_RESULT, &params);
    if let Err(e) = ws.send_text(&payload) {
        eprintln!("[komari] WARN: failed to send ping result: {e:?}");
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
    fn reports_and_heartbeats_have_independent_cadence() {
        // A 5-minute reporting interval cannot delay WS reads or heartbeat.
        assert_eq!(
            control_poll_delay(Duration::from_secs(300), Duration::from_secs(30)),
            Duration::from_secs(1)
        );
        assert_eq!(
            control_poll_delay(Duration::from_secs(300), Duration::from_millis(50)),
            Duration::from_millis(50)
        );
        assert_eq!(
            control_poll_delay(Duration::from_millis(20), Duration::from_secs(30)),
            Duration::from_millis(20)
        );
    }

    #[test]
    fn healthy_http_fallback_yields_for_ws_reprobe_but_http_only_does_not() {
        let mut fsm = ProtocolFsm::new(false);
        for _ in 0..3 {
            fsm.on_failure(FailureKind::WsConnect);
        }
        assert!(!reprobe_due(
            &fsm,
            REPROBE_INTERVAL - Duration::from_secs(1)
        ));
        assert!(reprobe_due(&fsm, REPROBE_INTERVAL));
        fsm.on_reconnect();
        assert_eq!(fsm.mode(), ProtocolMode::WsV2);
        assert!(!reprobe_due(&fsm, REPROBE_INTERVAL));
        assert!(!reprobe_due(&ProtocolFsm::new(true), REPROBE_INTERVAL));
    }

    #[test]
    fn every_transport_failure_keeps_the_same_v2_rpc_endpoint() {
        let mut config = Config::default();
        config.endpoint = "https://status.example.com/".into();
        config.token = "test token".into();
        let mut fsm = ProtocolFsm::new(false);
        for _ in 0..1000 {
            fsm.on_failure(FailureKind::HttpStatus(404));
            assert_eq!(
                build_http_url(&config),
                "https://status.example.com/api/clients/v2/rpc?token=test%20token"
            );
        }
    }

    #[test]
    fn http_only_connection_never_attempts_a_websocket_or_empty_probe() {
        let mut config = Config::default();
        // No listener exists: success here proves that HTTP selection does not
        // make an eager WS upgrade or invalid empty JSON-RPC HTTP probe.
        config.endpoint = "http://127.0.0.1:0".into();
        config.http_only = true;
        let tls = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let dial = crate::proxy::Dialer::from_config(&config);
        let fsm = ProtocolFsm::new(true);
        assert!(matches!(
            connect_with_fsm(&fsm, &config, &tls, &dial),
            Ok(Connection::Http)
        ));
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
    let mut caps = vec!["message", "event", "startup_config"];
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
    fn startup_config_params_echo_request_id_and_redact() {
        let mut c = Config::default();
        c.endpoint = "https://status.example.com".to_string();
        c.token = "fleet-secret".to_string();
        let params = v2::build_startup_config_params("req-42", &c.startup_config_json());
        let s = String::from_utf8(params).unwrap();
        assert!(s.starts_with("{\"request_id\":\"req-42\",\"config\":{"));
        assert!(s.ends_with("}}"));
        assert!(!s.contains("fleet-secret"));
        // Envelope: the result travels as a request whose id echoes the
        // server-assigned request id (upstream Go agent behaviour).
        let req = v2::new_request(
            "req-42",
            v2::METHOD_AGENT_STARTUP_CONFIG_RESULT,
            s.as_bytes(),
        );
        let r = String::from_utf8(req).unwrap();
        assert!(r.contains("\"method\":\"agent.startupConfig.result\""));
        assert!(r.contains("\"id\":\"req-42\""));
    }

    #[test]
    fn capabilities_hide_terminal_when_http_only_or_disabled() {
        let mut c = Config::default();
        c.http_only = true;
        c.disable_web_ssh = false;
        c.disable_exec = false;
        let caps = agent_capabilities(&c);
        assert!(caps.contains(&"message"));
        assert!(caps.contains(&"startup_config"));
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
