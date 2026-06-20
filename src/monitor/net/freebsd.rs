// komari-agent-rs: FreeBSD network metrics — getifaddrs AF_LINK + delta.
#![cfg(target_os = "freebsd")]

use std::ffi::CStr;
use std::time::Instant;

use crate::arena::{MAX_NETWORKS, SmallVec};
use crate::config::Config;

/// Per-interface network statistics for a single monitoring tick.
pub struct NetInfo {
    name_buf: [u8; 16],
    name_len: u8,
    pub up: u64,
    pub down: u64,
    pub total_up: u64,
    pub total_down: u64,
}

impl NetInfo {
    #[inline]
    pub fn name(&self) -> &str {
        std::str::from_utf8(&self.name_buf[..self.name_len as usize]).unwrap_or("?")
    }
}

// ── Previous snapshot ────────────────────────────────────────────────────────

pub struct PrevNetSnapshot {
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

// ── FFI: getifaddrs / freeifaddrs (libc) ────────────────────────────────────

const AF_LINK: u16 = 18; // AF_LINK on FreeBSD
const IFF_UP: u32 = 1;
const IFF_LOOPBACK: u32 = 8;

// struct sockaddr_dl — FreeBSD layout (same as Darwin/macOS for these fields)
#[repr(C)]
struct SockAddrDl {
    sdl_len: u8,
    sdl_family: u8,
    sdl_index: u16,
    sdl_type: u8,
    sdl_nlen: u8,
    sdl_alen: u8,
    sdl_slen: u8,
    sdl_data: [u8; 12],
}

// struct if_data — 64-bit FreeBSD layout (from net/if_var.h)
#[repr(C)]
struct IfData {
    ifi_type: u8,        // offset 0
    ifi_physical: u8,    // offset 1
    ifi_addrlen: u8,     // offset 2
    ifi_hdrlen: u8,      // offset 3
    ifi_link_state: u8,  // offset 4
    ifi_vhid: u8,        // offset 5
    ifi_datalen: u16,    // offset 6
    ifi_mtu: u32,        // offset 8
    ifi_metric: u32,     // offset 12
    ifi_baudrate: u64,   // offset 16
    ifi_ipackets: u64,   // offset 24
    ifi_ierrors: u64,    // offset 32
    ifi_opackets: u64,   // offset 40
    ifi_oerrors: u64,    // offset 48
    ifi_collisions: u64, // offset 56
    ifi_ibytes: u64,     // offset 64
    ifi_obytes: u64,     // offset 72
    ifi_imcasts: u64,    // offset 80
    ifi_omcasts: u64,    // offset 88
    ifi_iqdrops: u64,    // offset 96
    ifi_oqdrops: u64,    // offset 104
    ifi_noproto: u64,    // offset 112
    ifi_hwassist: u64,   // offset 120
                         // __ifi_epoch and __ifi_lastchange follow (16 bytes each)
}

// Minimal ifaddrs — only the fields we actually read
#[repr(C)]
struct IfAddrs {
    ifa_next: *mut IfAddrs,
    ifa_name: *mut i8,
    ifa_flags: u32,
    _pad: u32,
    ifa_addr: *mut u8,
    ifa_netmask: *mut u8,
    ifa_dstaddr: *mut u8,
    ifa_data: *mut IfData,
}

unsafe extern "C" {
    fn getifaddrs(ifap: *mut *mut IfAddrs) -> i32;
    fn freeifaddrs(ifa: *mut IfAddrs);
}

// ── Interface filtering ─────────────────────────────────────────────────────

fn is_virtual(name: &str) -> bool {
    const VP: &[&str] = &[
        "bridge", "gif", "stf", "tap", "tun", "epair", "wg", "pflog", "pfsync", "enc", "lo",
        "vlan", "lagg",
    ];
    name == "lo0" || VP.iter().any(|p| name.starts_with(p))
}

fn glob_match(pat: &str, name: &str) -> bool {
    match pat.split_once('*') {
        Some((pre, suf)) => name.starts_with(pre) && name.ends_with(suf),
        None => pat == name,
    }
}

fn include_nic(name: &str, cfg: &Config) -> bool {
    if !cfg.include_nics.is_empty() {
        return cfg.include_nics.iter().any(|p| glob_match(p, name));
    }
    !cfg.exclude_nics.iter().any(|p| glob_match(p, name))
}

// ── collect ─────────────────────────────────────────────────────────────────

/// Collect network statistics for all eligible interfaces via `getifaddrs`.
///
/// Iterates AF_LINK entries to read `ifi_ibytes` / `ifi_obytes` cumulative
/// counters.  Filters loopback and virtual interfaces.
/// Computes per-second upload/download speeds via delta from the previous tick.
pub fn collect(config: &Config, prev: &mut PrevNetSnapshot) -> SmallVec<NetInfo, MAX_NETWORKS> {
    let mut out = SmallVec::new();
    let now = Instant::now();

    let mut ifa_head: *mut IfAddrs = std::ptr::null_mut();
    let ret = unsafe { getifaddrs(&mut ifa_head) };
    if ret != 0 {
        return out;
    }

    let mut nn = [[0u8; 16]; MAX_NETWORKS];
    let mut nl = [0u8; MAX_NETWORKS];
    let mut nr = [0u64; MAX_NETWORKS];
    let mut nt = [0u64; MAX_NETWORKS];
    let mut nc: u8 = 0;

    let mut cur = ifa_head;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };

        // Must be up and not loopback
        if (ifa.ifa_flags & IFF_UP) == 0 || (ifa.ifa_flags & IFF_LOOPBACK) != 0 {
            cur = ifa.ifa_next;
            continue;
        }

        // Get interface name
        if ifa.ifa_name.is_null() {
            cur = ifa.ifa_next;
            continue;
        }
        let name = match unsafe { CStr::from_ptr(ifa.ifa_name) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                cur = ifa.ifa_next;
                continue;
            }
        };

        if is_virtual(name) || !include_nic(name, config) {
            cur = ifa.ifa_next;
            continue;
        }

        // Only AF_LINK entries carry if_data with byte counters
        if ifa.ifa_addr.is_null() {
            cur = ifa.ifa_next;
            continue;
        }
        // FreeBSD/BSD sockaddr has sa_len at offset 0, sa_family at offset 1
        let family = unsafe { *(ifa.ifa_addr.add(1)) } as u16;
        if family != AF_LINK {
            cur = ifa.ifa_next;
            continue;
        }

        // Read if_data counters
        if ifa.ifa_data.is_null() {
            cur = ifa.ifa_next;
            continue;
        }
        let ifd = unsafe { &*ifa.ifa_data };
        let rx = ifd.ifi_ibytes;
        let tx = ifd.ifi_obytes;

        // Compute per-second speeds from previous snapshot
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

        // Copy interface name into inline storage
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

        if (nc as usize) < MAX_NETWORKS {
            nn[nc as usize][..n].copy_from_slice(&b[..n]);
            nl[nc as usize] = n as u8;
            nr[nc as usize] = rx;
            nt[nc as usize] = tx;
            nc += 1;
        }

        cur = ifa.ifa_next;
    }

    unsafe { freeifaddrs(ifa_head) };

    // Commit the new snapshot for the next call
    prev.names = nn;
    prev.name_lens = nl;
    prev.rx = nr;
    prev.tx = nt;
    prev.ts = now;
    prev.len = nc;

    out
}
