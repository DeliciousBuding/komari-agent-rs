// komari-agent-rs: macOS connection counts — sysctlbyname net.inet.tcp.pcblist.
#![cfg(target_os = "macos")]

use std::io;

/// Error type for connection-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

/// Collected connection counts.
pub struct ConnectionsInfo {
    pub tcp: u32,
    pub udp: u32,
}

// ── sysctl FFI (libSystem) ──────────────────────────────────────────────────

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

// Each TCP PCB (protocol control block) entry starts with a fixed-size header.
// We count entries by querying the total buffer size, then dividing by the
// known entry size.  The sysctl returns a packed array of PCB entries.
//
// struct xinpcb_n — darwin xnu bsd/netinet/in_pcb.h
// struct xtcpcb_n — darwin xnu bsd/netinet/tcp_var.h

const XTCPCB_N_SIZE: usize = 920; // struct xtcpcb_n on macOS 14+ arm64/x86_64
const XINPCB_N_SIZE: usize = 464; // struct xinpcb_n (used for UDP pcblist)

/// Query a pcblist sysctl and count entries.
/// Returns the number of entries, or 0 on failure.
unsafe fn count_pcblist(name: &str, entry_size: usize) -> u32 {
    // First call: get required buffer size
    let mut len: usize = 0;
    let ret = sysctlbyname(
        name.as_ptr(),
        std::ptr::null_mut(),
        &mut len,
        std::ptr::null(),
        0,
    );
    if ret != 0 || len == 0 {
        return 0;
    }

    // Allocate and fetch
    let mut buf: Vec<u8> = vec![0u8; len];
    let ret = sysctlbyname(
        name.as_ptr(),
        buf.as_mut_ptr(),
        &mut len,
        std::ptr::null(),
        0,
    );
    if ret != 0 || len < entry_size {
        return 0;
    }

    (len / entry_size) as u32
}

// ── collect_connections ─────────────────────────────────────────────────────

/// Collect TCP and UDP connection counts via `sysctlbyname` pcblist queries.
///
/// Uses `net.inet.tcp.pcblist_n` and `net.inet.udp.pcblist_n` which return
/// arrays of xtcpcb_n / xinpcb_n structures respectively.  Count = buffer_size / struct_size.
pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = unsafe { count_pcblist("net.inet.tcp.pcblist_n", XTCPCB_N_SIZE) };
    let udp = unsafe { count_pcblist("net.inet.udp.pcblist_n", XINPCB_N_SIZE) };
    Ok(ConnectionsInfo { tcp, udp })
}
