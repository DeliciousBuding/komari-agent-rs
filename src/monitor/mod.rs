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
//! `ip`, `gpu`, `os`, `virtualization`.

use crate::arena::ScratchArena;
use crate::config::Config;
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
pub mod os;
pub mod process;
pub mod uptime;
pub mod virtualization;

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
    /// Previous CPU jiffy sample for usage-percentage delta.
    pub prev_cpu: cpu::PrevCpu,
    /// Previous network per-interface counters for bytes/sec delta.
    pub prev_net: net::PrevNetSnapshot,
}

impl Monitor {
    /// Create a fresh `Monitor` with no previous samples.
    pub fn new() -> Self {
        Self {
            prev_cpu: cpu::PrevCpu::default(),
            prev_net: net::PrevNetSnapshot::new(),
        }
    }

    /// Execute one monitoring tick.
    ///
    /// Resets the arena, collects all metrics, encodes them as JSON, and
    /// returns a `&[u8]` slice pointing into the arena.  The slice is valid
    /// until the next [`ScratchArena::reset`].
    pub fn tick<'a>(
        &mut self,
        arena: &'a mut ScratchArena,
        config: &Config,
    ) -> Result<&'a [u8], MonitorErr> {
        Ok(generate_report(self, arena, config))
    }
}

// ── generate_report ──────────────────────────────────────────────────────────

