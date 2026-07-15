//! Task types for server-sent commands (exec, ping, terminal).
//!
//! # References
//!
//! - Go `server/task.go` — task lifecycle and result upload
//! - Go `server/ping_icmp.go`, `server/ping_tcp.go`, `server/ping_http.go`
//!
//! Task classification (`TaskKind::from_method`) and ping helpers form part of the
//! server-task parity surface and feature-gated paths; not all are wired into the
//! live dispatch yet. Allow dead_code for the surface.
#![allow(dead_code)]

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
pub(crate) fn extract_json_string(params: &[u8], key: &str) -> Option<String> {
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

/// Extract a JSON number value for `key` (used for numeric `task_id` fields).
/// Returns the parsed i64, or None if absent / not a number.
pub(crate) fn extract_json_number(params: &[u8], key: &str) -> Option<i64> {
    let text = std::str::from_utf8(params).ok()?;
    let needle = format!("\"{key}\"");
    let mut idx = text.find(&needle)?;
    idx += needle.len();
    let bytes = text.as_bytes();
    // Skip whitespace + colon + whitespace.
    while idx < bytes.len()
        && (bytes[idx] == b' ' || bytes[idx] == b':' || bytes[idx] == b'\t' || bytes[idx] == b'\n')
    {
        idx += 1;
    }
    let start = idx;
    while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b'-') {
        idx += 1;
    }
    if start == idx {
        return None;
    }
    text[start..idx].parse::<i64>().ok()
}

