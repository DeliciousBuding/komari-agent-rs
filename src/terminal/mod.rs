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
    WebSocket(&'static str),
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

/// Launch an interactive terminal session connected to the Komari server.
///
/// `token`    — agent authentication token.
/// `id`       — agent ID string.
/// `endpoint` — server base URL (e.g. `wss://monitor.example.com`).
///
/// Spawns a platform PTY / ConPTY, then enters a bidirectional copy loop
/// between the WebSocket and the terminal.  Blocks for the session lifetime.
pub fn start_terminal(_token: &str, _id: &str, _endpoint: &str) -> Result<(), TerminalErr> {
    #[cfg(target_family = "unix")]
    let mut _term = UnixTerminal::spawn("/bin/sh")?;
    #[cfg(target_family = "windows")]
    let mut _term = WindowsTerminal::spawn("cmd.exe")?;

    // TODO: complete the WebSocket handshake with crate::ws, then enter a
    // bidirectional copy loop (WS → term.write, term.read → WS).
    // Model: komari-agent-go/terminal/terminal.go :: StartTerminal.
    Err(TerminalErr::WebSocket(
        "terminal WebSocket loop not yet wired — see komari-agent-go/terminal/terminal.go",
    ))
}
