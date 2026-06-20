// komari-agent-rs: Windows connection counts — GetTcpTable2 + GetUdpTable2.
#![cfg(windows)]

use std::io;

/// Error type for connection-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    /// An I/O error occurred.
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

/// Collected connection counts.
pub struct ConnectionsInfo {
    /// Number of active TCP connections (IPv4 + IPv6).
    pub tcp: u32,
    /// Number of active UDP sockets (IPv4 + IPv6).
    pub udp: u32,
}

// ── FFI: iphlpapi.dll ────────────────────────────────────────────────────────

// MIB_TCPTABLE2 / MIB_TCPROW2
#[repr(C)]
struct MIB_TCPTABLE2 {
    dwNumEntries: u32,
    // table: [MIB_TCPROW2] follows
}

#[repr(C)]
struct MIB_UDPTABLE2 {
    dwNumEntries: u32,
    // table: [MIB_UDPROW2] follows
}

unsafe extern "system" {
    fn GetTcpTable2(tcpTable: *mut MIB_TCPTABLE2, sizePointer: *mut u32, order: i32) -> u32;

    fn GetUdpTable2(udpTable: *mut MIB_UDPTABLE2, sizePointer: *mut u32, order: i32) -> u32;
}

const NO_ERROR: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Query a table-returning API with automatic buffer sizing.
/// Returns the number of entries (from dwNumEntries in the first 4 bytes), or 0 on failure.
fn get_table_entry_count(table_ptr: *const u8, size: u32) -> u32 {
    if size < 4 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts(table_ptr, size as usize) };
    u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]])
}

// ── collect_connections ──────────────────────────────────────────────────────

/// Collect TCP and UDP connection counts via `GetTcpTable2` and `GetUdpTable2`.
///
/// Counts all non-zero entries in both tables (IPv4 + IPv6 combined).
/// Returns `Ok(ConnectionsInfo { tcp: 0, udp: 0 })` when the APIs fail
/// because this metric is best-effort.
pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = {
        let mut size: u32 = 0;
        // First call: get required buffer size
        let ret = unsafe { GetTcpTable2(std::ptr::null_mut(), &mut size, 0) };
        if ret != ERROR_INSUFFICIENT_BUFFER || size == 0 {
            0
        } else {
            let mut buf: Vec<u8> = vec![0u8; size as usize];
            let ret = unsafe { GetTcpTable2(buf.as_mut_ptr() as *mut MIB_TCPTABLE2, &mut size, 0) };
            if ret != NO_ERROR {
                0
            } else {
                get_table_entry_count(buf.as_ptr(), size)
            }
        }
    };

    let udp = {
        let mut size: u32 = 0;
        let ret = unsafe { GetUdpTable2(std::ptr::null_mut(), &mut size, 0) };
        if ret != ERROR_INSUFFICIENT_BUFFER || size == 0 {
            0
        } else {
            let mut buf: Vec<u8> = vec![0u8; size as usize];
            let ret = unsafe { GetUdpTable2(buf.as_mut_ptr() as *mut MIB_UDPTABLE2, &mut size, 0) };
            if ret != NO_ERROR {
                0
            } else {
                get_table_entry_count(buf.as_ptr(), size)
            }
        }
    };

    Ok(ConnectionsInfo { tcp, udp })
}
