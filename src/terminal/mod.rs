// terminal/mod.rs — Feature-gated PTY / ConPTY stubs for interactive Web SSH.
//
// Enabled via `#[cfg(feature = "terminal")]`.  Excluded from the default
// <600 KB core; adds ≈60 KB when compiled in.
//
// References:
//   - komari-agent-go/terminal/terminal.go (interface, WebSocket loop)
//   - komari-agent-go/terminal/terminal_unix.go (PTY via posix_openpt/fork)
//   - komari-agent-go/terminal/terminal_windows.go (ConPTY via CreatePseudoConsole)
//   - docs/plan/spec.md DD11

use std::fmt;
use std::time::Duration;

use crate::ws::{WsConnection, WsErr, WsMessage};

// ── TerminalErr ─────────────────────────────────────────────────────────────

/// Errors that can occur during terminal operations.
#[derive(Debug)]
pub enum TerminalErr {
    /// Underlying I/O error (PTY fd or pipe read/write).
    Io(std::io::Error),
    /// PTY / ConPTY master could not be opened or configured.
    PtyOpen(&'static str),
    /// fork() system call failed (Unix).
    Fork(&'static str),
    /// execvp / CreateProcessW failed.
    Exec(&'static str),
    /// Window resize ioctl / ResizePseudoConsole failed.
    Resize(&'static str),
    /// WebSocket connection, handshake, or frame error.
    WebSocket(String),
}

impl fmt::Display for TerminalErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "terminal I/O error: {e}"),
            Self::PtyOpen(s) => write!(f, "PTY open error: {s}"),
            Self::Fork(s) => write!(f, "fork error: {s}"),
            Self::Exec(s) => write!(f, "exec error: {s}"),
            Self::Resize(s) => write!(f, "resize error: {s}"),
            Self::WebSocket(s) => write!(f, "WebSocket error: {s}"),
        }
    }
}

impl From<std::io::Error> for TerminalErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<WsErr> for TerminalErr {
    fn from(e: WsErr) -> Self {
        Self::WebSocket(e.to_string())
    }
}

impl std::error::Error for TerminalErr {}

// ── Terminal trait ──────────────────────────────────────────────────────────

/// Platform-agnostic terminal trait.
///
/// Implementors own the PTY / ConPTY handles and the child shell process.
pub trait Terminal: Send {
    /// Read output from the terminal into `buf`.
    /// Returns the number of bytes read (0 means EOF).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalErr>;

    /// Write input to the terminal.
    /// Returns the number of bytes written.
    fn write(&mut self, data: &[u8]) -> Result<usize, TerminalErr>;

    /// Resize the terminal window in character cells.
    fn resize(&mut self, cols: u16, rows: u16) -> Result<(), TerminalErr>;

    /// Close the terminal and terminate the child process.
    fn close(&mut self) -> Result<(), TerminalErr>;
}

// ── Platform modules ────────────────────────────────────────────────────────

#[cfg(target_family = "unix")]
mod unix;
#[cfg(target_family = "unix")]
use unix::UnixTerminal;

#[cfg(target_family = "windows")]
mod windows;
#[cfg(target_family = "windows")]
use windows::WindowsTerminal;

// ── Entry point ─────────────────────────────────────────────────────────────

/// WebSocket read poll timeout during the terminal copy loop.
///
/// We are a sync single-threaded agent and cannot spawn a WS→PTY and PTY→WS
/// reader pair in parallel, so we poll both directions by alternating: read
/// the WebSocket with a short read timeout (non-blocking-ish), drain any PTY
/// output, repeat.  50 ms keeps the loop responsive to keystrokes without
/// busy-spinning.
const WS_POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// Default blocking read timeout restored on the WS socket once the session
/// ends, so the caller (which may reuse the dialer / connection) is not left
/// with a 50 ms timeout.
const WS_RESTORE_TIMEOUT: Duration = Duration::from_secs(30);

