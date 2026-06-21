#![allow(dead_code)]
// komari-agent-rs: macOS IP detection — getifaddrs FFI + external HTTP APIs.
#![cfg(target_os = "macos")]

use crate::config::Config;
use core::fmt;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════
// MetricErr
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug)]
pub enum MetricErr {
    Io(std::io::Error),
    Parse(String),
}

impl fmt::Display for MetricErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Parse(s) => write!(f, "parse error: {}", s),
        }
    }
}

impl std::error::Error for MetricErr {}

impl From<std::io::Error> for MetricErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ═══════════════════════════════════════════════════════════════════
// FFI: getifaddrs / freeifaddrs (libSystem)
// ═══════════════════════════════════════════════════════════════════

// macOS uses AF_INET=2, AF_INET6=30 (not 10 as on Linux)
const AF_INET: u8 = 2;
const AF_INET6: u8 = 30;
const IFF_UP: u32 = 1;
const IFF_LOOPBACK: u32 = 8;

#[repr(C)]
struct InAddr {
    s_addr: [u8; 4],
}

#[repr(C)]
struct In6Addr {
    s6_addr: [u8; 16],
}

#[repr(C)]
pub(crate) struct SockAddr {
    pub(crate) sa_family: u16,
    pub(crate) sa_data: [u8; 14],
}

#[repr(C)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: InAddr,
    sin_zero: [u8; 8],
}

#[repr(C)]
struct SockAddrIn6 {
    sin6_family: u16,
    sin6_port: u16,
    sin6_flowinfo: u32,
    sin6_addr: In6Addr,
    sin6_scope_id: u32,
}

#[repr(C)]
pub(crate) struct IfAddrs {
    pub(crate) ifa_next: *mut IfAddrs,
    pub(crate) ifa_name: *mut core::ffi::c_char,
    pub(crate) ifa_flags: u32,
    pub(crate) _pad: u32,
    pub(crate) ifa_addr: *mut SockAddr,
    pub(crate) ifa_netmask: *mut SockAddr,
    pub(crate) ifa_dstaddr: *mut SockAddr,
    pub(crate) ifa_data: *mut u8,
}

unsafe extern "C" {
    fn getifaddrs(ifap: *mut *mut IfAddrs) -> i32;
    fn freeifaddrs(ifa: *mut IfAddrs);
}

// ═══════════════════════════════════════════════════════════════════
// collect_ip — main entry point
// ═══════════════════════════════════════════════════════════════════

/// Returns (ipv4, ipv6). Each is None if detection failed.
pub fn collect_ip(config: &Config) -> Result<(Option<String>, Option<String>), MetricErr> {
    // 1. NIC-based detection (if configured)
    if config.get_ip_addr_from_nic {
        let (v4, v6) = ip_from_nic(&config.include_nics, &config.exclude_nics);
        if v4.is_some() || v6.is_some() {
            return Ok((v4, v6));
        }
    }

    let mut ipv4: Option<String> = None;
    let mut ipv6: Option<String> = None;

    // 2. Custom IP overrides
    if !config.custom_ipv4.is_empty() {
        ipv4 = Some(config.custom_ipv4.clone());
    }
    if !config.custom_ipv6.is_empty() {
        ipv6 = Some(config.custom_ipv6.clone());
    }

    // 3. External HTTP APIs (best-effort, plain HTTP)
    if ipv4.is_none() {
        ipv4 = fetch_ipv4_http();
    }
    if ipv6.is_none() {
        ipv6 = fetch_ipv6_http();
    }

    // 4. Cloudflare trace fallback
    if ipv4.is_none()
        && ipv6.is_none()
        && let Some((v4, v6)) = fetch_cf_trace()
    {
        ipv4 = v4;
        ipv6 = v6;
    }

    Ok((ipv4, ipv6))
}

// ═══════════════════════════════════════════════════════════════════
// NIC-based detection via getifaddrs
// ═══════════════════════════════════════════════════════════════════

fn ip_from_nic(include: &[String], exclude: &[String]) -> (Option<String>, Option<String>) {
    let mut ifa_head: *mut IfAddrs = std::ptr::null_mut();
    let ret = unsafe { getifaddrs(&mut ifa_head) };
    if ret != 0 {
        return (None, None);
    }

    let mut ipv4: Option<String> = None;
    let mut ipv6: Option<String> = None;
    let mut cur = ifa_head;

    while !cur.is_null() {
        let ifa = unsafe { &*cur };

        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();

        // Filter: skip if include list is non-empty and NIC not in it;
        // skip if NIC is in exclude list.
        let included = include.is_empty() || include.contains(&name);
        let excluded = exclude.contains(&name);
        if !included || excluded {
            cur = ifa.ifa_next;
            continue;
        }

        // Skip down or loopback interfaces
        if (ifa.ifa_flags & IFF_UP) == 0 || (ifa.ifa_flags & IFF_LOOPBACK) != 0 {
            cur = ifa.ifa_next;
            continue;
        }

        if ifa.ifa_addr.is_null() {
            cur = ifa.ifa_next;
            continue;
        }

        // macOS/BSD sockaddr has sa_len at offset 0, sa_family at offset 1.
        // Read the family byte directly — the Rust struct definitions below
        // have sa_family at offset 0 (as a u16) for code simplicity, so we
        // cannot use sa.sa_family on macOS.  Address layout at offsets >= 4
        // (sin_addr / sin6_addr) is identical in both layouts.
        let family = unsafe { *(ifa.ifa_addr as *const u8).add(1) };
        match family {
            AF_INET if ipv4.is_none() => {
                let sin = unsafe { &*(ifa.ifa_addr as *const SockAddrIn) };
                let addr = &sin.sin_addr.s_addr;
                ipv4 = Some(format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]));
            }
            AF_INET6 if ipv6.is_none() => {
                let sin6 = unsafe { &*(ifa.ifa_addr as *const SockAddrIn6) };
                let a = &sin6.sin6_addr.s6_addr;
                // Skip link-local (fe80::/10)
                if a[0] == 0xfe && (a[1] & 0xc0) == 0x80 {
                    cur = ifa.ifa_next;
                    continue;
                }
                // Skip unique local (fc00::/7) and multicast (ff00::/8)
                if a[0] == 0xfc || a[0] == 0xfd || a[0] == 0xff {
                    cur = ifa.ifa_next;
                    continue;
                }
                ipv6 = Some(format_ipv6(a));
            }
            _ => {}
        }

        if ipv4.is_some() && ipv6.is_some() {
            break;
        }
        cur = ifa.ifa_next;
    }

    unsafe { freeifaddrs(ifa_head) };
    (ipv4, ipv6)
}

