//! Linux memory metrics from `/proc/meminfo`.
//!
//! Mode 0 (htop-like): used = total - (free + buffers + cached + sreclaimable).
//! Mode 1 (default):    used = total - MemAvailable.
//! Mode 2:              run `free -b`, parse Used column.
//!
//! All values are bytes. Swap is always sourced from `/proc/meminfo`.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::Command;

use crate::config::Config;
use crate::monitor::run_with_timeout;

/// Collected memory metrics (bytes).
#[derive(Debug, Default, Clone, Copy)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

/// Parsed `/proc/meminfo` fields (kB → bytes).
struct ProcMem {
    total: u64,
    free: u64,
    available: u64,
    buffers: u64,
    cached: u64,
    s_reclaimable: u64,
    shmem: u64,
    swap_cached: u64,
    zswap: u64,
    zswapped: u64,
    swap_total: u64,
    swap_free: u64,
}

fn read_proc_meminfo() -> Option<ProcMem> {
    let file = File::open("/proc/meminfo").ok()?;
    let mut m = ProcMem {
        total: 0,
        free: 0,
        available: 0,
        buffers: 0,
        cached: 0,
        s_reclaimable: 0,
        shmem: 0,
        swap_cached: 0,
        zswap: 0,
        zswapped: 0,
        swap_total: 0,
        swap_free: 0,
    };
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let mut parts = line.split_whitespace();
        let key = parts.next()?.trim_end_matches(':');
        let val_str = parts.next()?;
        let parsed: u64 = val_str.parse().ok()?;
        let val: u64 = parsed * 1024; // kB → bytes
        match key {
            "MemTotal" => m.total = val,
            "MemFree" => m.free = val,
            "MemAvailable" => m.available = val,
            "Buffers" => m.buffers = val,
            "Cached" => m.cached = val,
            "SReclaimable" => m.s_reclaimable = val,
            "Shmem" => m.shmem = val,
            "SwapCached" => m.swap_cached = val,
            "Zswap" => m.zswap = val,
            "Zswapped" => m.zswapped = val,
            "SwapTotal" => m.swap_total = val,
            "SwapFree" => m.swap_free = val,
            _ => {}
        }
    }
    if m.total == 0 { None } else { Some(m) }
}

#[inline]
fn swap_used(m: &ProcMem) -> u64 {
    m.swap_total
        .saturating_sub(m.swap_free)
        .saturating_sub(m.swap_cached)
}

/// Mode 0: htop-like.
fn mode_0(m: &ProcMem) -> MemInfo {
    let d = m.free + m.buffers + m.cached + m.s_reclaimable;
    MemInfo {
        total: m.total,
        used: m.total.saturating_sub(d).saturating_add(m.shmem),
        swap_total: m.swap_total,
        swap_used: swap_used(m),
    }
}

/// Mode 1 (default): gopsutil-like.
fn mode_1(m: &ProcMem) -> MemInfo {
    MemInfo {
        total: m.total,
        used: m.total.saturating_sub(m.available),
        swap_total: m.swap_total,
        swap_used: swap_used(m),
    }
}

/// Mode 2: `free -b` subprocess. Swap from `/proc/meminfo`.
fn mode_2() -> Option<MemInfo> {
    let out = run_with_timeout(&mut Command::new("free").arg("-b"), 30).ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&out.stdout).ok()?;
    for line in stdout.lines().skip(1) {
        if let Some(rest) = line.strip_prefix("Mem:") {
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() >= 2 {
                let total: u64 = f[0].parse().ok()?;
                let used: u64 = f[1].parse().ok()?;
                let (st, su) = read_proc_meminfo()
                    .map(|m| (m.swap_total, swap_used(&m)))
                    .unwrap_or((0, 0));
                return Some(MemInfo {
                    total,
                    used,
                    swap_total: st,
                    swap_used: su,
                });
            }
            break;
        }
    }
    None
}

/// Collect memory + swap. Priority (mirrors Go komari-agent):
/// 1. `memory_include_cache` → used = total - free (cache/buffer counted as used)
/// 2. `memory_report_raw_used` → mode 0 (htop-like)
/// 3. Default (both false) → mode 1 (gopsutil-like: total - MemAvailable)
/// 4. Fallback when /proc/meminfo unavailable → mode 2 (`free -b`)
pub fn collect(config: &Config) -> MemInfo {
    // 1. memory_include_cache: used = total - free (includes cache/buffer)
    if config.memory_include_cache
        && let Some(m) = read_proc_meminfo()
    {
        return MemInfo {
            total: m.total,
            used: m.total.saturating_sub(m.free),
            swap_total: m.swap_total,
            swap_used: swap_used(&m),
        };
    }
    // 2. memory_report_raw_used: htop-like (total - free - buffers - cached - sreclaimable)
    if config.memory_report_raw_used
        && let Some(m) = read_proc_meminfo()
    {
        return mode_0(&m);
    }
    // 3. Default: gopsutil-like (total - MemAvailable)
    if let Some(m) = read_proc_meminfo() {
        return mode_1(&m);
    }
    // 4. Last resort: `free -b` subprocess
    if let Some(mi) = mode_2() {
        return mi;
    }
    MemInfo::default()
}
