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
pub mod netstatic;
pub mod os;
pub mod process;
pub mod uptime;
pub mod virtualization;

// ── run_with_timeout ──────────────────────────────────────────────────────────
// Shared helper: spawn a command, wait with a deadline, kill on timeout.
// Returns io::Result<Output> like Command::output(), but with a timeout.
// Pattern matches execute_exec in server/task.rs (30 s deadline, kill on expiry).

/// Run a command with a timeout. Spawns the child, polls with
/// [`std::process::Child::try_wait`], kills on timeout, and returns the
/// captured output (stdout + stderr).  The pipes are drained after the
/// process exits so the output is always collected even for a killed child.
///
/// Returns `Err(io::ErrorKind::TimedOut)` when the deadline expires; the
/// `Output` inside is lost (callers that need partial output should use the
/// raw spawn + poll pattern directly).
#[allow(dead_code)]
pub fn run_with_timeout(
    cmd: &mut std::process::Command,
    timeout_secs: u64,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Take ownership of the child's stdout/stderr so we can drain pipes
    // after the process exits without deadlocking.
    let mut child_stdout = child.stdout.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                // Timeout — kill the child and bail out.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    // Drain whatever the child wrote before exiting (or being killed).
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let _ = child_stdout.read_to_end(&mut stdout_buf);
    let _ = child_stderr.read_to_end(&mut stderr_buf);

    match exit_status {
        Some(status) => Ok(std::process::Output {
            status,
            stdout: stdout_buf,
            stderr: stderr_buf,
        }),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("command timed out after {}s", timeout_secs),
        )),
    }
}

// ── MonitorErr ───────────────────────────────────────────────────────────────

/// Errors that can occur during a monitoring tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorErr {
    /// The scratch arena has insufficient space for the report.
    #[allow(dead_code)]
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
    /// Optional monthly traffic persistence, enabled when
    /// [`Config::month_rotate`] != 0.  When present, the report's `totalUp` /
    /// `totalDown` come from these persisted month-to-date totals instead of
    /// the raw process-lifetime interface counters.
    pub netstatic: Option<netstatic::NetStatic>,
    /// Previous aggregate raw TX counter (sum across interfaces) — used to
    /// derive per-tick byte deltas fed into [`netstatic`].
    prev_total_up: u64,
    /// Previous aggregate raw RX counter (sum across interfaces).
    prev_total_down: u64,
    /// False until the first tick seeds [`prev_total_up`] / [`prev_total_down`].
    net_total_ready: bool,
}

impl Monitor {
    /// Create a fresh `Monitor` with no previous samples and netstatic
    /// disabled (equivalent to `month_rotate == 0`).
    pub fn new() -> Self {
        Self {
            prev_cpu: cpu::PrevCpu::default(),
            prev_net: net::PrevNetSnapshot::new(),
            netstatic: None,
            prev_total_up: 0,
            prev_total_down: 0,
            net_total_ready: false,
        }
    }

    /// Create a `Monitor`, enabling monthly traffic persistence when
    /// [`Config::month_rotate`] != 0.
    ///
    /// When enabled, the netstatic database is loaded from the persistence
    /// file (creating a fresh one if absent), and the report's `totalUp` /
    /// `totalDown` will reflect the month-to-date persisted totals.  When
    /// disabled (`month_rotate == 0`) the monitor behaves exactly like
    /// [`new`](Self::new) — process-lifetime interface counters are reported
    /// and no file I/O occurs.
    pub fn new_with_config(config: &Config) -> Self {
        let mut mon = Self::new();
        if config.month_rotate != 0 {
            mon.netstatic = Some(load_or_create_netstatic(config));
        }
        mon
    }

    /// Execute one monitoring tick.
    ///
    /// Resets the arena, collects all metrics, encodes them as JSON, and
    /// returns a `&[u8]` slice pointing into the arena.  The slice is valid
    /// until the next [`ScratchArena::reset`].
    #[allow(dead_code)]
    pub fn tick<'a>(
        &mut self,
        arena: &'a mut ScratchArena,
        config: &Config,
    ) -> Result<&'a [u8], MonitorErr> {
        Ok(generate_report(self, arena, config))
    }
}