fn format_ipv6(addr: &[u8; 16]) -> String {
    let mut s = String::with_capacity(39);
    for (i, chunk) in addr.chunks(2).enumerate() {
        if i > 0 {
            s.push(':');
        }
        let word = u16::from_be_bytes([chunk[0], chunk[1]]);
        s.push_str(&format!("{:x}", word));
    }
    s
}

// ═══════════════════════════════════════════════════════════════════
// HTTP-based detection (TcpStream, plain HTTP only — best-effort)
// ═══════════════════════════════════════════════════════════════════

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

fn http_get(url: &str) -> Option<String> {
    let (host, port, path) = parse_url(url)?;

    let addr = format!("{}:{}", host, port)
        .to_socket_addrs()
        .ok()?
        .next()?;
    let mut stream = TcpStream::connect_timeout(&addr, HTTP_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT)).ok()?;

    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: curl/8.0.1\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream.write_all(req.as_bytes()).ok()?;

    let mut buf = vec![0u8; 4096];
    let mut body = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if body.len() > 65536 {
            break;
        }
    }

    let body_str = std::str::from_utf8(&body).ok()?;
    let sep = "\r\n\r\n";
    let pos = body_str.find(sep)?;
    let payload = &body_str[pos + sep.len()..];

    Some(payload.to_string())
}

fn parse_url(url: &str) -> Option<(&str, u16, &str)> {
    let rest = url.strip_prefix("http://")?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.find(':') {
        Some(i) => (&host_port[..i], host_port[i + 1..].parse::<u16>().ok()?),
        None => (host_port, 80),
    };
    Some((host, port, path))
}

fn fetch_ipv4_http() -> Option<String> {
    let apis = [
        "http://ipv4.ip.sb",
        "http://api.ipify.org",
        "http://ipv4.icanhazip.com",
    ];
    for url in &apis {
        if let Some(body) = http_get(url)
            && let Some(ip) = extract_ipv4(&body)
        {
            return Some(ip);
        }
    }
    None
}

fn fetch_ipv6_http() -> Option<String> {
    let apis = [
        "http://v6.ip.zxinc.org",
        "http://api6.ipify.org",
        "http://ipv6.icanhazip.com",
    ];
    for url in &apis {
        if let Some(body) = http_get(url)
            && let Some(ip) = extract_ipv6(&body)
        {
            return Some(ip);
        }
    }
    None
}

fn fetch_cf_trace() -> Option<(Option<String>, Option<String>)> {
    let body = http_get("http://cloudflare.com/cdn-cgi/trace")?;
    let mut ipv4: Option<String> = None;
    let mut ipv6: Option<String> = None;
    for line in body.lines() {
        if let Some(val) = line.strip_prefix("ip=") {
            let val = val.trim();
            if val.contains('.') {
                ipv4 = Some(val.to_string());
            } else if val.contains(':') {
                ipv6 = Some(val.to_string());
            }
        }
    }
    if ipv4.is_some() || ipv6.is_some() {
        Some((ipv4, ipv6))
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════════════════
// IP extraction helpers (no regex crate — manual scanning)
// ═══════════════════════════════════════════════════════════════════

fn extract_ipv4(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0u8;
            let mut ok = true;
            while i < len {
                let b = bytes[i];
                if b == b'.' {
                    dots += 1;
                    if dots > 3 {
                        ok = false;
                        break;
                    }
                } else if !b.is_ascii_digit() {
                    break;
                }
                i += 1;
            }
            if ok && dots == 3 {
                let candidate = &s[start..i];
                if candidate
                    .split('.')
                    .all(|o| o.parse::<u16>().is_ok_and(|n| n <= 255))
                {
                    return Some(candidate.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn extract_ipv6(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if b.is_ascii_hexdigit() || b == b':' {
            let start = i;
            let mut colons = 0u8;
            let mut groups = 0u8;
            let mut has_double_colon = false;
            let mut ok = true;
            while i < len {
                let b = bytes[i];
                if b == b':' {
                    if i + 1 < len && bytes[i + 1] == b':' {
                        if has_double_colon {
                            ok = false;
                            break;
                        }
                        has_double_colon = true;
                        i += 1;
                    }
                    colons += 1;
                } else if b.is_ascii_hexdigit() {
                    groups += 1;
                    let mut hex_count = 0;
                    while i < len && bytes[i].is_ascii_hexdigit() && hex_count < 4 {
                        i += 1;
                        hex_count += 1;
                    }
                    continue;
                } else {
                    break;
                }
                i += 1;
            }
            if ok && colons >= 2 && (1..=8).contains(&groups) {
                let candidate = &s[start..i];
                let trimmed =
                    candidate.trim_end_matches(|c: char| !c.is_ascii_hexdigit() && c != ':');
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    None
}
