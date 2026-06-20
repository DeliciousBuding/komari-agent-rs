//! ICMP ping — raw socket on Unix, IcmpSendEcho2 FFI on Windows.
//! Behind `#[cfg(feature = "ping")]`.
//!
//! Requires `CAP_NET_RAW` (Linux) or Administrator (Windows).
//! Returns RTT in milliseconds, or -1 on failure / permission denied.

use std::net::Ipv4Addr;

/// Compute ICMP header checksum (RFC 792 — one's complement of one's complement sum).
fn icmp_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an ICMP echo request (type=8, code=0) with a simple payload.
fn build_echo_request(seq: u16) -> Vec<u8> {
    let mut pkt = vec![0u8; 40]; // 8-byte ICMP header + 32-byte payload
    pkt[0] = 8; // Type: Echo Request
    pkt[1] = 0; // Code: 0
    // bytes 2-3: checksum (filled after body is built)
    pkt[4] = 0; // Identifier MSB
    pkt[5] = 0; // Identifier LSB
    pkt[6] = (seq >> 8) as u8; // Sequence MSB
    pkt[7] = seq as u8; // Sequence LSB
    for i in 8..40 {
        pkt[i] = i as u8;
    }
    let cs = icmp_checksum(&pkt);
    pkt[2] = (cs >> 8) as u8;
    pkt[3] = cs as u8;
    pkt
}