/// Collect all metrics and assemble the full Komari JSON report into `arena`.
///
/// Returns a `&[u8]` slice pointing into `arena`.  The slice is valid until
/// the next [`ScratchArena::reset`] or until the arena is dropped.
///
/// This is the main entry point called by the event loop each tick.
pub fn generate_report<'a>(
    monitor: &mut Monitor,
    arena: &'a mut ScratchArena,
    config: &Config,
) -> &'a [u8] {
    arena.reset();

    // Build JSON in a small stack buffer, then copy into the arena.
    // 4096 bytes comfortably holds the full monitoring report (typically 2-3 KB).
    let mut scratch = [0u8; 4096];
    let mut written = match encode_report(monitor, arena, config, &mut scratch) {
        Ok(n) => n,
        Err(_) => {
            // On encode failure, return a minimal valid JSON object.
            scratch[0] = b'{';
            scratch[1] = b'}';
            2
        }
    };

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
/// Collects all metrics via the platform-specific sub-modules, assembles
/// a flat JSON object matching the Go komari-agent wire format exactly,
/// and returns the number of bytes written.
fn encode_report(
    monitor: &mut Monitor,
    arena: &mut ScratchArena,
    config: &Config,
    buf: &mut [u8],
) -> Result<usize, MonitorErr> {
    let mut j = JsonBuf::new(buf);
    let mut message = String::new();

    // ── CPU: /proc/stat delta + /proc/cpuinfo ──
    let cpu = match cpu::collect_cpu(arena, &mut monitor.prev_cpu) {
        Ok(info) => info,
        Err(_e) => {
            message.push_str("cpu error; ");
            // Fallback: arena-alloc placeholder strings, usage = 0.001 (Go minimum).
            let fallback = arena.alloc_bytes(7);
            fallback.copy_from_slice(b"Unknown");
            let fallback_str = unsafe { std::str::from_utf8_unchecked(fallback) };
            cpu::CpuInfo {
                name: fallback_str,
                cores: 0,
                physical_cores: 0,
                arch: fallback_str,
                usage: 0.001,
            }
        }
    };

    // ── Memory + Swap: /proc/meminfo with mode selection ──
    let mem = mem::collect(config);

    // ── Disk: /proc/mounts + statfs ──
    let disks = disk::collect(config);
    let (disk_total, disk_used) = disk::aggregate(&disks);

    // ── Network: /proc/net/dev with per-interface delta ──
    let net_entries = net::collect(config, &mut monitor.prev_net);
    let mut net_up: u64 = 0;
    let mut net_down: u64 = 0;
    let mut net_total_up: u64 = 0;
    let mut net_total_down: u64 = 0;
    for ni in net_entries.iter() {
        net_up += ni.up;
        net_down += ni.down;
        net_total_up += ni.total_up;
        net_total_down += ni.total_down;
    }

    // ── Load: /proc/loadavg ──
    let load = load::collect().unwrap_or_else(|_| {
        message.push_str("load error; ");
        load::LoadInfo::default()
    });

    // ── Connections: /proc/net/tcp + /proc/net/udp ──
    let conns = connections::collect_connections().unwrap_or_else(|_| {
        message.push_str("connections error; ");
        connections::ConnectionsInfo { tcp: 0, udp: 0 }
    });

    // ── Process count: /proc enumeration ──
    let proc_count = process::collect_process_count().unwrap_or_else(|_| {
        message.push_str("process error; ");
        0
    });

    // ── Uptime: /proc/uptime ──
    let uptime_secs = uptime::collect_uptime().unwrap_or_else(|_| {
        message.push_str("uptime error; ");
        0
    });

    // ── IP: NIC + HTTP APIs (collected for basicInfo, not in monitoring JSON) ──
    let (_ipv4, _ipv6) = ip::collect_ip(config).unwrap_or_else(|_| {
        (None, None)
    });

    // ══════════════════════════════════════════════════════════════════════
    // Build JSON matching Go komari-agent wire format exactly.
    // Reference: D:/Code/Projects/external/komari-agent-go/monitoring/monitoring.go
    // ══════════════════════════════════════════════════════════════════════

    j.begin_obj()?;

    // CPU
    j.begin_obj_field(Field::Cpu)?;
    let cpu_usage = if cpu.usage <= 0.001 { 0.001 } else { cpu.usage };
    j.f64_field(Field::Usage, cpu_usage)?;
    j.end_obj()?;

    // RAM
    j.begin_obj_field(Field::Ram)?;
    j.u64_field(Field::Total, mem.total)?;
    j.u64_field(Field::Used, mem.used)?;
    j.end_obj()?;

    // Swap
    j.begin_obj_field(Field::Swap)?;
    j.u64_field(Field::Total, mem.swap_total)?;
    j.u64_field(Field::Used, mem.swap_used)?;
    j.end_obj()?;

    // Load
    j.begin_obj_field(Field::Load)?;
    j.f64_field(Field::Load1, load.load1)?;
    j.f64_field(Field::Load5, load.load5)?;
    j.f64_field(Field::Load15, load.load15)?;
    j.end_obj()?;

    // Disk
    j.begin_obj_field(Field::Disk)?;
    j.u64_field(Field::Total, disk_total)?;
    j.u64_field(Field::Used, disk_used)?;
    j.end_obj()?;

    // Network
    j.begin_obj_field(Field::Net)?;
    j.f64_field(Field::Up, net_up as f64)?;
    j.f64_field(Field::Down, net_down as f64)?;
    j.u64_field(Field::TotalUp, net_total_up)?;
    j.u64_field(Field::TotalDown, net_total_down)?;
    j.end_obj()?;

    // Connections
    j.begin_obj_field(Field::Connections)?;
    j.u64_field(Field::Tcp, conns.tcp as u64)?;
    j.u64_field(Field::Udp, conns.udp as u64)?;
    j.end_obj()?;

    // Process count
    j.u64_field(Field::Process, proc_count as u64)?;

    // Uptime
    j.u64_field(Field::Uptime, uptime_secs)?;

    // GPU stub — only included when enabled (matches Go behavior)
    if config.enable_gpu {
        j.begin_obj_field(Field::Gpu)?;
        j.end_obj()?; // empty {} for now
    }

    // Message (error accumulation, empty on success)
    j.str_field(Field::Message, &message)?;

    j.end_obj()?; // close top-level object

    Ok(j.finish().len())
}
