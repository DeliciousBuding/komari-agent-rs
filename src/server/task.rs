//! Task types for server-sent commands (exec, ping, terminal).
//!
//! # References
//!
//! - Go `server/task.go` — task lifecycle and result upload
//! - Go `server/ping_icmp.go`, `server/ping_tcp.go`, `server/ping_http.go`

/// A task dispatched by the Komari server for this agent to execute.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Task {
    /// Unique task identifier (server-assigned).
    pub task_id: String,
    /// Task type discriminator.
    pub kind: TaskKind,
    /// Raw JSON parameters (lazily parsed by the handler).
    pub params: Vec<u8>,
}

/// Discriminator for server-sent task types.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TaskKind {
    /// Remote command execution (PowerShell on Windows, `sh -s` on Unix).
    Exec,
    /// Network probe (ICMP / TCP / HTTP ping).
    Ping,
    /// Interactive Web SSH terminal session.
    Terminal,
}

impl TaskKind {
    /// Classify from the v2 JSON-RPC method string.
    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "agent.exec" => Some(Self::Exec),
            "agent.ping" => Some(Self::Ping),
            "agent.terminal.request" => Some(Self::Terminal),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Ping handler (feature-gated)
// ═══════════════════════════════════════════════════════════════════════════

/// Minimal byte-scanner: extract a JSON string value for `key` from raw JSON bytes.
/// Handles both `"key":"value"` (v1) and `"key":"value"` (v2 camelCase).
/// Returns `None` if the key is not found or the JSON is malformed.
#[cfg(feature = "ping")]
fn extract_json_string(params: &[u8], key: &str) -> Option<String> {
    let text = std::str::from_utf8(params).ok()?;

    // Manual scan: find `"key":`
    let mut i = 0;
    while i < text.len() {
        // Skip whitespace
        while i < text.len() && text.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= text.len() {
            break;
        }

        // Expect a quote
        if text.as_bytes()[i] != b'"' {
            i += 1;
            continue;
        }
        i += 1; // skip opening quote

        // Match key bytes
        let key_start = i;
        while i < text.len() && text.as_bytes()[i] != b'"' {
            i += 1;
        }
        let found_key = &text[key_start..i];
        i += 1; // skip closing quote

        // Skip `:`
        while i < text.len() && text.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= text.len() || text.as_bytes()[i] != b':' {
            continue;
        }
        i += 1; // skip colon
        while i < text.len() && text.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }

        if found_key == key {
            // Extract the value — expect a JSON string
            if i < text.len() && text.as_bytes()[i] == b'"' {
                i += 1;
                let val_start = i;
                while i < text.len() && text.as_bytes()[i] != b'"' {
                    i += 1;
                }
                return Some(text[val_start..i].to_string());
            }
            return None;
        }

        // Skip the value (handle string, number, object, array)
        if i < text.len() {
            match text.as_bytes()[i] {
                b'"' => {
                    i += 1;
                    while i < text.len() && text.as_bytes()[i] != b'"' {
                        i += 1;
                    }
                    i += 1;
                }
                b'{' | b'[' => {
                    let open = text.as_bytes()[i];
                    let close = if open == b'{' { b'}' } else { b']' };
                    let mut depth = 1u32;
                    i += 1;
                    while i < text.len() && depth > 0 {
                        match text.as_bytes()[i] {
                            c if c == open => depth += 1,
                            c if c == close => depth -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                }
                _ => {
                    // number, true, false, null — skip until comma or closing brace
                    while i < text.len() && text.as_bytes()[i] != b',' && text.as_bytes()[i] != b'}'
                    {
                        i += 1;
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct PingResult {
    pub ping_type: String,
    pub value: i64,
}

impl PingResult {
    pub fn new(ping_type: &str, value: i64) -> Self {
        Self {
            ping_type: ping_type.to_string(),
            value,
        }
    }

    /// Build the JSON payload for ping result upload.
    #[cfg(feature = "ping")]
    pub fn build_payload(&self, task_id: u64, protocol_version: u8) -> Vec<u8> {
        let now = current_time_iso8601();
        if protocol_version >= 2 {
            format!(
                r#"{{"type":"ping_result","taskId":{},"pingType":"{}","value":{},"finishedAt":"{}"}}"#,
                task_id, self.ping_type, self.value, now
            )
            .into_bytes()
        } else {
            format!(
                r#"{{"type":"ping_result","task_id":{},"ping_type":"{}","value":{},"finished_at":"{}"}}"#,
                task_id, self.ping_type, self.value, now
            )
            .into_bytes()
        }
    }
}

/// Minimal ISO 8601 timestamp (UTC, no external crate).
#[cfg(feature = "ping")]
fn current_time_iso8601() -> String {
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Manual conversion — GM time components from UNIX epoch
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate year/month/day from days since 1970-01-01
    let (year, month, day) = civil_from_days(days as i64);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since 1970-01-01 to (year, month, day).
/// Algorithm from Howard Hinnant's civil_from_days.
#[cfg(feature = "ping")]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + (m as i64 <= 2) as i64, m, d)
}

/// Perform ping with 3-tier fallback.
///
/// ## Fallback chain
/// - `icmp` → `ping_icmp()`; on failure (permission denied, timeout), falls back to TCP.
/// - `tcp`  → `ping_tcp()`; on failure, falls back to HTTP.
/// - `http` → `ping_http()`; no further fallback.
///
/// `target` is the host/IP for icmp/tcp, or URL for http.
/// Returns `PingResult` with the actual ping type used and RTT (or -1).
#[cfg(feature = "ping")]
pub fn handle_ping(ping_type: &str, target: &str, timeout_ms: Option<u64>) -> PingResult {
    match ping_type {
        "icmp" => {
            let rtt = super::ping_icmp::ping_icmp(target, timeout_ms);
            if rtt >= 0 {
                return PingResult::new("icmp", rtt);
            }
            // Fallback: ICMP failed (likely permission denied) → TCP
            let rtt = super::ping_tcp::ping_tcp(target, timeout_ms);
            if rtt >= 0 {
                return PingResult::new("tcp", rtt);
            }
            // Fallback: TCP failed → HTTP
            let rtt = super::ping_http::ping_http(target, timeout_ms);
            PingResult::new("http", rtt)
        }
        "tcp" => {
            let rtt = super::ping_tcp::ping_tcp(target, timeout_ms);
            if rtt >= 0 {
                return PingResult::new("tcp", rtt);
            }
            // Fallback: TCP failed → HTTP
            let rtt = super::ping_http::ping_http(target, timeout_ms);
            PingResult::new("http", rtt)
        }
        "http" => {
            let rtt = super::ping_http::ping_http(target, timeout_ms);
            PingResult::new("http", rtt)
        }
        _ => PingResult::new("unknown", -1),
    }
}

/// Parse ping parameters from raw JSON bytes.
/// Returns (ping_type, target) or None if parsing fails.
#[cfg(feature = "ping")]
pub fn parse_ping_params(params: &[u8]) -> Option<(String, String)> {
    // Try v2 camelCase first: "pingType"
    let ping_type = extract_json_string(params, "pingType")
        .or_else(|| extract_json_string(params, "ping_type"))?;
    let target = extract_json_string(params, "target")?;
    Some((ping_type, target))
}

// ═══════════════════════════════════════════════════════════════════════════
// Ping stub (when feature is disabled)
// ═══════════════════════════════════════════════════════════════════════════

/// When the `ping` feature is disabled, return -1 gracefully.
#[cfg(not(feature = "ping"))]
pub fn handle_ping(_ping_type: &str, _target: &str, _timeout_ms: Option<u64>) -> PingResult {
    PingResult::new("disabled", -1)
}

#[cfg(not(feature = "ping"))]
pub fn parse_ping_params(_params: &[u8]) -> Option<(String, String)> {
    None
}