/// Launch an interactive terminal session over an already-connected WebSocket.
///
/// `ws` — an established `WsConnection` (handshake complete) that the caller
/// has upgraded into terminal mode.  The terminal owns this connection for the
/// duration of the session and restores its read timeout on exit.
///
/// Spawns a platform PTY / ConPTY, then runs a single-threaded bidirectional
/// copy loop between the WebSocket and the terminal, modelled on
/// `komari-agent-go/terminal/terminal.go :: StartTerminal` but polled instead
/// of goroutine-driven (this agent is sync, no async runtime).
///
/// Protocol (mirrors the Go agent):
///   - WS text message  → JSON `{"type":"input","input":"…"}` writes `input`
///                        to the PTY;
///                        JSON `{"type":"resize","cols":N,"rows":N}` resizes.
///   - WS binary frame  → written verbatim to the PTY.
///   - WS close / EOF   → session ends.
///   - PTY output       → sent as a WS binary frame.
///
/// Returns `Ok(())` on a clean close, or an error if either side fails.
pub fn start_terminal(ws: &mut WsConnection) -> Result<(), TerminalErr> {
    #[cfg(target_family = "unix")]
    let mut term: Box<dyn Terminal> = Box::new(UnixTerminal::spawn("/bin/sh")?);
    #[cfg(target_family = "windows")]
    let mut term: Box<dyn Terminal> = Box::new(WindowsTerminal::spawn("cmd.exe")?);

    // Run the copy loop; `run_copy_loop` always restores the WS read timeout
    // and closes the PTY before returning (even on error).
    let result = run_copy_loop(ws, &mut *term);

    // Graceful shutdown: best-effort.  Close the PTY first (which the
    // terminal's Drop would do anyway), then close the WS.  We intentionally
    // keep this minimal relative to the Go agent's Ctrl+C×3 / Ctrl+D / "exit"
    // dance: closing the PTY master ends the child session deterministically
    // without sending shell-level signals that depend on the child's state.
    let _ = term.close();
    // Close the WS handshake (best-effort; ignore errors — we're tearing down).
    let _ = ws.close();

    result
}

/// The bidirectional copy loop.
///
/// Polls the WebSocket (50 ms read timeout) and drains the PTY each
/// iteration.  Exits when:
///   - the WS yields `Close` or `Ok(None)` (clean EOF), or
///   - a read returns a non-timeout error, or
///   - sending a PTY→WS frame fails.
///
/// The PTY read is also non-blocking-ish: WS short timeout bounds the time
/// between PTY drains, so PTY output is flushed at most every ~50 ms.  The PTY
/// itself is read with a blocking call but returns quickly once the child
/// produces output or the pipe closes.
fn run_copy_loop(ws: &mut WsConnection, term: &mut dyn Terminal) -> Result<(), TerminalErr> {
    // Tighten the read timeout for polling.  We restore it on every return path.
    let _ = ws.get_ref().set_read_timeout(Some(WS_POLL_TIMEOUT));

    let result = copy_loop_inner(ws, term);

    let _ = ws.get_ref().set_read_timeout(Some(WS_RESTORE_TIMEOUT));
    result
}