/// Extract the `"method":"..."` value from a JSON-RPC message.
pub(crate) fn extract_json_method(params: &[u8]) -> Option<String> {
    extract_json_string(params, "method")
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
pub(crate) fn current_time_iso8601() -> String {
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

// ═══════════════════════════════════════════════════════════════════════════
// High-latency retry constants (issue #63)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "ping")]
const HIGH_LATENCY_THRESHOLD_MS: u64 = 1000;

#[cfg(feature = "ping")]
const MAX_LATENCY_RETRIES: u32 = 3;

#[cfg(feature = "ping")]
const TCP_RETRANSMISSION_DROP_THRESHOLD_MS: u64 = 800;

/// Measure a ping with high-latency retry.
///
/// If the first measurement succeeds but its RTT exceeds
/// [`HIGH_LATENCY_THRESHOLD_MS`], retry up to [`MAX_LATENCY_RETRIES`] more
/// times and return the minimum observed RTT.
///
/// ## TCP retransmission detection
///
/// On the **second** TCP measurement (`attempt == 1`), if the RTT drop from the
/// first attempt exceeds [`TCP_RETRANSMISSION_DROP_THRESHOLD_MS`], the function
/// returns `-1` immediately — a first-ping spike this large is treated as a TCP
/// retransmission artifact.
///
/// For non-TCP pings retransmission detection is skipped and the simple
/// min-of-N strategy is used.
#[cfg(feature = "ping")]
fn measure_with_retry(ping_type: &str, mut measure: impl FnMut() -> i64) -> i64 {
    let first = measure();
    if first < 0 || first <= HIGH_LATENCY_THRESHOLD_MS as i64 {
        return first;
    }

    // High latency on first attempt — retry
    let mut best = first;
    for attempt in 0..MAX_LATENCY_RETRIES {
        let rtt = measure();
        if rtt < 0 {
            // Measurement failed; keep the best valid RTT we already have.
            return best;
        }

        // TCP retransmission detection: only on the second measurement.
        if ping_type == "tcp" && attempt == 0 {
            let drop = first - rtt;
            if drop > TCP_RETRANSMISSION_DROP_THRESHOLD_MS as i64 {
                return -1;
            }
        }

        if rtt < best {
            best = rtt;
        }

        // Latency is now acceptable — stop retrying early.
        if rtt <= HIGH_LATENCY_THRESHOLD_MS as i64 {
            break;
        }
    }

    best
}

/// Perform ping with 3-tier fallback and high-latency retry.
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
            let rtt =
                measure_with_retry("icmp", || super::ping_icmp::ping_icmp(target, timeout_ms));
            if rtt >= 0 {
                return PingResult::new("icmp", rtt);
            }
            // Fallback: ICMP failed (likely permission denied) → TCP
            let rtt = measure_with_retry("tcp", || super::ping_tcp::ping_tcp(target, timeout_ms));
            if rtt >= 0 {
                return PingResult::new("tcp", rtt);
            }
            // Fallback: TCP failed → HTTP
            let rtt =
                measure_with_retry("http", || super::ping_http::ping_http(target, timeout_ms));
            PingResult::new("http", rtt)
        }
        "tcp" => {
            let rtt = measure_with_retry("tcp", || super::ping_tcp::ping_tcp(target, timeout_ms));
            if rtt >= 0 {
                return PingResult::new("tcp", rtt);
            }
            // Fallback: TCP failed → HTTP
            let rtt =
                measure_with_retry("http", || super::ping_http::ping_http(target, timeout_ms));
            PingResult::new("http", rtt)
        }
        "http" => {
            let rtt =
                measure_with_retry("http", || super::ping_http::ping_http(target, timeout_ms));
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

// ═══════════════════════════════════════════════════════════════════════════
// Remote command execution (agent.exec) — parity with Go server/task.go
// ═══════════════════════════════════════════════════════════════════════════

/// Outcome of a remote command execution.
#[derive(Debug, Clone)]
pub struct ExecResult {
    /// Combined stdout + stderr (stderr appended after a newline).
    pub output: String,
    /// Process exit code (-1 when the command could not be run at all).
    pub exit_code: i32,
}

impl ExecResult {
    pub fn new(output: String, exit_code: i32) -> Self {
        Self { output, exit_code }
    }
}

/// Execute a server-sent command and return its combined output + exit code.
///
/// Mirrors Go `executeCommand`, but the gate is **`disable_exec`** (independent
/// of WebSSH / `disable_web_ssh`):
///   - When `disable_exec` is set, returns a fixed "disabled" message with
///     exit code -1 (the server surfaces this as a failed task).
///   - An empty/whitespace command yields "No command provided", code 0.
///   - Unix: `sh -s` with the command piped to stdin.
///   - Windows: a temp `.ps1` script (UTF-8 + BOM, ExecutionPolicy bypass)
///     run via `powershell.exe`.
///   - stdout and stderr are merged (stderr last); `\r\n` is normalised to `\n`.
pub fn execute_exec(command: &str, disable_exec: bool) -> ExecResult {
    if disable_exec {
        return ExecResult::new("Remote control is disabled.".to_string(), -1);
    }
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return ExecResult::new("No command provided".to_string(), 0);
    }

    #[cfg(unix)]
    {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        // Parity with Go's exec context: bound execution so a hanging command
        // (e.g. `sleep infinity`, a stuck REPL) cannot stall the single-threaded
        // tick loop forever. 30 s matches Go's default.
        const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

        let mut child = match Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ExecResult::new(format!("failed to spawn shell: {e}"), -1),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(command.as_bytes());
            // stdin dropped here → child sees EOF and can finish.
        }
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        // Drain stdout/stderr on worker threads so a large output cannot fill
        // the pipe buffer and deadlock the child while we poll its status.
        let out_handle = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stdout.read_to_end(&mut b);
            b
        });
        let err_handle = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stderr.read_to_end(&mut b);
            b
        });

        // Poll for completion with a deadline; kill + reap on timeout.
        let deadline = Instant::now() + EXEC_TIMEOUT;
        let result: Result<Option<i32>, &'static str> = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status.code()),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("timeout");
                }
                Err(_) => break Err("wait failed"),
            }
        };

        let stdout_buf = out_handle.join().unwrap_or_default();
        let stderr_buf = err_handle.join().unwrap_or_default();

        match result {
            Ok(code) => normalize_output(&stdout_buf, &stderr_buf, code),
            Err("timeout") => {
                let mut r = normalize_output(&stdout_buf, &stderr_buf, None);
                r.output
                    .push_str("\n[komari] command timed out after 30s and was killed");
                r.exit_code = -124; // 124 = GNU `timeout` convention
                r
            }
            _ => ExecResult::new("failed to wait for command".to_string(), -1),
        }
    }

    #[cfg(windows)]
    {
        exec_windows_powershell(command)
    }

    #[cfg(not(any(unix, windows)))]
    {
        ExecResult::new(
            "remote command execution is not supported on this platform".to_string(),
            -1,
        )
    }
}

/// Merge stdout + stderr, normalise line endings, attach the exit code.
#[cfg(any(unix, windows))]
fn normalize_output(stdout: &[u8], stderr: &[u8], code: Option<i32>) -> ExecResult {
    let mut combined = String::from_utf8_lossy(stdout).into_owned();
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(stderr));
    }
    combined = combined.replace("\r\n", "\n");
    ExecResult::new(combined, code.unwrap_or(-1))
}

