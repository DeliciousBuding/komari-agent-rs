// komari-agent-rs: Windows network metrics — GetIfTable2 + delta calculation.
#![cfg(windows)]

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

// ── FFI: iphlpapi.dll — MIB_IF_ROW2 / MIB_IF_TABLE2 ─────────────────────────
// Offsets verified against Windows 11 SDK for x86_64.

const IF_MAX_STRING_SIZE: usize = 256;

#[repr(C)]
struct MIB_IF_ROW2 {
    _pad0: [u8; 0x222],                         // InterfaceGuid..Alias
    description: [u16; IF_MAX_STRING_SIZE + 1], // offset 0x222, size 514
    _pad1: [u8; 0x474 - 0x424],                 // PhysicalAddressLength..MediaType
    if_type: u32,                               // offset 0x474 (IFTYPE)
    _pad2: [u8; 0x490 - 0x478],                 // TunnelType..IfAndOperStatusFlags
    oper_status: u32,                           // offset 0x490
    admin_status: u32,                          // offset 0x494
    media_connect_state: u32,                   // offset 0x498
    _pad3: [u8; 0x4C0 - 0x49C],                 // NetworkGuid..ReceiveLinkSpeed
    in_octets: u64,                             // offset 0x4C0
    _pad4: [u8; 0x508 - 0x4C8],                 // InUcastPkts..InBroadcastOctets
    out_octets: u64,                            // offset 0x508
}

#[repr(C)]
struct MIB_IF_TABLE2 {
    num_entries: u32,
    _pad: u32,
}

unsafe extern "system" {
    fn GetIfTable2(table: *mut *mut MIB_IF_TABLE2) -> u32;
    fn FreeMibTable(memory: *mut std::ffi::c_void);
}

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: u32 = 1;
const MEDIA_CONNECT_STATE_CONNECTED: u32 = 1;
const NO_ERROR: u32 = 0;

fn wide_to_utf8(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16(&wide[..end]).unwrap_or_else(|_| "Unknown".to_string())
}

fn fit_name(name: &str) -> ([u8; 16], u8) {
    let b = name.as_bytes();
    let n = b.len().min(15);
    let mut buf = [0u8; 16];
    buf[..n].copy_from_slice(&b[..n]);
    (buf, n as u8)
}

pub fn collect(config: &Config, prev: &mut PrevNetSnapshot) -> SmallVec<NetInfo, MAX_NETWORKS> {
    let mut out = SmallVec::new();
    let now = Instant::now();

    let mut table_ptr: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    let ret = unsafe { GetIfTable2(&mut table_ptr) };
    if ret != NO_ERROR || table_ptr.is_null() {
        return out;
    }

    let table = unsafe { &*table_ptr };
    let num = table.num_entries as usize;

    let mut nn = [[0u8; 16]; MAX_NETWORKS];
    let mut nl = [0u8; MAX_NETWORKS];
    let mut nr = [0u64; MAX_NETWORKS];
    let mut nt = [0u64; MAX_NETWORKS];
    let mut nc: u8 = 0;

    let header_size = std::mem::size_of::<MIB_IF_TABLE2>();
    let rows_base = unsafe { (table_ptr as *const u8).add(header_size) } as *const MIB_IF_ROW2;

    for i in 0..num {
        let row = unsafe { &*rows_base.add(i) };

        if row.oper_status != IF_OPER_STATUS_UP {
            continue;
        }
        if row.media_connect_state != MEDIA_CONNECT_STATE_CONNECTED {
            continue;
        }
        if row.if_type == IF_TYPE_SOFTWARE_LOOPBACK {
            continue;
        }

        let desc = wide_to_utf8(&row.description);
        if desc.is_empty() || desc == "Unknown" {
            continue;
        }

        let use_whitelist = !config.include_nics.is_empty();
        let included = if use_whitelist {
            config
                .include_nics
                .iter()
                .any(|p| desc.contains(p.as_str()))
        } else {
            true
        };
        let excluded = config
            .exclude_nics
            .iter()
            .any(|p| desc.contains(p.as_str()));
        if !included || excluded {
            continue;
        }

        let rx = row.in_octets;
        let tx = row.out_octets;

        let (down, up) = match prev.get(&desc) {
            Some((prx, ptx, pts)) => {
                let secs = now.duration_since(pts).as_secs_f64();
                if secs < 0.001 {
                    (0, 0)
                } else {
                    (
                        (rx.wrapping_sub(prx) as f64 / secs) as u64,
                        (tx.wrapping_sub(ptx) as f64 / secs) as u64,
                    )
                }
            }
            None => (0, 0),
        };

        let (name_buf, name_len) = fit_name(&desc);

        let _ = out.push(NetInfo {
            name_buf,
            name_len,
            up,
            down,
            total_up: tx,
            total_down: rx,
        });

        if (nc as usize) < MAX_NETWORKS {
            nn[nc as usize] = name_buf;
            nl[nc as usize] = name_len;
            nr[nc as usize] = rx;
            nt[nc as usize] = tx;
            nc += 1;
        }
    }

    unsafe {
        FreeMibTable(table_ptr as *mut std::ffi::c_void);
    }

    prev.names = nn;
    prev.name_lens = nl;
    prev.rx = nr;
    prev.tx = nt;
    prev.ts = now;
    prev.len = nc;

    out
}