/// Inner loop, separated so the timeout restore always runs exactly once.
fn copy_loop_inner(ws: &mut WsConnection, term: &mut dyn Terminal) -> Result<(), TerminalErr> {
    let mut pty_out = [0u8; 4096];

    loop {
        // ── WS → PTY (polled with the short timeout) ──────────────────────
        match ws.read_message() {
            Ok(Some(WsMessage::Text(data))) => {
                handle_text_command(&data, term)?;
            }
            Ok(Some(WsMessage::Binary(data))) => {
                write_all_to_pty(term, &data)?;
            }
            Ok(Some(WsMessage::Ping(payload))) => {
                // RFC 6455: reply with a pong carrying the same payload.
                ws.send_pong(&payload)?;
            }
            Ok(Some(WsMessage::Pong(_))) => {
                // Unsolicited pong — ignore.
            }
            Ok(Some(WsMessage::Close)) | Ok(None) => {
                // Clean shutdown from the server side.
                return Ok(());
            }
            Err(e) if is_ws_timeout(&e) => {
                // No WS data this poll — fall through to drain the PTY.
            }
            Err(e) => {
                return Err(TerminalErr::WebSocket(e.to_string()));
            }
        }

        // ── PTY → WS (drain whatever is available right now) ──────────────
        // The PTY read is blocking; because the WS side just polled (or timed
        // out within 50 ms), we bound the time the PTY can stall the loop by
        // only reading once per iteration.  A 0-byte read (child exited / pipe
        // closed) ends the session.
        match term.read(&mut pty_out) {
            Ok(0) => {
                // PTY EOF: child process exited.
                return Ok(());
            }
            Ok(n) => {
                ws.send_binary(&pty_out[..n])?;
            }
            // PTYs surface non-blocking as an I/O error on some platforms;
            // treat would-block as "no output this iteration" and continue.
            Err(TerminalErr::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // No PTY output ready; continue polling.
            }
            // A NotConnected / BrokenPipe means the child exited and the PTY
            // master / ConPTY pipe is gone — end the session cleanly.
            Err(TerminalErr::Io(ref e))
                if e.kind() == std::io::ErrorKind::NotConnected
                    || e.kind() == std::io::ErrorKind::BrokenPipe =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
}

/// Parse a WS text message as a terminal control command.
///
/// Recognises two JSON shapes (no serde — manual field extraction to keep the
/// agent dependency-free):
///   - `{"type":"input","input":"<bytes>"}`
///   - `{"type":"resize","cols":<n>,"rows":<n>}`
///
/// Unknown / malformed text is written to the PTY verbatim, matching the Go
/// agent's fallback (`term.Write(p)` when JSON unmarshal fails).
fn handle_text_command(data: &[u8], term: &mut dyn Terminal) -> Result<(), TerminalErr> {
    if let Some(cmd) = parse_terminal_message(data) {
        match cmd {
            TerminalCommand::Input(bytes) => {
                if !bytes.is_empty() {
                    write_all_to_pty(term, &bytes)?;
                }
            }
            TerminalCommand::Resize { cols, rows } => {
                if cols > 0 && rows > 0 {
                    // Best-effort resize: a failure to resize should not kill
                    // the interactive session.
                    let _ = term.resize(cols, rows);
                }
            }
        }
        Ok(())
    } else {
        // Not a recognised command — write raw bytes to the PTY (Go parity).
        write_all_to_pty(term, data)
    }
}

/// Write `data` to the PTY, retrying on partial writes.
fn write_all_to_pty(term: &mut dyn Terminal, mut data: &[u8]) -> Result<(), TerminalErr> {
    while !data.is_empty() {
        let n = term.write(data)?;
        if n == 0 {
            return Err(TerminalErr::Io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "PTY wrote zero bytes",
            )));
        }
        data = &data[n..];
    }
    Ok(())
}

/// Parsed terminal control message.
enum TerminalCommand {
    /// Raw input bytes to feed to the shell.
    Input(Vec<u8>),
    /// Window resize in character cells.
    Resize { cols: u16, rows: u16 },
}

/// Minimal, allocation-light JSON field extractor for terminal control frames.
///
/// We only ever need `type`, `input`, `cols`, `rows` from two known shapes, so
/// a hand-rolled scan is both smaller (no serde) and sufficient.  Returns
/// `None` if the payload is not a JSON object we understand — the caller falls
/// back to writing the raw bytes to the PTY.
fn parse_terminal_message(data: &[u8]) -> Option<TerminalCommand> {
    // Quick shape check: must be a JSON object.
    let s = std::str::from_utf8(data).ok()?;
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }

    let kind = json_string_field(s, "type")?;
    match kind.as_str() {
        "input" => {
            let input = json_string_field(s, "input").unwrap_or_default();
            Some(TerminalCommand::Input(input.into_bytes()))
        }
        "resize" => {
            let cols = json_number_field(s, "cols")?;
            let rows = json_number_field(s, "rows")?;
            Some(TerminalCommand::Resize {
                cols: cols as u16,
                rows: rows as u16,
            })
        }
        _ => None,
    }
}

