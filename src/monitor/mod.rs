//! Monitor orchestration — collect system metrics, encode as JSON, zero allocation.
//!
//! # Architecture
//! - [`Monitor`] holds platform-specific previous-sample state used by delta
//!   collectors (CPU usage %, network bytes/sec).  It does **not** own the
//!   scratch arena — the caller passes one in each tick so it can be reused
//!   across the event loop.
//! - [`generate_report`] is the public entry point: it calls every collector,
//!   assembles the flat JSON payload inside [`ScratchArena`], and returns a
//!   `&[u8]` slice valid until the next arena reset.
//! - All string data lives inside the arena; no heap allocation in the hot path.
//!
//! # Sub-modules
//! Each collector has a per-platform implementation:
//! `cpu`, `mem`, `disk`, `net`, `load`, `connections`, `process`, `uptime`,
//! `ip`, `gpu`.

use crate::arena::ScratchArena;
use crate::json::{Field, JsonBuf, JsonErr};

// ── Sub-module declarations ──────────────────────────────────────────────────

pub mod connections;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod ip;
pub mod load;
pub mod mem;
pub mod net;
pub mod process;
pub mod uptime;

// ── State types ───────────────────────────────────────────────────────────────

/// Previous CPU sample for usage-percentage delta calculation.
///
/// Stored as raw `/proc/stat` jiffy counts (or platform equivalent).
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuSample {
    pub total: u64,
    pub idle: u64,
}

/// Previous network sample for bytes/sec rate calculation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetSample {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

// ── MonitorErr ───────────────────────────────────────────────────────────────

/// Errors that can occur during a monitoring tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorErr {
    /// The scratch arena has insufficient space for the report.
    ArenaFull,
    /// JSON encoding failed (buffer exhausted or nesting depth > 8).
    EncodeError,
}

impl From<JsonErr> for MonitorErr {
    #[inline]
    fn from(_: JsonErr) -> Self {
        MonitorErr::EncodeError
    }
}

// ── Monitor ──────────────────────────────────────────────────────────────────

/// Aggregated monitoring state, holding only the previous-sample data needed
/// for delta calculations.  The arena is caller-owned so it can be shared with
/// WS framing and protocol encoding in the event loop.
pub struct Monitor {
    /// Previous CPU sample for usage-percentage delta.
    pub prev_cpu: Option<CpuSample>,
    /// Previous network sample for bytes/sec delta.
    pub prev_net: Option<NetSample>,
}

impl Monitor {
    /// Create a fresh `Monitor` with no previous samples.
    pub fn new() -> Self {
        Self {
            prev_cpu: None,
            prev_net: None,
        }
    }

    /// Execute one monitoring tick.
    ///
    /// Resets the arena, collects all metrics, encodes them as JSON, and
    /// returns a `&[u8]` slice pointing into the arena.  The slice is valid
    /// until the next [`ScratchArena::reset`].
    pub fn tick<'a>(&mut self, arena: &'a mut ScratchArena) -> Result<&'a [u8], MonitorErr> {
        Ok(generate_report(self, arena))
    }
}

// ── generate_report ──────────────────────────────────────────────────────────

/// Collect all metrics and assemble the full Komari JSON report into `arena`.
///
/// Returns a `&[u8]` slice pointing into `arena`.  The slice is valid until
/// the next [`ScratchArena::reset`] or until the arena is dropped.
///
/// This is the main entry point called by the event loop each tick.
pub fn generate_report<'a>(monitor: &mut Monitor, arena: &'a mut ScratchArena) -> &'a [u8] {
    arena.reset();

    // Build JSON in a small stack buffer, then copy into the arena.
    // 4096 bytes comfortably holds the full monitoring report (typically 2-3 KB).
    // No heap allocation — both `scratch` and the arena buffer live on the stack
    // (the arena is 64 KB, owned by the caller).
    let mut scratch = [0u8; 4096];
    let mut written = match encode_report(monitor, &mut scratch) {
        Ok(n) => n,
        Err(_) => {
            // On encode failure, return a minimal valid JSON object.
            // Should never happen with a 4 KB stack buffer.
            scratch[0] = b'{';
            scratch[1] = b'}';
            2
        }
    };

    // Defensive: if arena cannot hold the report (should never happen — scratch
    // is 4 KB, arena is 64 KB), fall back to a minimal `{}`.
    if written > arena.remaining() {
        let fallback = b"{}";
        written = fallback.len();
        scratch[..written].copy_from_slice(fallback);
    }

    arena
        .alloc_bytes(written)
        .copy_from_slice(&scratch[..written]);
    arena.as_bytes()
}

// ── encode_report (internal) ─────────────────────────────────────────────────

/// Encode the full monitoring JSON into a [`JsonBuf`] byte buffer.
///
/// Returns the number of bytes written.  All values are zero until the
/// per-platform collectors are wired in.
fn encode_report(monitor: &mut Monitor, buf: &mut [u8]) -> Result<usize, MonitorErr> {
    let mut j = JsonBuf::new(buf);

    // ── Top-level object ──
    j.begin_obj()?;

    // ── CPU ──
    let cpu_usage = collect_cpu(monitor);
    j.begin_obj_field(Field::Cpu)?;
    j.f64_field(Field::Usage, cpu_usage)?;
    j.end_obj()?;

    // ── RAM ──
    j.begin_obj_field(Field::Ram)?;
    j.u64_field(Field::Total, 0)?; // stub
    j.u64_field(Field::Used, 0)?; // stub
    j.end_obj()?;

    // ── Swap ──
    j.begin_obj_field(Field::Swap)?;
    j.u64_field(Field::Total, 0)?;
    j.u64_field(Field::Used, 0)?;
    j.end_obj()?;

    // ── Load ──
    j.begin_obj_field(Field::Load)?;
    j.f64_field(Field::Load1, 0.0)?;
    j.f64_field(Field::Load5, 0.0)?;
    j.f64_field(Field::Load15, 0.0)?;
    j.end_obj()?;

    // ── Disk ──
    j.begin_obj_field(Field::Disk)?;
    j.u64_field(Field::Total, 0)?;
    j.u64_field(Field::Used, 0)?;
    j.end_obj()?;

    // ── Network ──
    j.begin_obj_field(Field::Net)?;
    j.f64_field(Field::Up, 0.0)?;
    j.f64_field(Field::Down, 0.0)?;
    j.u64_field(Field::TotalUp, 0)?;
    j.u64_field(Field::TotalDown, 0)?;
    j.end_obj()?;

    // ── Connections ──
    j.begin_obj_field(Field::Connections)?;
    j.u64_field(Field::Tcp, 0)?;
    j.u64_field(Field::Udp, 0)?;
    j.end_obj()?;

    // ── Process count ──
    j.u64_field(Field::Process, 0)?;

    // ── Uptime (seconds) ──
    j.u64_field(Field::Uptime, 0)?;

    // Close top-level object
    j.end_obj()?;

    Ok(j.finish().len())
}

// ── Stub collectors (replaced by platform modules in later PRs) ──────────────

/// Stub CPU collector — returns 0.0% usage.
///
/// Will be replaced by `crate::monitor::cpu::collect()` once the per-platform
/// implementations are wired in.  The `_monitor` reference is reserved for
/// delta calculation (reading and updating `prev_cpu`).
fn collect_cpu(_monitor: &mut Monitor) -> f64 {
    // TODO: Read /proc/stat (Linux) or platform equivalent, compute delta from
    //       _monitor.prev_cpu, store new sample back.
    0.0
}
