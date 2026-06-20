// komari-agent-rs: FreeBSD connection counts — sysctlbyname net.inet.tcp.pcblist / net.inet.udp.pcblist.
#![cfg(target_os = "freebsd")]

use std::io;

/// Error type for connection-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    Io(io::Error),
    Parse(String),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

/// Collected connection counts.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionsInfo {
    pub tcp: u64,
    pub udp: u64,
}

// ── sysctl FFI (libc) ──────────────────────────────────────────────────────

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

// struct xtcpcb and struct xinpcb sizes on FreeBSD 14+ amd64.
// These values are kernel-version-dependent but stable within a major release.
//   sizeof(struct xtcpcb)  ≈ 784  (from netinet/tcp_var.h: struct xtcpcb)
//   sizeof(struct xinpcb)  ≈ 408  (from netinet/in_pcb.h: struct xinpcb)
const XTCPCB_SIZE: usize = 784;
const XINPCB_SIZE: usize = 408;

/// Query a pcblist sysctl and count entries.
/// Returns the number of entries, or 0 on failure.
unsafe fn count_pcblist(name: &str, entry_size: usize) -> u64 {
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

    (len / entry_size) as u64
}

// ── collect_connections ─────────────────────────────────────────────────────

/// Collect TCP and UDP connection counts via `sysctlbyname` pcblist queries.
///
/// Uses `net.inet.tcp.pcblist` and `net.inet.udp.pcblist` which return
/// arrays of xtcpcb / xinpcb structures respectively.
/// Count = buffer_size / struct_size.
pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = unsafe { count_pcblist("net.inet.tcp.pcblist\0", XTCPCB_SIZE) };
    let udp = unsafe { count_pcblist("net.inet.udp.pcblist\0", XINPCB_SIZE) };
    Ok(ConnectionsInfo { tcp, udp })
}
