// komari-agent-rs: Linux IP detection — NIC via getifaddrs FFI + external HTTP APIs.
//
// Endpoints matched from D:/Code/Projects/external/komari-agent-go/monitoring/unit/ip.go
// (7 IPv4 + 4 IPv6 endpoints in the Go reference; 3+3+cf-trace used here for simplicity).
#![allow(dead_code)]

use crate::config::Config;
use core::ffi::c_void;
use core::fmt;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════
// MetricErr — lightweight error for monitoring metric collection
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
// FFI: getifaddrs / freeifaddrs (glibc, no libc crate dependency)
// ═══════════════════════════════════════════════════════════════════

const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;
const IFF_UP: u32 = 1;
const IFF_LOOPBACK: u32 = 8;

#[repr(C)]
struct in_addr {
    s_addr: [u8; 4],
}

#[repr(C)]
struct in6_addr {
    s6_addr: [u8; 16],
}

#[repr(C)]
struct sockaddr {
    sa_family: u16,
    sa_data: [u8; 14],
}

#[repr(C)]
struct sockaddr_in {
    sin_family: u16,
    sin_port: u16,
    sin_addr: in_addr,
    sin_zero: [u8; 8],
}

#[repr(C)]
struct sockaddr_in6 {
    sin6_family: u16,
    sin6_port: u16,
    sin6_flowinfo: u32,
    sin6_addr: in6_addr,
    sin6_scope_id: u32,
}

#[repr(C)]
struct ifaddrs {
    ifa_next: *mut ifaddrs,
    ifa_name: *mut core::ffi::c_char,
    ifa_flags: u32,
    _pad: u32,
    ifa_addr: *mut sockaddr,
    ifa_netmask: *mut sockaddr,
    ifa_ifu: *mut sockaddr,
    ifa_data: *mut c_void,
}

unsafe extern "C" {
    fn getifaddrs(ifap: *mut *mut ifaddrs) -> i32;
    fn freeifaddrs(ifa: *mut ifaddrs);
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
    if ipv4.is_none() && ipv6.is_none() {
        if let Some((v4, v6)) = fetch_cf_trace() {
            ipv4 = v4;
            ipv6 = v6;
        }
    }

    Ok((ipv4, ipv6))
}

// ═══════════════════════════════════════════════════════════════════
// NIC-based detection
// ═══════════════════════════════════════════════════════════════════

fn ip_from_nic(include: &[String], exclude: &[String]) -> (Option<String>, Option<String>) {
    let mut ifa_head: *mut ifaddrs = core::ptr::null_mut();
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
        let included = include.is_empty() || include.iter().any(|n| *n == name);
        let excluded = exclude.iter().any(|n| *n == name);
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

        let sa = unsafe { &*ifa.ifa_addr };

        match sa.sa_family {
            AF_INET if ipv4.is_none() => {
                let sin = unsafe { &*(ifa.ifa_addr as *const sockaddr_in) };
                let addr = &sin.sin_addr.s_addr;
                ipv4 = Some(format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]));
            }
            AF_INET6 if ipv6.is_none() => {
                let sin6 = unsafe { &*(ifa.ifa_addr as *const sockaddr_in6) };
                let a = &sin6.sin6_addr.s6_addr;
                // Skip link-local (fe80::/10)
                if a[0] == 0xfe && (a[1] & 0xc0) == 0x80 {
                    cur = ifa.ifa_next;
                    continue;
                }
                // Format as colon-hex groups
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
        // Zero-compression: we use the simple "never compress" form to avoid complexity.
        // The full form is always valid per RFC 4291 §2.2.
        s.push_str(&format!("{:x}", word));
    }
    s
}

// ═══════════════════════════════════════════════════════════════════
// HTTP-based detection (TcpStream, plain HTTP only — best-effort)
// ═══════════════════════════════════════════════════════════════════

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

fn http_get(url: &str) -> Option<String> {
    // Parse host:port from URL
    let (host, port, path) = parse_url(url)?;

    // Resolve and connect
    let addr = format!("{}:{}", host, port)
        .to_socket_addrs()
        .ok()?
        .next()?;
    let mut stream = TcpStream::connect_timeout(&addr, HTTP_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT)).ok()?;

    // Send HTTP GET
    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: curl/8.0.1\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream.write_all(req.as_bytes()).ok()?;

    // Read response
    let mut buf = vec![0u8; 4096];
    let mut body = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if body.len() > 65536 {
            break; // cap at 64 KiB
        }
    }

    // Split headers from body at \r\n\r\n
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

/// Try each IPv4 HTTP endpoint; return first match.
fn fetch_ipv4_http() -> Option<String> {
    let apis = [
        "http://ipv4.ip.sb",
        "http://api.ipify.org",
        "http://ipv4.icanhazip.com",
    ];
    for url in &apis {
        if let Some(body) = http_get(url) {
            if let Some(ip) = extract_ipv4(&body) {
                return Some(ip);
            }
        }
    }
    None
}

/// Try each IPv6 HTTP endpoint; return first match.
fn fetch_ipv6_http() -> Option<String> {
    let apis = [
        "http://v6.ip.zxinc.org",
        "http://api6.ipify.org",
        "http://ipv6.icanhazip.com",
    ];
    for url in &apis {
        if let Some(body) = http_get(url) {
            if let Some(ip) = extract_ipv6(&body) {
                return Some(ip);
            }
        }
    }
    None
}

/// Cloudflare trace fallback — parse "ip=" line from /cdn-cgi/trace.
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
        // Look for start of an IPv4-like pattern: digit
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
                // Validate each octet is 0-255
                if candidate
                    .split('.')
                    .all(|o| o.parse::<u16>().map_or(false, |n| n <= 255))
                {
                    return Some(candidate.to_string());
                }
            }
            // Continue scanning
        } else {
            i += 1;
        }
    }
    None
}

fn extract_ipv6(s: &str) -> Option<String> {
    // Match: 1-8 colon-separated hex groups (full or compressed form).
    // Scan for a run of hex digits and colons that looks like an IPv6 address.
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
                        i += 1; // consume second colon below
                    }
                    colons += 1;
                } else if b.is_ascii_hexdigit() {
                    groups += 1;
                    // Consume 1-4 hex digits
                    let mut hex_count = 0;
                    while i < len && bytes[i].is_ascii_hexdigit() && hex_count < 4 {
                        i += 1;
                        hex_count += 1;
                    }
                    continue; // don't increment i again
                } else {
                    break;
                }
                i += 1;
            }
            if ok && colons >= 2 && groups >= 1 && groups <= 8 {
                let candidate = &s[start..i];
                // Trim trailing colons and non-hex chars
                let trimmed =
                    candidate.trim_end_matches(|c: char| !c.is_ascii_hexdigit() && c != ':');
                // Verify starts with hex digit or colon
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
