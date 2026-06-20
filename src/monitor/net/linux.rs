//! Linux network metrics from `/proc/net/dev`.
//!
//! Parses cumulative RX/TX byte counters, filters loopback and virtual
//! interfaces, applies user-configured NIC include/exclude glob patterns,
//! and computes per-second upload/download speeds via delta from the
//! previous monitoring tick.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::Instant;

use crate::arena::{MAX_NETWORKS, SmallVec};
use crate::config::Config;

/// Per-interface network statistics for a single monitoring tick.
///
/// Interface names are stored inline (`[u8; 16]` — Linux IFNAMSIZ = 16) and
/// exposed via [`name()`](NetInfo::name).  No arena borrow needed; each
/// `NetInfo` is fully self-contained.
pub struct NetInfo {
    name_buf: [u8; 16],
    name_len: u8,
    /// Upload speed this tick, bytes/sec.
    pub up: u64,
    /// Download speed this tick, bytes/sec.
    pub down: u64,
    /// Cumulative bytes sent since boot (raw counter).
    pub total_up: u64,
    /// Cumulative bytes received since boot (raw counter).
    pub total_down: u64,
}

impl NetInfo {
    /// Interface name (e.g. "eth0", "ens3").
    #[inline]
    pub fn name(&self) -> &str {
        std::str::from_utf8(&self.name_buf[..self.name_len as usize]).unwrap_or("?")
    }
}

// ── Previous snapshot: stores per-interface cumulative counters ───────────────

/// Fixed-size store of previous-tick counter values for delta calculation.
///
/// Interface names are stored inline as `[u8; 16]`.  A single timestamp covers
/// the whole snapshot — all interfaces are sampled atomically from `/proc/net/dev`.
pub(crate) struct PrevNetSnapshot {
    names: [[u8; 16]; MAX_NETWORKS],
    name_lens: [u8; MAX_NETWORKS],
    rx: [u64; MAX_NETWORKS],
    tx: [u64; MAX_NETWORKS],
    ts: Instant,
    len: u8,
}

impl PrevNetSnapshot {
    pub fn new() -> Self {
        Self {
            names: [[0u8; 16]; MAX_NETWORKS],
            name_lens: [0u8; MAX_NETWORKS],
            rx: [0u64; MAX_NETWORKS],
            tx: [0u64; MAX_NETWORKS],
            ts: Instant::now(),
            len: 0,
        }
    }

