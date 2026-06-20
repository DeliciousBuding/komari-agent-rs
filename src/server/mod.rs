//! Server orchestration: WebSocket event loop, reconnection, heartbeat,
//! and server-message dispatch.
//!
//! This is the agent's runtime backbone after config loading.  It runs a
//! single-threaded, never-returning loop that:
//!
//! 1. Initialises TLS (OS-native root certificates via rustls/ring)
//! 2. Uploads basic system info (once, fire-and-forget)
//! 3. Connects to the Komari server via WebSocket
//! 4. Sends heartbeats, reads/dispatches server messages
//! 5. Reconnects on connection loss
//!
//! # References
//!
//! - Go `server/websocket.go` (`EstablishWebSocketConnection`) — connection
//!   lifecycle and message dispatch
//! - Go `server/basicInfo.go` (`uploadBasicInfo`) — startup info upload
//! - Architecture reference §2.2 (server/ tree, ~135 lines target)

use crate::config::Config;
use crate::protocol::v2;
use std::sync::Arc;
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════════════
// Logging shim — switch to `log` crate when it lands in Cargo.toml.
// ═══════════════════════════════════════════════════════════════════════════

macro_rules! info {
    ($($arg:tt)*) => (eprintln!("[komari] {}", format!($($arg)*)));
}
macro_rules! warn {
    ($($arg:tt)*) => (eprintln!("[komari] WARN: {}", format!($($arg)*)));
}
macro_rules! error {
    ($($arg:tt)*) => (eprintln!("[komari] ERROR: {}", format!($($arg)*)));
}
macro_rules! debug {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!("[komari] DEBUG: {}", format!($($arg)*));
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════════
// Placeholder: WebSocket types (→ crate::ws, currently a stub)
// ═══════════════════════════════════════════════════════════════════════════

/// An open WebSocket connection.
///
/// To be moved to `crate::ws::WsConnection`.  Wraps a `rustls::StreamOwned`
/// over `TcpStream`.  Provides frame-level send/receive with timeout.
#[allow(dead_code)]
struct WsConnection;

/// A decoded WebSocket data/control frame.
///
/// To be moved to `crate::ws::WsMessage`.
#[allow(dead_code)]
#[derive(Debug)]
enum WsMessage {
    /// Text frame payload (UTF-8 JSON-RPC).
    Text(Vec<u8>),
    /// Ping control frame payload.
    Ping(Vec<u8>),
    /// Close control frame (connection teardown).
    Close,
}

/// WebSocket operation error.
///
/// To be moved to `crate::ws::WsErr`.
#[allow(dead_code)]
#[derive(Debug)]
enum WsErr {
    /// No frame received within the timeout window.
    Timeout,
    /// I/O error, protocol violation, or handshake failure.
    Other(String),
}

impl WsConnection {
    fn send_text(&self, _data: &[u8]) -> Result<(), WsErr> {
        todo!("ws::WsConnection::send_text — src/ws.rs not yet implemented (DD3)")
    }
    fn send_pong(&self, _data: &[u8]) -> Result<(), WsErr> {
        todo!("ws::WsConnection::send_pong — src/ws.rs not yet implemented (DD3)")
    }
    fn read_message_with_timeout(&self, _timeout: Duration) -> Result<WsMessage, WsErr> {
        todo!("ws::WsConnection::read_message_with_timeout — src/ws.rs not yet implemented (DD3)")
    }
    fn close(self) -> Result<(), WsErr> {
        todo!("ws::WsConnection::close — src/ws.rs not yet implemented (DD3)")
    }
}

/// Establish a WebSocket connection to `url`.
///
/// Performs: TCP connect → TLS handshake → HTTP upgrade → verify
/// `Sec-WebSocket-Accept`.  Uses `crate::crypto::ws_generate_key` +
/// `crate::crypto::ws_accept_key` for the handshake.
///
/// To be moved to `crate::ws::connect`.
fn ws_connect(
    _url: &str,
    _tls_config: &Arc<rustls::ClientConfig>,
    _headers: &[(String, String)],
) -> Result<WsConnection, WsErr> {
    todo!("ws::connect — src/ws.rs not yet implemented (DD3)")
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Main entry point for the agent runtime.  **Never returns.**
///
/// # Flow
///
/// ```text
/// make_tls_config(config) → Arc<ClientConfig>
///   → upload_basic_info(config, &tls_cfg)
///   → loop {
///       connect_ws(config, &tls_cfg)
///         → tick_loop(conn, config)
///         → sleep(reconnect_interval)
///     }
/// ```
///
/// TLS init failure is fatal (exit 1).  Basic-info upload failure is
/// non-fatal (logged, agent continues).  Connection loss triggers an
/// immediate reconnect attempt after `config.reconnect_interval` seconds.
pub fn run(config: &Config) -> ! {
    // Step 1: Initialise TLS config (fatal on failure).
    // Uses crate::tls::make_tls_config (rustls + ring, DD5/DD6).
    // The `ignore_unsafe_cert` flag maps to Go's InsecureSkipVerify.
    let tls_cfg = match crate::tls::make_tls_config(config) {
        Ok(cfg) => Arc::new(cfg),
        Err(e) => {
            error!("TLS config init failed: {}", e);
            std::process::exit(1);
        }
    };

    // Step 2: Upload basic system info once at startup (non-fatal).
    if let Err(e) = upload_basic_info(config, &tls_cfg) {
        warn!("basic info upload failed (continuing): {}", e);
    }

    // Step 3: Main loop — connect, tick, reconnect.
    loop {
        match connect_ws(config, &tls_cfg) {
            Ok(conn) => {
                info!("WebSocket connected, entering tick loop");
                tick_loop(conn, config);
                // tick_loop returned → connection lost.
            }
            Err(WsErr::Other(ref msg)) => {
                error!("WebSocket connection failed: {}", msg);
            }
            Err(WsErr::Timeout) => {
                error!("WebSocket connection timed out");
            }
        }

        info!("Reconnecting in {} seconds...", config.reconnect_interval);
        std::thread::sleep(Duration::from_secs(config.reconnect_interval));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Connection management
// ═══════════════════════════════════════════════════════════════════════════

/// Establish a WebSocket connection to the Komari server.
///
/// Builds the WebSocket URL from config (protocol version selection,
/// `http(s)://` → `ws(s)://`), constructs optional Cloudflare Access
/// headers, and delegates to `ws_connect`.
fn connect_ws(config: &Config, tls_cfg: &Arc<rustls::ClientConfig>) -> Result<WsConnection, WsErr> {
    let url = build_ws_url(config);
    let headers = build_ws_headers(config);

    debug!("Connecting to {}", url);
    ws_connect(&url, tls_cfg, &headers)
}

/// Build the WebSocket endpoint URL from the agent configuration.
///
/// Matches Go `buildWebSocketEndpoint`:
/// - v2: `wss://host/api/clients/v2/rpc?token=...`
/// - v1: `wss://host/api/clients/report?token=...`
///
/// The `http(s)://` prefix is replaced with `ws(s)://`.
fn build_ws_url(config: &Config) -> String {
    let base = config.endpoint.trim_end_matches('/');
    let encoded_token = crate::ws::url_encode(&config.token);

    let path = if config.protocol_version >= 2 {
        format!("/api/clients/v2/rpc?token={}", encoded_token)
    } else {
        format!("/api/clients/report?token={}", encoded_token)
    };

    // Replace http:// → ws://, https:// → wss://
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{}{}", rest, path)
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{}{}", rest, path)
    } else {
        // No recognised scheme — assume wss:// and prepend to host-only base
        format!("wss://{}{}", base, path)
    }
}

/// Build WebSocket upgrade headers from config.
///
/// Includes Cloudflare Access headers when configured
/// (matches Go `newWSHeaders`).
fn build_ws_headers(config: &Config) -> Vec<(String, String)> {
    let mut headers = Vec::new();

    if !config.cf_access_client_id.is_empty() && !config.cf_access_client_secret.is_empty() {
        headers.push((
            "CF-Access-Client-Id".to_string(),
            config.cf_access_client_id.clone(),
        ));
        headers.push((
            "CF-Access-Client-Secret".to_string(),
            config.cf_access_client_secret.clone(),
        ));
    }

    headers
}

/// Build Cloudflare Access headers as an `Option<(&str, &str)>` tuple for
/// use with `crate::http::http_post`.
fn build_cf_headers(config: &Config) -> Option<(&str, &str)> {
    if config.cf_access_client_id.is_empty() || config.cf_access_client_secret.is_empty() {
        None
    } else {
        Some((&config.cf_access_client_id, &config.cf_access_client_secret))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tick loop
// ═══════════════════════════════════════════════════════════════════════════

/// Single-threaded event loop: send heartbeat, poll for server messages,
/// sleep, repeat.
///
/// Returns when the connection is no longer viable (I/O error, close frame
/// received).  The caller (`run`) then reconnects.
///
/// # Heartbeat
///
/// A static JSON-RPC v2 `agent.report` notification is sent every iteration.
/// Real system metrics will replace the static payload in Phase 2.
///
/// # Message polling
///
/// Server messages are read with a 100 ms timeout (`poll()`-style
/// non-blocking under the hood).  Supported incoming frames:
///
/// | Frame | Action |
/// |-------|--------|
/// | Text  | Dispatch to `handle_server_message` |
/// | Ping  | Reply with a Pong frame |
/// | Close | Break out of the loop |
fn tick_loop(conn: WsConnection, config: &Config) {
    loop {
        // Send heartbeat (static JSON for now — real metrics in P2).
        let heartbeat = build_static_heartbeat();
        if let Err(e) = conn.send_text(&heartbeat) {
            error!("Failed to send heartbeat: {:?}", e);
            break;
        }

        // Read any server messages (non-blocking with timeout).
        match conn.read_message_with_timeout(Duration::from_millis(100)) {
            Ok(WsMessage::Text(data)) => {
                handle_server_message(&data, config);
            }
            Ok(WsMessage::Ping(data)) => {
                if let Err(e) = conn.send_pong(&data) {
                    error!("Failed to send pong: {:?}", e);
                    break;
                }
            }
            Ok(WsMessage::Close) => {
                info!("Server sent close frame");
                break;
            }
            Err(WsErr::Timeout) => {
                // No message within the timeout window — continue.
            }
            Err(e) => {
                error!("WebSocket read error: {:?}", e);
                break;
            }
        }

        std::thread::sleep(Duration::from_secs_f64(config.interval));
    }

    // Best-effort close (the connection object is dropped here regardless).
    let _ = conn.close();
}

// ═══════════════════════════════════════════════════════════════════════════
// Heartbeat
// ═══════════════════════════════════════════════════════════════════════════

/// Build a minimal JSON-RPC v2 `agent.report` notification for connectivity
/// testing.
///
/// The payload is structurally valid but semantically empty — a placeholder
/// until real system metrics collection (via `monitor::generate_report`)
/// is wired into the tick loop in Phase 2.
///
/// Wire format (via `v2::new_notification`):
///
/// ```json
/// {"jsonrpc":"2.0","method":"agent.report","params":{…}}
/// ```
fn build_static_heartbeat() -> Vec<u8> {
    // Minimal params: structurally valid but semantically empty snapshot.
    // Will be replaced by `crate::monitor::generate_report` in P2.
    let params = br#"{"cpu":{"usage":0.0},"ram":{"total":0,"used":0,"swap":0},"disk":[],"network":[],"uptime":0,"os":"","arch":""}"#;
    v2::new_notification(v2::METHOD_AGENT_REPORT, params)
}

// ═══════════════════════════════════════════════════════════════════════════
// Server message dispatch
// ═══════════════════════════════════════════════════════════════════════════

/// Parse an incoming WebSocket text frame and dispatch to the appropriate
/// handler stub.
///
/// # Classification (stub phase — string matching)
///
/// | Pattern | Handler |
/// |---------|---------|
/// | `"jsonrpc":"2.0"` with method | v2 JSON-RPC event dispatch |
/// | `"message":"exec"` or `"task_id"` | v1 exec stub |
/// | `"message":"ping"` or `"ping_task_id"` | v1 ping stub |
/// | `"message":"terminal"` or `"request_id"` | v1 terminal stub |
///
/// Full JSON-RPC parsing (via `crate::json` or a dedicated codec) and real
/// task/terminal handlers will replace this in later phases.
fn handle_server_message(data: &[u8], config: &Config) {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            warn!("Received non-UTF8 WebSocket message, ignoring");
            return;
        }
    };

    // v2 JSON-RPC events — method-based dispatch.
    if text.contains("\"jsonrpc\":\"2.0\"") || text.contains("\"jsonrpc\": \"2.0\"") {
        dispatch_v2_event(data, config);
        return;
    }

    // v1 flat-message dispatch (matching Go handleWebSocketMessages).
    if text.contains("\"message\":\"exec\"") || text.contains("\"task_id\"") {
        handle_exec_stub(data, config);
        return;
    }

    if text.contains("\"message\":\"ping\"") || text.contains("\"ping_task_id\"") {
        handle_ping_stub(data, config);
        return;
    }

    if text.contains("\"message\":\"terminal\"") || text.contains("\"request_id\"") {
        handle_terminal_stub(data, config);
        return;
    }

    debug!("Unrecognised server message: {}", text);
}

/// Stub: dispatch a v2 JSON-RPC event by method name.
///
/// Matches Go `processV2Event` dispatch: `agent.exec`, `agent.ping`,
/// `agent.terminal.request`, `agent.message`, `agent.event`.
fn dispatch_v2_event(data: &[u8], _config: &Config) {
    let text = std::str::from_utf8(data).unwrap_or("");

    if text.contains("\"agent.exec\"") {
        info!("[stub] v2 exec event received");
    } else if text.contains("\"agent.ping\"") {
        info!("[stub] v2 ping event received");
    } else if text.contains("\"agent.terminal") {
        info!("[stub] v2 terminal event received");
    } else if text.contains("\"agent.message\"") || text.contains("\"agent.event\"") {
        info!("[stub] v2 message/event: {}", text);
    } else {
        debug!("[stub] Unknown v2 method in: {}", text);
    }
}

/// Stub: remote command execution.
///
/// In the full implementation: spawn subprocess (PowerShell on Windows,
/// `sh -s` on Unix), capture stdout/stderr, upload result via WebSocket or
/// HTTP POST.
fn handle_exec_stub(data: &[u8], _config: &Config) {
    let text = std::str::from_utf8(data).unwrap_or("<non-utf8>");
    info!("[stub] Exec request: {}", text);
}

/// Stub: ping probe (ICMP/TCP/HTTP).
///
/// In the full implementation: dispatch to `server/ping_icmp.rs`,
/// `server/ping_tcp.rs`, or `server/ping_http.rs` based on `ping_type`.
fn handle_ping_stub(data: &[u8], _config: &Config) {
    let text = std::str::from_utf8(data).unwrap_or("<non-utf8>");
    info!("[stub] Ping request: {}", text);
}

/// Stub: interactive Web SSH terminal.
///
/// In the full implementation: establish a second WebSocket connection to
/// `/api/clients/terminal` and bridge PTY/ConPTY I/O.
fn handle_terminal_stub(data: &[u8], _config: &Config) {
    let text = std::str::from_utf8(data).unwrap_or("<non-utf8>");
    info!("[stub] Terminal request: {}", text);
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic info upload
// ═══════════════════════════════════════════════════════════════════════════

/// Upload basic system identification info to the Komari server.
///
/// Called once at startup.  POSTs a JSON-RPC v2 `agent.basicInfo`
/// notification or v1 raw JSON to the appropriate endpoint.
///
/// # Protocol selection (matching Go basicInfo.go)
///
/// - v2: `POST https://host/api/clients/v2/rpc?token=...` with JSON-RPC
///   `{"jsonrpc":"2.0","method":"agent.basicInfo","params":{"info":{…}}}`
/// - v1: `POST https://host/api/clients/uploadBasicInfo?token=...` with
///   raw JSON object
///
/// # Cloudflare Access
///
/// Passes `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers when
/// configured (via `crate::http::http_post` cf_headers parameter).
fn upload_basic_info(config: &Config, tls_cfg: &Arc<rustls::ClientConfig>) -> Result<(), String> {
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
        None, // no compression for stub
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

/// Build a minimal v2 JSON-RPC `agent.basicInfo` notification.
///
/// Wire format:
/// ```json
/// {"jsonrpc":"2.0","method":"agent.basicInfo","params":{"info":{...}}}
/// ```
///
/// Field names match Go `basicInfo.go` exactly for server compatibility.
/// Real collection will use `monitor/os.rs`, `monitor/cpu/`, `monitor/mem/`,
/// `monitor/ip/`, etc.
fn build_basic_info_v2() -> Vec<u8> {
    let info = br#"{"cpu_name":"","cpu_cores":0,"cpu_physical_cores":0,"arch":"","os":"","kernel_version":"","ipv4":"","ipv6":"","mem_total":0,"swap_total":0,"disk_total":0,"gpu_name":"","virtualization":"","version":"0.1.0"}"#;

    // Wrap in JSON-RPC v2: {"jsonrpc":"2.0","method":"agent.basicInfo","params":{"info":<info>}}
    let params = {
        let mut v = Vec::with_capacity(info.len() + 20);
        v.extend_from_slice(b"{\"info\":");
        v.extend_from_slice(info);
        v.push(b'}');
        v
    };
    v2::new_notification(v2::METHOD_AGENT_BASIC_INFO, &params)
}

/// Build a minimal v1 basic info payload (raw JSON object, no JSON-RPC
/// envelope).
fn build_basic_info_v1() -> Vec<u8> {
    br#"{"cpu_name":"","cpu_cores":0,"cpu_physical_cores":0,"arch":"","os":"","kernel_version":"","ipv4":"","ipv6":"","mem_total":0,"swap_total":0,"disk_total":0,"gpu_name":"","virtualization":"","version":"0.1.0"}"#.to_vec()
}
