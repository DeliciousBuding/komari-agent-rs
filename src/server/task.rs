//! Task types for server-sent commands (exec, ping, terminal).
//!
//! Stub phase — task structs are defined but not yet processed by
//! real handlers.  Full implementation will integrate with the FSM
//! for state transitions and with `crate::monitor` for result reporting.
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