/// Windows execution path: write a UTF-8 (+ BOM) PowerShell script and run it
/// with `-NoProfile -ExecutionPolicy Bypass`, matching the Go agent.
#[cfg(windows)]
fn exec_windows_powershell(command: &str) -> ExecResult {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Write the command to a temp .ps1 file. Prefix with a UTF-8 BOM and an
    // encoding shim so non-ASCII output round-trips correctly.
    let tmp = match tempfile_path("komari-exec", ".ps1") {
        Ok(p) => p,
        Err(e) => return ExecResult::new(format!("failed to create temp script: {e}"), -1),
    };
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
         $OutputEncoding = [System.Text.Encoding]::UTF8\n\
         {command}"
    );
    let mut file = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return ExecResult::new(format!("failed to open temp script: {e}"), -1);
        }
    };
    // UTF-8 BOM + script body.
    let _ = file.write_all(&[0xEF, 0xBB, 0xBF]);
    let _ = file.write_all(script.as_bytes());
    drop(file);

    // Spawn with piped stdout/stderr, then apply the same 30 s timeout + worker
    // reader pattern as the Unix path (parity with Go's exec context).
    use std::io::Read;
    use std::time::{Duration, Instant};
    const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

    let mut child = match Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return ExecResult::new(format!("failed to run powershell: {e}"), -1);
        }
    };
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let out_handle = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b);
        b
    });
    let err_handle = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b);
        b
    });

    let deadline = Instant::now() + EXEC_TIMEOUT;
    let result: Result<Option<i32>, &'static str> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.code()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("timeout");
            }
            Err(_) => break Err("wait failed"),
        }
    };
    let _ = std::fs::remove_file(&tmp);

    let stdout_buf = out_handle.join().unwrap_or_default();
    let stderr_buf = err_handle.join().unwrap_or_default();
    match result {
        Ok(code) => normalize_output(&stdout_buf, &stderr_buf, code),
        Err("timeout") => {
            let mut r = normalize_output(&stdout_buf, &stderr_buf, None);
            r.output
                .push_str("\n[komari] command timed out after 30s and was killed");
            r.exit_code = -124;
            r
        }
        _ => ExecResult::new("failed to wait for command".to_string(), -1),
    }
}

/// Build a temp file path in the OS temp dir without pulling in the `tempfile`
/// crate. Uses the PID + a static counter to avoid collisions.
#[cfg(windows)]
fn tempfile_path(prefix: &str, suffix: &str) -> std::io::Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(dir.join(format!("{prefix}-{pid}-{n}{suffix}")))
}

/// Escape a string for safe embedding as a JSON string literal.
///
/// Handles the mandatory escapes (`\"`, `\\`, control chars) and common
/// whitespace (`\n`, `\r`, `\t`). Non-ASCII bytes are passed through as UTF-8.
pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build the v1 `task/result` upload body:
/// `{"task_id":"...","result":"...","exit_code":N,"finished_at":"..."}`
pub fn build_task_result(task_id: &str, result: &str, exit_code: i32) -> Vec<u8> {
    let now = current_time_iso8601();
    format!(
        r#"{{"task_id":"{}","result":"{}","exit_code":{},"finished_at":"{}"}}"#,
        json_escape(task_id),
        json_escape(result),
        exit_code,
        now
    )
    .into_bytes()
}