/// Extract a JSON string field's value from `s` by key, without a full parser.
///
/// Looks for `"key"` followed by `:` and a string value.  Handles the small
/// set of escapes we expect in terminal input (`\\`, `\"`, `\n`, `\r`, `\t`).
/// Returns `None` if the field is absent or not a string.
fn json_string_field(s: &str, key: &str) -> Option<String> {
    // Build the needle `"key"` (quoted) so we match the JSON key, not a
    // substring of a value.
    let mut needle = String::with_capacity(key.len() + 2);
    needle.push('"');
    needle.push_str(key);
    needle.push('"');

    let mut rest = s;
    // Find the key; skip past any earlier occurrence that is inside a value by
    // a simple scan (good enough for our flat two-field objects).
    loop {
        let idx = rest.find(needle.as_str())?;
        let after = &rest[idx + needle.len()..];
        // Skip whitespace then expect ':'.
        let after = after.trim_start();
        if !after.starts_with(':') {
            rest = &rest[idx + needle.len()..];
            continue;
        }
        let value_start = after[1..].trim_start();
        return parse_json_string(value_start);
    }
}

/// Parse a JSON string literal at the start of `s`, returning its decoded value.
///
/// Operates on the validated-UTF-8 `&str`.  Because `s` is UTF-8, byte indices
/// that fall on `"` / `\` (all ASCII) are always char boundaries, so indexing
/// the byte slice between them yields a valid UTF-8 slice — even when it
/// contains multi-byte characters.  We collect raw slices for normal bytes and
/// only special-case backslash escapes.
fn parse_json_string(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.first()? != &b'"' {
        return None;
    }

    let mut out = String::new();
    let mut i = 1; // skip opening quote
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            return Some(out);
        } else if c == b'\\' {
            i += 1;
            if i >= bytes.len() {
                return None;
            }
            match bytes[i] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'b' => out.push('\u{0008}'),
                b'f' => out.push('\u{000C}'),
                // `\uXXXX` — decode the BMP code point.
                b'u' if i + 4 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 5]).ok()?;
                    let cp = u32::from_str_radix(hex, 16).ok()?;
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(ch);
                    }
                    i += 4;
                }
                _ => return None,
            }
            i += 1;
        } else {
            // Find the next ASCII special char (`"` or `\`) so we can copy the
            // intervening bytes (which may include multi-byte UTF-8) as one
            // slice rather than byte-by-byte.
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\\' {
                i += 1;
            }
            // `s` is validated UTF-8, and `start..i` excludes `"`/`\`, so this
            // slice is a valid UTF-8 substring.
            out.push_str(&s[start..i]);
        }
    }
    None
}

/// Extract a JSON numeric field's value as `u64`.
///
/// Looks for `"key"` followed by `:` and an unsigned integer literal.
fn json_number_field(s: &str, key: &str) -> Option<u64> {
    let mut needle = String::with_capacity(key.len() + 2);
    needle.push('"');
    needle.push_str(key);
    needle.push('"');

    let mut rest = s;
    loop {
        let idx = rest.find(needle.as_str())?;
        let after = &rest[idx + needle.len()..];
        let after = after.trim_start();
        if !after.starts_with(':') {
            rest = &rest[idx + needle.len()..];
            continue;
        }
        let value_start = after[1..].trim_start();
        // Read consecutive ASCII digits.
        let digits: String = value_start.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        return digits.parse::<u64>().ok();
    }
}

/// Detect WS read timeouts (mirrors server/reconnection.rs :: is_timeout).
///
/// A timeout is the normal "no data this poll" signal, not an error.
fn is_ws_timeout(e: &WsErr) -> bool {
    matches!(e, WsErr::Io(s) if {
        let s = s.to_lowercase();
        s.contains("timed out")
            || s.contains("would block")
            || s.contains("temporarily unavailable")
    })
}