    /// Look up a previous counter value by interface name.
    fn get(&self, name: &str) -> Option<(u64, u64, Instant)> {
        let b = name.as_bytes();
        for i in 0..self.len as usize {
            if self.name_lens[i] as usize == b.len() && self.names[i][..b.len()] == *b {
                return Some((self.rx[i], self.tx[i], self.ts));
            }
        }
        None
    }
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Split a `/proc/net/dev` line into interface name and the remainder after ':'.
fn parse_iface(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let colon = line.find(':')?;
    let name = line[..colon].trim();
    if name.is_empty() {
        None
    } else {
        Some((name, &line[colon + 1..]))
    }
}

/// Parse the 16 space-separated u64 stat fields after the interface name.
///
/// Column order (from kernel `net/core/net-procfs.c`):
///   rx: bytes, packets, errs, drop, fifo, frame, compressed, multicast
///   tx: bytes, packets, errs, drop, fifo, colls, carrier, compressed
fn parse_stats(rest: &str) -> Option<[u64; 16]> {
    let mut n = [0u64; 16];
    let mut it = rest.split_whitespace();
    for v in n.iter_mut() {
        *v = it.next()?.parse().ok()?;
    }
    Some(n)
}

// ── Interface filtering ───────────────────────────────────────────────────────

/// Returns `true` for lo and common virtual/container interface prefixes.
fn is_virtual(name: &str) -> bool {
    const VP: &[&str] = &["docker", "veth", "br-", "tun", "tap", "virbr", "vnet"];
    name == "lo" || VP.iter().any(|p| name.starts_with(p))
}

/// Simple glob: `*` matches any substring.  `eth*` matches `eth0`.
/// `*lan` matches `wlan`.  `*` matches everything.  No `*` = exact match.
fn glob_match(pat: &str, name: &str) -> bool {
    match pat.split_once('*') {
        Some((pre, suf)) => name.starts_with(pre) && name.ends_with(suf),
        None => pat == name,
    }
}

/// Apply `Config::include_nics` / `Config::exclude_nics` filtering.
///
/// If the include list is non-empty, only interfaces matching at least one
/// include pattern are kept (whitelist mode).  Otherwise, interfaces matching
/// any exclude pattern are dropped.
fn include_nic(name: &str, cfg: &Config) -> bool {
    if !cfg.include_nics.is_empty() {
        return cfg.include_nics.iter().any(|p| glob_match(p, name));
    }
    !cfg.exclude_nics.iter().any(|p| glob_match(p, name))
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Collect network statistics for all eligible interfaces.
///
/// Reads `/proc/net/dev`, skips the two header lines, and for each remaining
/// line parses the interface name and 16 stat columns.  Loopback and virtual
/// interfaces (docker*, veth*, br-*, tun*, tap*, virbr*, vnet*) are excluded
/// by default.  `Config::include_nics` / `Config::exclude_nics` refine
/// selection with glob patterns.
///
/// Speeds are computed as the delta from the previous call divided by elapsed
/// time.  On the first call all speeds are 0.
pub fn collect(config: &Config, prev: &mut PrevNetSnapshot) -> SmallVec<NetInfo, MAX_NETWORKS> {
    let mut out = SmallVec::new();
    let now = Instant::now();

    let file = match File::open("/proc/net/dev") {
        Ok(f) => f,
        Err(_) => return out,
    };

    // Staging buffers for the next snapshot (written back to `prev` at end).
    let mut nn = [[0u8; 16]; MAX_NETWORKS];
    let mut nl = [0u8; MAX_NETWORKS];
    let mut nr = [0u64; MAX_NETWORKS];
    let mut nt = [0u64; MAX_NETWORKS];
    let mut nc: u8 = 0;

    for line in BufReader::new(file).lines().skip(2) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let (name, rest) = match parse_iface(&line) {
            Some(v) => v,
            None => continue,
        };
        if is_virtual(name) || !include_nic(name, config) {
            continue;
        }
        let nums = match parse_stats(rest) {
            Some(v) => v,
            None => continue,
        };
        let rx = nums[0]; // bytes received
        let tx = nums[8]; // bytes sent

        // Compute per-second speeds from previous snapshot.
        let (up, down) = match prev.get(name) {
            Some((prx, ptx, pts)) => {
                let secs = now.duration_since(pts).as_secs_f64();
                if secs < 0.001 {
                    (0, 0)
                } else {
                    (
                        (tx.wrapping_sub(ptx) as f64 / secs) as u64,
                        (rx.wrapping_sub(prx) as f64 / secs) as u64,
                    )
                }
            }
            None => (0, 0),
        };

        // Copy interface name into inline storage.
        let b = name.as_bytes();
        let n = b.len().min(15);
        let mut name_buf = [0u8; 16];
        name_buf[..n].copy_from_slice(&b[..n]);

        let _ = out.push(NetInfo {
            name_buf,
            name_len: n as u8,
            up,
            down,
            total_up: tx,
            total_down: rx,
        });

        // Record this interface for the next tick's snapshot.
        if (nc as usize) < MAX_NETWORKS {
            nn[nc as usize][..n].copy_from_slice(&b[..n]);
            nl[nc as usize] = n as u8;
            nr[nc as usize] = rx;
            nt[nc as usize] = tx;
            nc += 1;
        }
    }

    // Commit the new snapshot for the next call.
    prev.names = nn;
    prev.name_lens = nl;
    prev.rx = nr;
    prev.tx = nt;
    prev.ts = now;
    prev.len = nc;

    out
}