/// Build the v2 `agent.taskResult` JSON-RPC notification wrapping the same
/// payload fields (camelCase).
pub fn build_task_result_v2(task_id: &str, result: &str, exit_code: i32) -> Vec<u8> {
    let now = current_time_iso8601();
    let params = format!(
        r#"{{"taskId":"{}","result":"{}","exitCode":{},"finishedAt":"{}"}}"#,
        json_escape(task_id),
        json_escape(result),
        exit_code,
        now
    )
    .into_bytes();
    crate::protocol::v2::new_notification(crate::protocol::v2::METHOD_AGENT_TASK_RESULT, &params)
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn exec_runs_simple_command() {
        let r = execute_exec("echo hello", false);
        assert!(r.output.contains("hello"), "got: {}", r.output);
        assert_eq!(r.exit_code, 0);
    }

    #[test]
    #[cfg(unix)]
    fn exec_empty_command() {
        let r = execute_exec("   ", false);
        assert_eq!(r.exit_code, 0);
        assert!(r.output.contains("No command"));
    }

    #[test]
    fn exec_disabled_when_disable_exec() {
        let r = execute_exec("rm -rf /", true);
        assert_eq!(r.exit_code, -1);
        assert!(r.output.contains("disabled"));
    }

    #[test]
    fn exec_enabled_even_if_web_ssh_would_be_off() {
        // Document independence: the parameter is only disable_exec.
        // Callers pass config.disable_exec, not disable_web_ssh.
        let r = execute_exec("true", false);
        // On non-unix this still returns a platform message or runs; just ensure not the disabled path.
        assert_ne!(r.output, "Remote control is disabled.");
    }

    #[test]
    fn json_escape_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_escape_newline() {
        assert_eq!(json_escape("a\nb"), r#"a\nb"#);
    }

    #[test]
    fn task_result_body_shape() {
        let body = build_task_result("t1", "ok", 0);
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("\"task_id\":\"t1\""));
        assert!(s.contains("\"exit_code\":0"));
        assert!(s.contains("\"finished_at\""));
    }
}

#[cfg(all(test, feature = "ping"))]
mod ping_retry_tests {
    use super::*;

    #[test]
    fn no_retry_when_latency_under_threshold() {
        let mut calls = 0;
        let rtt = measure_with_retry("icmp", || {
            calls += 1;
            500
        });
        assert_eq!(rtt, 500);
        assert_eq!(calls, 1);
    }

    #[test]
    fn no_retry_on_failure() {
        let mut calls = 0;
        let rtt = measure_with_retry("icmp", || {
            calls += 1;
            -1
        });
        assert_eq!(rtt, -1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn retry_on_high_latency_uses_min() {
        let values = vec![1500i64, 1200, 900];
        let mut idx = 0;
        let rtt = measure_with_retry("icmp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 900);
        assert_eq!(idx, 3);
    }

    #[test]
    fn retry_uses_min_across_all_high_attempts() {
        let values = vec![2000i64, 1500, 1800, 2500];
        let mut idx = 0;
        let rtt = measure_with_retry("http", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 1500);
        assert_eq!(idx, 4);
    }

    #[test]
    fn retry_keeps_best_when_later_attempt_fails() {
        let values = vec![1500i64, 1300, -1];
        let mut idx = 0;
        let rtt = measure_with_retry("icmp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 1300);
        assert_eq!(idx, 3);
    }

    #[test]
    fn tcp_retransmission_detected() {
        // drop = 1200 - 200 = 1000 > 800 → fail immediately
        let values = vec![1200i64, 200, 500, 300];
        let mut idx = 0;
        let rtt = measure_with_retry("tcp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, -1);
        assert_eq!(idx, 2); // stopped after 2nd call
    }

    #[test]
    fn tcp_no_retransmission_when_drop_small() {
        // drop = 1200 - 500 = 700 <= 800 → proceed with retry.
        // Second attempt (500) is under threshold → stops early.
        let values = vec![1200i64, 500, 9999];
        let mut idx = 0;
        let rtt = measure_with_retry("tcp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 500);
        assert_eq!(idx, 2); // stopped after first retry (under threshold, no retransmission)
    }

    #[test]
    fn icmp_no_retransmission_detection() {
        // ICMP with large drop: no retransmission check, just min.
        // Second attempt (100) is under threshold → stops early.
        let values = vec![1200i64, 100, 9999];
        let mut idx = 0;
        let rtt = measure_with_retry("icmp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 100);
        assert_eq!(idx, 2); // stopped after first retry (under threshold)
    }

    #[test]
    fn http_no_retransmission_detection() {
        // HTTP with large drop: no retransmission check, just min
        let values = vec![2000i64, 500];
        let mut idx = 0;
        let rtt = measure_with_retry("http", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 500);
        assert_eq!(idx, 2);
    }

    #[test]
    fn retry_stops_early_when_under_threshold() {
        // 2000 → 1500 → 800 (under 1000) — 4th value never consumed
        let values = vec![2000i64, 1500, 800, 9999];
        let mut idx = 0;
        let rtt = measure_with_retry("icmp", || {
            let v = values[idx];
            idx += 1;
            v
        });
        assert_eq!(rtt, 800);
        assert_eq!(idx, 3);
    }
}
