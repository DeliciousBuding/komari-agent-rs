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
    luids: [u64; MAX_NETWORKS],
    rx: [u64; MAX_NETWORKS],
    tx: [u64; MAX_NETWORKS],
    ts: Instant,
    len: u8,
}

impl PrevNetSnapshot {
    pub fn new() -> Self {
        Self {
            luids: [0u64; MAX_NETWORKS],
            rx: [0u64; MAX_NETWORKS],
            tx: [0u64; MAX_NETWORKS],
            ts: Instant::now(),
            len: 0,
        }
    }

    fn get(&self, luid: u64) -> Option<(u64, u64, Instant)> {
        for i in 0..self.len as usize {
            if self.luids[i] == luid {
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
    interface_luid: u64,
    _pad0: [u8; 0x1C - 8],
    // 0x01C..0x21E: Alias [u16;257] = 514 bytes
    _alias: [u16; IF_MAX_STRING_SIZE + 1], // offset 0x01C, size 514
    description: [u16; IF_MAX_STRING_SIZE + 1], // offset 0x21E, size 514
    // 0x420..0x468: PhysicalAddressLength(4) + PhysicalAddress(32) +
    //                PermanentPhysicalAddress(32) + Mtu(4) = 72 bytes
    _pad1: [u8; 0x468 - 0x420],
    if_type: u32, // offset 0x468 (IFTYPE)
    // 0x46C..0x480: TunnelType..DirectionType (5x u32) = 20 bytes
    _pad2: [u8; 0x480 - 0x46C],
    // 0x480..0x484: InterfaceAndOperStatusFlags (4 bytes incl. internal pad)
    interface_flags: u8,
    _pad3: [u8; 3],
    oper_status: u32,         // offset 0x484
    admin_status: u32,        // offset 0x488
    media_connect_state: u32, // offset 0x48C
    // 0x490..0x4B8: NetworkGuid(16) + ConnectionType(4) + 4 pad + TransmitLinkSpeed(8) + ReceiveLinkSpeed(8) = 40 bytes
    _pad4: [u8; 0x4B8 - 0x490],
    in_octets: u64, // offset 0x4B8
    // 0x4C0..0x500: InUcastPkts..InBroadcastOctets (8x u64) = 64 bytes
    _pad5: [u8; 0x500 - 0x4C0],
    out_octets: u64, // offset 0x500
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
// MIB_IF_ROW2::InterfaceAndOperStatusFlags.FilterInterface (netioapi.h).
// These NDIS filter rows duplicate an adapter rather than a traffic source.
const FILTER_INTERFACE: u8 = 1 << 1;
const IF_OPER_STATUS_UP: u32 = 1;
const MEDIA_CONNECT_STATE_CONNECTED: u32 = 1;
const NO_ERROR: u32 = 0;

fn wide_to_utf8(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16(&wide[..end]).unwrap_or_else(|_| "Unknown".to_string())
}

fn fit_name(name: &str) -> ([u8; 16], u8) {
    let b = name.as_bytes();
    let mut n = b.len().min(15);
    while !name.is_char_boundary(n) {
        n -= 1;
    }
    let mut buf = [0u8; 16];
    buf[..n].copy_from_slice(&b[..n]);
    (buf, n as u8)
}

pub fn collect(config: &Config, prev: &mut PrevNetSnapshot) -> SmallVec<NetInfo, MAX_NETWORKS> {
    let mut out = SmallVec::new();

    let mut table_ptr: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    let ret = unsafe { GetIfTable2(&mut table_ptr) };
    if ret != NO_ERROR || table_ptr.is_null() {
        return out;
    }

    let table = unsafe { &*table_ptr };
    let num = table.num_entries as usize;

    let header_size = std::mem::size_of::<MIB_IF_TABLE2>();
    let rows_base = unsafe { (table_ptr as *const u8).add(header_size) } as *const MIB_IF_ROW2;
    let rows = unsafe { std::slice::from_raw_parts(rows_base, num) };
    out = collect_rows(rows, config, prev, Instant::now());
    unsafe {
        FreeMibTable(table_ptr as *mut std::ffi::c_void);
    }
    out
}

fn collect_rows(
    rows: &[MIB_IF_ROW2],
    config: &Config,
    prev: &mut PrevNetSnapshot,
    now: Instant,
) -> SmallVec<NetInfo, MAX_NETWORKS> {
    let mut out = SmallVec::new();
    let mut luids = [0u64; MAX_NETWORKS];
    let mut nr = [0u64; MAX_NETWORKS];
    let mut nt = [0u64; MAX_NETWORKS];
    let mut nc: u8 = 0;

    for row in rows {
        if row.interface_flags & FILTER_INTERFACE != 0 {
            continue;
        }
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

        let (down, up) = match prev.get(row.interface_luid) {
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
            luids[nc as usize] = row.interface_luid;
            nr[nc as usize] = rx;
            nt[nc as usize] = tx;
            nc += 1;
        }
    }

    prev.luids = luids;
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

    fn row(luid: u64, name: &str, flags: u8, rx: u64, tx: u64) -> MIB_IF_ROW2 {
        // All fields are integer values or arrays, so zero is valid.
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.interface_luid = luid;
        row.interface_flags = flags;
        row.oper_status = IF_OPER_STATUS_UP;
        row.media_connect_state = MEDIA_CONNECT_STATE_CONNECTED;
        row.if_type = 6; // ethernet
        for (dst, src) in row.description.iter_mut().zip(name.encode_utf16()) {
            *dst = src;
        }
        row.in_octets = rx;
        row.out_octets = tx;
        row
    }

    #[test]
    fn mib_row_layout_matches_sdk() {
        assert_eq!(std::mem::size_of::<MIB_IF_ROW2>(), 0x548);
        assert_eq!(std::mem::size_of::<MIB_IF_TABLE2>(), 8);
        assert_eq!(std::mem::offset_of!(MIB_IF_ROW2, description), 0x21E);
        assert_eq!(std::mem::offset_of!(MIB_IF_ROW2, interface_flags), 0x480);
        assert_eq!(std::mem::offset_of!(MIB_IF_ROW2, oper_status), 0x484);
        assert_eq!(std::mem::offset_of!(MIB_IF_ROW2, in_octets), 0x4B8);
        assert_eq!(std::mem::offset_of!(MIB_IF_ROW2, out_octets), 0x500);
    }

    #[test]
    fn filter_rows_cannot_exhaust_capacity_before_real_adapter() {
        let mut rows: Vec<_> = (0..MAX_NETWORKS + 5)
            .map(|i| row(i as u64, "NDIS filter", FILTER_INTERFACE, 0, 0))
            .collect();
        rows.push(row(100, "Ethernet adapter", 0, 1234, 5678));
        let nets = collect_rows(
            &rows,
            &Config::default(),
            &mut PrevNetSnapshot::new(),
            Instant::now(),
        );
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].total_down, 1234);
        assert_eq!(nets[0].total_up, 5678);
    }

    #[test]
    fn long_names_and_row_reordering_preserve_distinct_rate_baselines() {
        let config = Config::default();
        let mut prev = PrevNetSnapshot::new();
        let now = Instant::now();
        let first = [
            row(1, "Ethernet adapter A", 0, 100, 200),
            row(2, "Ethernet adapter B", 0, 1000, 2000),
        ];
        let nets = collect_rows(&first, &config, &mut prev, now);
        assert_eq!(nets[0].down, 0);
        let second = [
            row(2, "Ethernet adapter B", 0, 1300, 2400),
            row(1, "Ethernet adapter A", 0, 150, 275),
        ];
        let nets = collect_rows(
            &second,
            &config,
            &mut prev,
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!((nets[0].down, nets[0].up), (300, 400));
        assert_eq!((nets[1].down, nets[1].up), (50, 75));
    }

    #[test]
    fn display_name_truncation_preserves_utf8() {
        let (buf, len) = fit_name("网卡适配器 Ethernet");
        assert_eq!(
            std::str::from_utf8(&buf[..len as usize]).unwrap(),
            "网卡适配器"
        );
    }

    /// Regression for the MIB_IF_ROW2 byte-offset fix.
    ///
    /// The struct's `InterfaceGuid` field is 4-byte aligned (its first member
    /// is a ULONG), so it lands at 0x00C, not 0x010. Hand-rolling the struct
    /// with 8-byte GUID alignment shifts every field after it by 4 bytes; the
    /// Up+Connected+non-loopback filter then rejects every adapter and
    /// `total_up`/`total_down` read as 0 even on hosts with heavy traffic. On
    /// any active Windows host at least one qualifying adapter must survive
    /// the filter — if none do, the offsets have regressed. We assert on the
    /// adapter count (not octets) so the test stays green on a CI runner whose
    /// NIC may momentarily be quiet; per-adapter octets are printed for
    /// diagnosis.
    #[test]
    fn net_offsets_read_nonzero_octets() {
        let config = Config::default();
        let mut prev = PrevNetSnapshot::new();
        let nets = collect(&config, &mut prev);

        let total_down: u64 = nets.iter().map(|n| n.total_down).sum();
        let total_up: u64 = nets.iter().map(|n| n.total_up).sum();
        for n in nets.iter() {
            eprintln!(
                "  {} total_up={} total_down={}",
                n.name(),
                n.total_up,
                n.total_down
            );
        }
        eprintln!(
            "aggregated: {} adapters passed filter, total_up={}, total_down={}",
            nets.len(),
            total_up,
            total_down
        );

        // Before the fix every field sat 4 bytes off, so the Up+Connected
        // filter matched zero adapters. At least one adapter must survive on
        // any host with a live NIC. Octets are NOT hard-asserted (a CI runner
        // NIC can be momentarily quiet); they are printed above for diagnosis.
        assert!(
            !nets.is_empty(),
            "MIB_IF_ROW2 offsets regressed: no adapter passed the Up+Connected filter"
        );
    }
}
