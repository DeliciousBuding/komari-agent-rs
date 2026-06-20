// komari-agent-rs: Windows IP detection — GetAdaptersAddresses + HTTP fallback chain.
#![cfg(windows)]

use crate::config::Config;
use std::fmt;
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
// FFI: iphlpapi.dll — GetAdaptersAddresses
// ═══════════════════════════════════════════════════════════════════
// Offsets verified against Windows 11 SDK for x86_64.
// Windows uses AF_INET6 = 23 (not 10 as on POSIX).

const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: u32 = 1;
const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
const ERROR_BUFFER_OVERFLOW: u32 = 111;

#[repr(C)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

#[repr(C)]
struct SockAddrIn6 {
    sin6_family: u16,
    sin6_port: u16,
    sin6_flowinfo: u32,
    sin6_addr: [u8; 16],
    sin6_scope_id: u32,
}

/// Minimal `IP_ADAPTER_UNICAST_ADDRESS` for x64.
/// Fields used: Next, Address (lpSockaddr).
#[repr(C)]
struct IPAdapterUnicastAddress {
    _pad0: [u8; 0x08],                          // Length + Flags (u64)
    next: *mut IPAdapterUnicastAddress,          // offset 0x08
    lp_sockaddr: *mut u8,                        // offset 0x10 (SOCKET_ADDRESS.lpSockaddr)
    _pad1: [u8; 0x20 - 0x18],                   // iSockaddrLength + fields after
}

/// Minimal `IP_ADAPTER_ADDRESSES` for x64 Windows 10/11.
/// Fields used: Next, FirstUnicastAddress, FriendlyName, IfType, OperStatus.
#[repr(C)]
struct IPAdapterAddresses {
    _pad0: [u8; 0x08],                           // Length + IfIndex (u64)
    next: *mut IPAdapterAddresses,                // offset 0x08
    _pad1: [u8; 0x18 - 0x10],                    // AdapterName (PSTR)
    first_unicast: *mut IPAdapterUnicastAddress,  // offset 0x18
    _pad2: [u8; 0x48 - 0x20],                    // several pointers + Description
    friendly_name: *mut u16,                      // offset 0x48 (PWSTR)
    _pad3: [u8; 0x68 - 0x50],                    // PhysicalAddress..Mtu
    if_type: u32,                                 // offset 0x68
    oper_status: u32,                             // offset 0x6C
}

unsafe extern "system" {
    fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut std::ffi::c_void,
        adapter_addresses: *mut IPAdapterAddresses,
        size_pointer: *mut u32,
    ) -> u32;
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

    // 3. External HTTP APIs
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
// NIC-based detection via GetAdaptersAddresses
// ═══════════════════════════════════════════════════════════════════

fn ip_from_nic(include: &[String], exclude: &[String]) -> (Option<String>, Option<String>) {
    let mut size: u32 = 0;
    let ret = unsafe {
        GetAdaptersAddresses(
            0, // AF_UNSPEC — get both IPv4 and IPv6
            GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };

    if ret != ERROR_BUFFER_OVERFLOW || size == 0 {
        return (None, None);
    }

    let mut buf: Vec<u8> = vec![0u8; size as usize];
    let ret = unsafe {
        GetAdaptersAddresses(
            0,
            GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut IPAdapterAddresses,
            &mut size,
        )
    };

    if ret != 0 {
        return (None, None);
    }

    let mut ipv4: Option<String> = None;
    let mut ipv6: Option<String> = None;

    let mut cur = buf.as_ptr() as *const IPAdapterAddresses;

    while !cur.is_null() {
        let adapter = unsafe { &*cur };

        // Skip down or loopback
        if adapter.oper_status != IF_OPER_STATUS_UP || adapter.if_type == IF_TYPE_SOFTWARE_LOOPBACK {
            cur = adapter.next;
            continue;
        }

        // NIC include/exclude filtering on friendly name
        let name = unsafe { wide_ptr_to_string(adapter.friendly_name) };
        let included = include.is_empty() || include.iter().any(|n| name.contains(n.as_str()));
        let excluded = exclude.iter().any(|n| name.contains(n.as_str()));
        if !included || excluded {
            cur = adapter.next;
            continue;
        }

        // Walk unicast addresses
        let mut ua = adapter.first_unicast;
        while !ua.is_null() {
            let unicast = unsafe { &*ua };
            if unicast.lp_sockaddr.is_null() {
                ua = unicast.next;
                continue;
            }

            let family = unsafe { *(unicast.lp_sockaddr as *const u16) };

            match family {
                AF_INET if ipv4.is_none() => {
                    let sin = unsafe { &*(unicast.lp_sockaddr as *const SockAddrIn) };
                    let a = &sin.sin_addr;
                    ipv4 = Some(format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]));
                }
                AF_INET6 if ipv6.is_none() => {
                    let sin6 = unsafe { &*(unicast.lp_sockaddr as *const SockAddrIn6) };
                    let a = &sin6.sin6_addr;
                    // Skip link-local (fe80::/10)
                    if a[0] == 0xfe && (a[1] & 0xc0) == 0x80 {
                        ua = unicast.next;
                        continue;
                    }
                    ipv6 = Some(format_ipv6(a));
                }
                _ => {}
            }

            if ipv4.is_some() && ipv6.is_some() {
                break;
            }
            ua = unicast.next;
        }

        if ipv4.is_some() && ipv6.is_some() {
            break;
        }
        cur = adapter.next;
    }

    (ipv4, ipv6)
}

unsafe fn wide_ptr_to_string(ptr: *const u16) -> String { unsafe {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let wide = std::slice::from_raw_parts(ptr, len);
    String::from_utf16(wide).unwrap_or_default()
}}

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
// HTTP-based detection (identical to Linux)
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
        if let Some(body) = http_get(url) {
            if let Some(ip) = extract_ipv4(&body) {
                return Some(ip);
            }
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
        if let Some(body) = http_get(url) {
            if let Some(ip) = extract_ipv6(&body) {
                return Some(ip);
            }
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
// IP extraction helpers (no regex — manual scanning)
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
                    .all(|o| o.parse::<u16>().map_or(false, |n| n <= 255))
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
            if ok && colons >= 2 && groups >= 1 && groups <= 8 {
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