// ═══════════════════════════════════════════════════════════════════════════
// Unix: raw socket (Linux / macOS / FreeBSD)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod unix_raw {
    use super::*;
    use std::mem;
    use std::os::raw::{c_int, c_void};
    use std::time::Instant;

    const AF_INET: c_int = 2;
    const SOCK_RAW: c_int = 3;
    const IPPROTO_ICMP: c_int = 1;
    const SOL_SOCKET: c_int = 1;
    const SO_RCVTIMEO: c_int = 20;

    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i64,
    }
    #[repr(C)]
    struct InAddr {
        s_addr: u32,
    }
    #[repr(C)]
    struct SockaddrIn {
        sin_family: u16,
        sin_port: u16,
        sin_addr: InAddr,
        sin_zero: [u8; 8],
    }

    unsafe extern "C" {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        fn setsockopt(
            fd: c_int,
            level: c_int,
            optname: c_int,
            optval: *const c_void,
            optlen: u32,
        ) -> c_int;
        fn sendto(
            fd: c_int,
            buf: *const c_void,
            len: usize,
            flags: c_int,
            to: *const SockaddrIn,
            tolen: u32,
        ) -> isize;
        fn recvfrom(
            fd: c_int,
            buf: *mut c_void,
            len: usize,
            flags: c_int,
            from: *mut SockaddrIn,
            fromlen: *mut u32,
        ) -> isize;
        fn close(fd: c_int) -> c_int;
    }

    /// Send one ICMP echo request and measure RTT.  Returns -1 on any error.
    pub fn send_icmp(ip: Ipv4Addr, timeout_ms: u64) -> i64 {
        let fd = unsafe { socket(AF_INET, SOCK_RAW, IPPROTO_ICMP) };
        if fd < 0 {
            return -1;
        }

        // Set receive timeout
        let tv = Timeval {
            tv_sec: (timeout_ms / 1000) as i64,
            tv_usec: ((timeout_ms % 1000) * 1000) as i64,
        };
        unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                &tv as *const _ as *const c_void,
                mem::size_of::<Timeval>() as u32,
            );
        }

        let pkt = build_echo_request(1);
        let dest = SockaddrIn {
            sin_family: AF_INET as u16,
            sin_port: 0,
            sin_addr: InAddr {
                s_addr: u32::from(ip).to_be(),
            },
            sin_zero: [0u8; 8],
        };

        let start = Instant::now();
        let sent = unsafe {
            sendto(
                fd,
                pkt.as_ptr() as *const c_void,
                pkt.len(),
                0,
                &dest,
                mem::size_of::<SockaddrIn>() as u32,
            )
        };
        if sent < 0 {
            unsafe { close(fd) };
            return -1;
        }

        let mut buf = [0u8; 256];
        let n = unsafe {
            recvfrom(
                fd,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        unsafe { close(fd) };

        if n < 0 {
            return -1;
        }
        let ms = start.elapsed().as_millis() as i64;
        if ms == 0 { 1 } else { ms }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Windows: IcmpSendEcho2 via iphlpapi.dll FFI
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
mod win_icmp {
    use super::*;
    use std::mem;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct IpOptionInformation {
        Ttl: u8,
        Tos: u8,
        Flags: u8,
        OptionsSize: u8,
        OptionsData: *const u8,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct IcmpEchoReply {
        Address: u32,
        Status: u32,
        RoundTripTime: u32,
        DataSize: u16,
        Reserved: u16,
        Data: *const u8,
        Options: IpOptionInformation,
    }

    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn IcmpCreateFile() -> *mut std::ffi::c_void;
        fn IcmpSendEcho2(
            icmp_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            dest_addr: u32,
            request_data: *const u8,
            request_size: u16,
            request_options: *const IpOptionInformation,
            reply_buffer: *mut u8,
            reply_size: u32,
            timeout: u32,
        ) -> u32;
        fn IcmpCloseHandle(icmp_handle: *mut std::ffi::c_void) -> bool;
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn inet_addr(cp: *const u8) -> u32;
    }

    /// Send one ICMP echo via IcmpSendEcho2.  Returns RTT in ms or -1.
    pub fn send_icmp(ip: Ipv4Addr, timeout_ms: u64) -> i64 {
        let handle = unsafe { IcmpCreateFile() };
        if handle.is_null() {
            return -1;
        }

        let dest = u32::from(ip); // IcmpSendEcho2 expects host byte order IP

        let payload = build_echo_request(1);
        // ICMP_ECHO_REPLY + echo data + margin
        let reply_size = mem::size_of::<IcmpEchoReply>() + payload.len() + 8;
        let mut reply_buf: Vec<u8> = vec![0u8; reply_size];

        let ret = unsafe {
            IcmpSendEcho2(
                handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dest,
                payload.as_ptr(),
                payload.len() as u16,
                std::ptr::null(),
                reply_buf.as_mut_ptr(),
                reply_buf.len() as u32,
                timeout_ms as u32,
            )
        };

        if ret == 0 {
            unsafe { IcmpCloseHandle(handle) };
            return -1;
        }

        let reply: &IcmpEchoReply = unsafe { &*(reply_buf.as_ptr() as *const IcmpEchoReply) };
        let rtt = if reply.Status == 0 {
            reply.RoundTripTime as i64
        } else {
            -1
        };

        unsafe { IcmpCloseHandle(handle) };
        if rtt == 0 { 1 } else { rtt }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Perform an ICMP echo (ping) to `target` (IPv4 address or hostname).
///
/// `timeout_ms` defaults to 3000 ms.  Returns round-trip time in
/// milliseconds, or -1 on failure, permission denied, or timeout.
///
/// ## Platform notes
/// - Linux: needs `CAP_NET_RAW` (`setcap cap_net_raw=+ep <binary>`).
/// - macOS/FreeBSD: needs root (or the binary must be setuid).
/// - Windows: needs Administrator (or the calling user must be in
///   the Network Operators group).
/// - Other platforms: always returns -1.
pub fn ping_icmp(target: &str, timeout_ms: Option<u64>) -> i64 {
    let timeout = timeout_ms.unwrap_or(3000);

    // Resolve hostname → IPv4
    let ip: Ipv4Addr = match target.parse() {
        Ok(addr) => addr,
        Err(_) => match crate::dns::resolve(target, 0, "", &[]) {
            Ok(addrs) => match addrs.iter().find(|a| a.is_ipv4()) {
                Some(sa) => match sa.ip() {
                    std::net::IpAddr::V4(v4) => v4,
                    _ => return -1,
                },
                None => return -1,
            },
            Err(_) => return -1,
        },
    };

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        return unix_raw::send_icmp(ip, timeout);
    }
    #[cfg(target_os = "windows")]
    {
        return win_icmp::send_icmp(ip, timeout);
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        -1
    }
}