/// Default persistence path for the monthly netstatic database.
///
/// Lives in the OS temp dir as a single-file JSON store; matches the task
/// spec's `<temp>/komari-netstatic.json` design.  Kept here so both the
/// initial load and every [`NetStatic::save`] use the same path derivation.
fn netstatic_default_path() -> String {
    let mut path = std::env::temp_dir();
    path.push("komari-netstatic.json");
    path.to_string_lossy().into_owned()
}

/// Load the monthly netstatic database, or create a fresh one on miss.
///
/// On any read/parse failure we fall back to a new empty instance rather than
/// failing the whole agent — a corrupt store should never block monitoring.
fn load_or_create_netstatic(config: &Config) -> netstatic::NetStatic {
    let path = netstatic_default_path();
    match netstatic::NetStatic::load(&path) {
        Ok(mut ns) => {
            // Apply rotation on startup so a stale month is cleared before
            // the first tick contributes to it.
            ns.maybe_reset(config);
            ns
        }
        Err(_) => netstatic::NetStatic::new(&path),
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

    let report = arena.alloc_bytes(written);
    report.copy_from_slice(&scratch[..written]);
    // Return ONLY the report slice. During encode_report, collect_cpu and
    // collect_ip allocate CPU name / IP strings into this same arena BEFORE
    // the report bytes; arena.as_bytes() would prepend those strings and
    // corrupt the JSON (observed as a server "Invalid JSON" error in production).
    &report[..]
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

    // ── Monthly traffic persistence (Go `--month-rotate` parity) ──
    //
    // When month_rotate != 0 the report's `totalUp` / `totalDown` come from
    // the netstatic month-to-date totals instead of the raw process-lifetime
    // interface counters.  Per-tick byte deltas are accumulated and persisted
    // every tick so a restart resumes the running monthly total.
    //
    // The previous aggregate counters are read into locals before taking the
    // netstatic field borrow, then written back after — this keeps the field
    // borrow disjoint from the other `monitor` field accesses.
    let prev_up = monitor.prev_total_up;
    let prev_down = monitor.prev_total_down;
    let ready = monitor.net_total_ready;
    // Snapshot the raw aggregate counters *before* they are overwritten by
    // the netstatic month-to-date totals — these are the per-tick delta
    // baseline for the next tick.
    let raw_total_up = net_total_up;
    let raw_total_down = net_total_down;
    if let Some(ref mut ns) = monitor.netstatic {
        // Derive this tick's byte delta from the raw aggregate counters.
        // wrapping_sub mirrors the kernel counter-wrap handling used by the
        // per-interface delta path; the first tick seeds the baseline without
        // contributing a (huge) bogus delta.
        if ready {
            let up_delta = raw_total_up.wrapping_sub(prev_up);
            let down_delta = raw_total_down.wrapping_sub(prev_down);
            ns.update(up_delta, down_delta);
        }

        // Roll over to the current billing month if the day boundary passed.
        ns.maybe_reset(config);

        // Persist on every tick — the file is a single-line JSON (~100 B),
        // so the I/O cost is negligible relative to a 1 s monitoring cadence.
        let _ = ns.save();

        // Replace the wire-format totals with the persisted month-to-date.
        net_total_up = ns.tx_bytes;
        net_total_down = ns.rx_bytes;
    }
    // Seed the next tick's delta baseline from the raw counters (NOT the
    // persisted month-to-date totals, which reset on rotation).
    monitor.prev_total_up = raw_total_up;
    monitor.prev_total_down = raw_total_down;
    monitor.net_total_ready = true;

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

    // ── IP: DISABLED 2026-06-21 — collect_ip ran every 1s tick but its result
    // was discarded (_ipv4/_ipv6 unused, not in monitoring JSON), opening up to
    // 7 outbound HTTP dials per tick (~420/min) to IP-echo services, violating
    // the <3MB-RSS / daemon goal. Re-enable only if basicInfo actually needs
    // live IPs, and then cache once at startup, not per-tick.
    // let (_ipv4, _ipv6) = ip::collect_ip(config).unwrap_or((None, None));

    // ══════════════════════════════════════════════════════════════════════
    // Build JSON matching Go komari-agent wire format exactly.
    // Reference: D:/Code/Projects/external/komari-agent-go/monitoring/monitoring.go
    // ══════════════════════════════════════════════════════════════════════

    j.begin_obj()?;

    // CPU
    j.begin_obj_field(Field::Cpu)?;
    let cpu_usage = sanitize_percent(cpu.usage).max(0.001);
    j.f64_field(Field::Usage, cpu_usage)?;
    j.end_obj()?;

    // RAM
    let (ram_total, ram_used) = sanitize_used_total(mem.total, mem.used);
    j.begin_obj_field(Field::Ram)?;
    j.u64_field(Field::Total, ram_total)?;
    j.u64_field(Field::Used, ram_used)?;
    j.end_obj()?;

    // Swap
    let (swap_total, swap_used) = sanitize_used_total(mem.swap_total, mem.swap_used);
    j.begin_obj_field(Field::Swap)?;
    j.u64_field(Field::Total, swap_total)?;
    j.u64_field(Field::Used, swap_used)?;
    j.end_obj()?;

    // Load
    j.begin_obj_field(Field::Load)?;
    j.f64_field(Field::Load1, load.load1)?;
    j.f64_field(Field::Load5, load.load5)?;
    j.f64_field(Field::Load15, load.load15)?;
    j.end_obj()?;

    // Disk
    let (disk_total, disk_used) = sanitize_used_total(disk_total, disk_used);
    j.begin_obj_field(Field::Disk)?;
    j.u64_field(Field::Total, disk_total)?;
    j.u64_field(Field::Used, disk_used)?;
    j.end_obj()?;

    // Network — up/down are bytes/s as uint64 (matches Go wire format),
    // NOT float. Emitting "0.0" here made the server reject "Invalid report
    // format".
    j.begin_obj_field(Field::Net)?;
    j.u64_field(Field::Up, net_up)?;
    j.u64_field(Field::Down, net_down)?;
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

    // GPU — only included when enabled (matches Go behavior)
    if config.enable_gpu {
        match gpu::detect_gpus() {
            Ok((_backend, gpus)) => {
                j.begin_obj_field(Field::Gpu)?;

                // Device count
                j.u64_field(Field::Count, gpus.len() as u64)?;

                // Average GPU utilisation
                let total_util: f64 = gpus.iter().map(|g| sanitize_percent(g.utilization)).sum();
                let avg_usage = if gpus.is_empty() {
                    0.0
                } else {
                    total_util / gpus.len() as f64
                };
                j.f64_field(Field::AverageUsage, avg_usage)?;

                // Detailed info array
                if !gpus.is_empty() {
                    j.begin_arr_field(Field::DetailedInfo)?;
                    for gpu_info in gpus.iter() {
                        let (memory_total, memory_used) =
                            sanitize_used_total(gpu_info.memory_total, gpu_info.memory_used);
                        let utilization = sanitize_percent(gpu_info.utilization);
                        j.begin_obj()?;
                        j.str_field(Field::Name, &gpu_info.name)?;
                        j.u64_field(Field::MemoryTotal, memory_total)?;
                        j.u64_field(Field::MemoryUsed, memory_used)?;
                        j.f64_field(Field::Utilization, utilization)?;
                        j.u64_field(Field::Temperature, gpu_info.temperature)?;
                        j.end_obj()?;
                    }
                    j.end_arr()?;
                }

                j.end_obj()?;
            }
            Err(e) => {
                // GPU detection failed — emit empty object, note error.
                message.push_str(&format!("gpu error: {}; ", e));
                j.begin_obj_field(Field::Gpu)?;
                j.end_obj()?;
            }
        }
    }

    // Message (error accumulation, empty on success)
    j.str_field(Field::Message, &message)?;

    j.end_obj()?; // close top-level object

    Ok(j.finish().len())
}

fn sanitize_percent(value: f64) -> f64 {
    if value.is_nan() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

fn sanitize_used_total(total: u64, used: u64) -> (u64, u64) {
    if total == 0 {
        return (0, 0);
    }
    (total, used.min(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_percent_bounds_metric_percentages() {
        assert_eq!(sanitize_percent(-10.0), 0.0);
        assert_eq!(sanitize_percent(0.001), 0.001);
        assert_eq!(sanitize_percent(42.5), 42.5);
        assert_eq!(sanitize_percent(150.0), 100.0);
        assert_eq!(sanitize_percent(f64::NAN), 0.0);
        assert_eq!(sanitize_percent(f64::INFINITY), 100.0);
    }

    #[test]
    fn sanitize_used_total_never_reports_used_above_total() {
        assert_eq!(sanitize_used_total(100, 40), (100, 40));
        assert_eq!(sanitize_used_total(100, 140), (100, 100));
        assert_eq!(sanitize_used_total(0, 140), (0, 0));
    }
}
