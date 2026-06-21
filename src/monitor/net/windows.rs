// komari-agent-rs: Windows network metrics — GetIfTable2 + delta calculation.
#![cfg(windows)]
#![allow(dead_code)]

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
// Offsets are the ground-truth x86_64 layout, obtained empirically via an
// offset_of! probe against the microsoft/windows-rs `MIB_IF_ROW2` #[repr(C)]
// definition (= SDK netioapi.h). The decisive detail: `InterfaceGuid` is
// 4-byte aligned (its first member is a ULONG), so it sits right after
// `InterfaceIndex` at 0x00C with NO padding — NOT at 0x010. Getting that wrong
// shifts every subsequent field by 4 bytes and the reads come back as garbage.
// Struct total size = 0x548 (1352 bytes).
//
//   InterfaceLuid              @ 0x000  u64 (8)
//   InterfaceIndex             @ 0x008  u32 (4)
//   InterfaceGuid              @ 0x00C  [u8;16] (4-byte aligned, no pad)
//   Alias                      @ 0x01C  [u16;257] (514)
//   Description                @ 0x21E  [u16;257] (514)
//   PhysicalAddressLength      @ 0x420  u32
//   PhysicalAddress            @ 0x424  [u8;32]
//   PermanentPhysicalAddress   @ 0x444  [u8;32]
//   Mtu                        @ 0x464  u32
//   Type                       @ 0x468  u32  (IFTYPE; loopback filter)
//   TunnelType                 @ 0x46C  u32
//   MediaType                  @ 0x470  u32
//   PhysicalMediumType         @ 0x474  u32
//   AccessType                 @ 0x478  u32
//   DirectionType              @ 0x47C  u32
//   InterfaceAndOperStatusFlags@ 0x480  (4 bytes incl. internal pad)
//   OperStatus                 @ 0x484  u32  (UP filter)
//   AdminStatus                @ 0x488  u32
//   MediaConnectState          @ 0x48C  u32  (CONNECTED filter)
//   NetworkGuid                @ 0x490  [u8;16] (4-byte aligned)
//   ConnectionType             @ 0x4A0  u32   +4 pad to u64 align
//   TransmitLinkSpeed          @ 0x4A8  u64
//   ReceiveLinkSpeed           @ 0x4B0  u64
//   InOctets                   @ 0x4B8  u64  (-> rx / total_down)
//   InUcastPkts..InBroadcastOctets (8x u64) @ 0x4C0..0x500
//   OutOctets                  @ 0x500  u64  (-> tx / total_up)
//   OutUcastPkts..OutQLen (8x u64)       @ 0x508..0x548

const IF_MAX_STRING_SIZE: usize = 256;

#[repr(C)]
struct MIB_IF_ROW2 {
    // 0x000..0x01C: InterfaceLuid(8) + InterfaceIndex(4) + InterfaceGuid(16, 4-byte aligned) = 28
    _pad0: [u8; 0x1C],
    // 0x01C..0x21E: Alias [u16;257] = 514 bytes
    _alias: [u16; IF_MAX_STRING_SIZE + 1],          // offset 0x01C, size 514
    description: [u16; IF_MAX_STRING_SIZE + 1],      // offset 0x21E, size 514
    // 0x420..0x468: PhysicalAddressLength(4) + PhysicalAddress(32) +
    //                PermanentPhysicalAddress(32) + Mtu(4) = 72 bytes
    _pad1: [u8; 0x468 - 0x420],
    if_type: u32,                                    // offset 0x468 (IFTYPE)
    // 0x46C..0x480: TunnelType..DirectionType (5x u32) = 20 bytes
    _pad2: [u8; 0x480 - 0x46C],
    // 0x480..0x484: InterfaceAndOperStatusFlags (4 bytes incl. internal pad)
    _pad3: [u8; 0x484 - 0x480],
    oper_status: u32,                                // offset 0x484
    admin_status: u32,                               // offset 0x488
    media_connect_state: u32,                        // offset 0x48C
    // 0x490..0x4B8: NetworkGuid(16) + ConnectionType(4) + 4 pad + TransmitLinkSpeed(8) + ReceiveLinkSpeed(8) = 40 bytes
    _pad4: [u8; 0x4B8 - 0x490],
    in_octets: u64,                                  // offset 0x4B8
    // 0x4C0..0x500: InUcastPkts..InBroadcastOctets (8x u64) = 64 bytes
    _pad5: [u8; 0x500 - 0x4C0],
    out_octets: u64,                                 // offset 0x500
    // 0x508..0x548: OutUcastPkts..OutQLen (8x u64) = 64 bytes — MUST be
    // included so that sizeof(MIB_IF_ROW2) == 0x548 (1352), matching the SDK
    // row stride. Without this tail padding every adapter after the first is
    // read from a misaligned address.
    _pad6: [u8; 0x548 - 0x508],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Regression for the MIB_IF_ROW2 byte-offset fix.
    ///
    /// The struct's `InterfaceGuid` field is 4-byte aligned (its first member
    /// is a ULONG), so it lands at 0x00C, not 0x010. Hand-rolling the struct
    /// with 8-byte GUID alignment shifts every field after it by 4 bytes; the
    /// Up+Connected+non-loopback filter then rejects every adapter and
    /// `total_up`/`total_down` read as 0 even on hosts with heavy traffic. On
    /// any active Windows host at least one qualifying adapter must report
    /// nonzero cumulative octets — if everything aggregates to 0 the offsets
    /// have regressed again. (Virtual / disconnected adapters legitimately
    /// report 0; this passes because a real NIC like WLAN carries traffic.)
    #[test]
    fn net_offsets_read_nonzero_octets() {
        let config = Config::default();
        let mut prev = PrevNetSnapshot::new();
        let nets = collect(&config, &mut prev);

        for n in nets.iter() {
            eprintln!(
                "  {} total_up={} total_down={}",
                n.name(),
                n.total_up,
                n.total_down
            );
        }
        let total_down: u64 = nets.iter().map(|n| n.total_down).sum();
        let total_up: u64 = nets.iter().map(|n| n.total_up).sum();
        eprintln!(
            "aggregated: {} adapters passed filter, total_up={}, total_down={}",
            nets.len(),
            total_up,
            total_down
        );

        assert!(
            total_down > 0 || total_up > 0,
            "MIB_IF_ROW2 offsets regressed: every adapter reports 0 octets"
        );
    }
}
