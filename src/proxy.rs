// proxy.rs — HTTP CONNECT + SOCKS5 proxy support, NO_PROXY bypass, and a
// unified `Dialer` that owns the full "how do I reach (host, port)" decision.
//
// Design goals (per project AGENTS.md "综合适配"):
//   - The agent must work in ANY network environment, not just our own mihomo
//     TUN setup. That means: direct connect, corporate HTTP proxies, SOCKS5
//     proxies (clash/mihomo/v2ray default), proxies requiring auth, and
//     NO_PROXY bypass lists — all driven by standard env vars.
//   - Pure std (no extra crate).
//   - Sync single-threaded, no async runtime.
//   - When no proxy env var is present, the direct path is byte-for-byte
//     identical to before (custom-dns / prefer-ip-version handled by dns.rs).
//
// Some functions here (e.g. connect_via_proxy, parse_proxy_url_simple) are
// kept as a stable public API surface even if the agent core currently only
// uses the Dialer; future modules (task exec, ping, self-update) consume the
// same primitives.

// This module exposes more public helpers than the binary currently calls; the
// rest form the network-layer API surface reserved for task/ping/update paths.
#![allow(dead_code)]
//
// Env vars honored (curl / Go httpproxy conventions):
//   HTTPS_PROXY / https_proxy   — proxy for https/wss targets
//   ALL_PROXY   / all_proxy      — any scheme, fallback
//   HTTP_PROXY  / http_proxy     — last-resort
//   NO_PROXY    / no_proxy        — bypass list (domains, wildcards, CIDRs)
//
// Proxy URL forms accepted:
//   http://[user:pass@]host[:port]   → HTTP CONNECT tunnel
//   https://[user:pass@]host[:port]  → HTTP CONNECT (proxy hop is plain TCP)
//   socks5://[user:pass@]host[:port]  → SOCKS5, local DNS resolution
//   socks5h://[user:pass@]host[:port] → SOCKS5, remote DNS (proxy resolves)
//   host[:port]                       → treated as HTTP CONNECT

use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::config::Config;

// ═══════════════════════════════════════════════════════════════════════════
// NetErr — unified network-layer error
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug)]
pub enum NetErr {
    /// I/O error from the underlying socket.
    Io(std::io::Error),
    /// Custom DNS resolution failure (from dns.rs).
    Dns(crate::dns::DnsErr),
    /// A proxy handshake (CONNECT or SOCKS5) failed.
    Proxy(String),
}

impl std::fmt::Display for NetErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Dns(e) => write!(f, "DNS: {e}"),
            Self::Proxy(s) => write!(f, "proxy: {s}"),
        }
    }
}

impl std::error::Error for NetErr {}

impl From<std::io::Error> for NetErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<crate::dns::DnsErr> for NetErr {
    fn from(e: crate::dns::DnsErr) -> Self {
        Self::Dns(e)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ProxySpec — parsed proxy URL
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    /// HTTP CONNECT tunnel (RFC 7231 §5.3.3).
    Connect,
    /// SOCKS5 with local DNS resolution (RFC 1928).
    Socks5,
    /// SOCKS5 with remote DNS resolution (proxy resolves the hostname).
    Socks5h,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub auth: Option<(String, String)>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Env lookup + NO_PROXY bypass
// ═══════════════════════════════════════════════════════════════════════════

/// Return the proxy URL configured for `target_host`, honoring NO_PROXY.
///
/// Lookup order for the proxy URL itself (first non-empty wins):
///   HTTPS_PROXY → ALL_PROXY → HTTP_PROXY
///
/// Before returning, the NO_PROXY / no_proxy list is consulted. If
/// `target_host` matches any entry (exact domain, suffix/wildcard, IP, or
/// CIDR), this returns `None` (bypass the proxy and connect directly).
pub fn get_proxy_for(target_host: &str) -> Option<String> {
    if should_bypass_proxy(target_host) {
        return None;
    }
    for var in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Decide whether `target_host` is on the NO_PROXY bypass list.
///
/// Reads the NO_PROXY / no_proxy env var and delegates to
/// [`bypass_list_matches`]. Thin env-reading wrapper around the pure matcher
/// so the matcher itself stays unit-testable without touching global state.
fn should_bypass_proxy(target_host: &str) -> bool {
    let raw = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    bypass_list_matches(target_host, &raw)
}

/// Pure NO_PROXY list matcher — no env access, safe to call concurrently.
///
/// Matches Go's `httpproxy` semantics:
///   - Empty list → never bypass.
///   - `*` → always bypass.
///   - A bare domain `example.com` matches `example.com` and any
///     `*.example.com` subdomain.
///   - A leading-dot entry `.example.com` matches subdomains (and the apex).
///   - CIDR notation (`10.0.0.0/8`) and bare IPs are matched when the host
///     is itself an IP literal.
fn bypass_list_matches(target_host: &str, no_proxy_list: &str) -> bool {
    let list = no_proxy_list.trim();
    if list.is_empty() {
        return false;
    }
    if list == "*" {
        return true;
    }

    // Normalise the host: strip a bracketed IPv6 form and any :port suffix.
    // We keep the original for domain matching but also try parsing as IP.
    let host = target_host.trim_start_matches('[').trim_end_matches(']');
    let host = host.rsplit_once(':').map_or(host, |(h, p)| {
        // Only strip a trailing :port if the remainder parses as u16; this
        // avoids mangling a bare IPv6 address.
        if p.parse::<u16>().is_ok() && !host.contains("::") {
            h
        } else {
            host
        }
    });

    let host_ip: Option<IpAddr> = host.parse::<IpAddr>().ok();

    for entry in list.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if host_matches_entry(host, host_ip, entry) {
            return true;
        }
    }
    false
}

/// Match a single NO_PROXY entry against the (possibly IP) host.
fn host_matches_entry(host: &str, host_ip: Option<IpAddr>, entry: &str) -> bool {
    // CIDR / IP entry → only meaningful against an IP host.
    if let Some(slash) = entry.find('/') {
        if let Some(ip) = host_ip
            && let Ok(net) = entry[..slash].parse::<IpAddr>()
        {
            let prefix: u8 =
                entry[slash + 1..]
                    .parse()
                    .unwrap_or(if net.is_ipv4() { 32 } else { 128 });
            return ip_in_cidr(ip, net, prefix);
        }
        return false;
    }
    if let Ok(entry_ip) = entry.parse::<IpAddr>() {
        return host_ip == Some(entry_ip);
    }

    // Domain matching.
    let entry = entry.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if let Some(suffix) = entry.strip_prefix('.') {
        // ".example.com" matches subdomains only.
        return host == suffix || host.ends_with(&entry);
    }
    // Bare "example.com" matches itself or any subdomain.
    host == entry || host.ends_with(&format!(".{entry}"))
}

/// Test whether `ip` falls inside `net/prefix`. Pure-std, no external crate.
fn ip_in_cidr(ip: IpAddr, net: IpAddr, prefix: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            if prefix > 32 {
                return false;
            }
            let mask: u32 = if prefix == 0 {
                0
            } else {
                (!0u32) << (32 - prefix)
            };
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            if prefix > 128 {
                return false;
            }
            let au = u128::from(a);
            let bu = u128::from(b);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                (!0u128) << (128 - prefix)
            };
            (au & mask) == (bu & mask)
        }
        _ => false,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Proxy URL parsing (scheme + host:port + optional auth)
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a proxy URL into a full [`ProxySpec`] (scheme, host, port, auth).
///
/// Accepted forms:
///   `http://user:pass@host:port`
///   `https://user:pass@host:port`   (proxy hop is still plain TCP)
///   `socks5://user:pass@host:port`
///   `socks5h://user:pass@host:port`
///   `host:port`                      (defaults to HTTP CONNECT)
///   `host`                           (port defaults below)
pub fn parse_proxy_url(proxy: &str) -> Result<ProxySpec, std::io::Error> {
    let proxy = proxy.trim();

    let (scheme, rest) = if let Some(s) = proxy.strip_prefix("socks5h://") {
        (ProxyScheme::Socks5h, s)
    } else if let Some(s) = proxy.strip_prefix("socks5://") {
        (ProxyScheme::Socks5, s)
    } else if let Some(s) = proxy.strip_prefix("socks5h:") {
        // Tolerate the bare-scheme form some tools emit.
        (ProxyScheme::Socks5h, s)
    } else if let Some(s) = proxy.strip_prefix("socks5:") {
        (ProxyScheme::Socks5, s)
    } else if let Some(s) = proxy.strip_prefix("http://") {
        (ProxyScheme::Connect, s)
    } else if let Some(s) = proxy.strip_prefix("https://") {
        (ProxyScheme::Connect, s)
    } else if proxy.starts_with("socks4") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SOCKS4 proxy not supported (use socks5/socks5h)",
        ));
    } else {
        (ProxyScheme::Connect, proxy)
    };

    // Drop any trailing path/query.
    let authority = rest.split('/').next().unwrap_or(rest);

    // Split optional user:pass@  (only the FIRST @ is a separator).
    let (auth, hostport) = match authority.rfind('@') {
        Some(idx) if idx > 0 => {
            let userinfo = &authority[..idx];
            let hostport = &authority[idx + 1..];
            let (user, pass) = match userinfo.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => (userinfo.to_string(), String::new()),
            };
            (Some((user, pass)), hostport)
        }
        _ => (None, authority),
    };

    if hostport.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("missing proxy host in: {proxy}"),
        ));
    }

    // Split host:port. Guard IPv6 brackets: an IPv6 literal looks like
    // [::1]:1080 or [::1].
    let (host, port) = split_host_port(hostport, scheme.default_port())?;

    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if host.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("missing proxy host in: {proxy}"),
        ));
    }

    Ok(ProxySpec {
        scheme,
        host,
        port,
        auth,
    })
}

impl ProxyScheme {
    const fn default_port(self) -> u16 {
        match self {
            ProxyScheme::Connect => 8080,
            ProxyScheme::Socks5 | ProxyScheme::Socks5h => 1080,
        }
    }
}

/// Split `host:port` (or bracketed IPv6) using `default_port` when absent.
/// Returns an error when a `:port` suffix is present but does not parse.
fn split_host_port(hostport: &str, default_port: u16) -> Result<(String, u16), std::io::Error> {
    // Bracketed IPv6: [::1] or [::1]:1080
    if hostport.starts_with('[')
        && let Some(end) = hostport.find(']')
    {
        let host = &hostport[..=end];
        let rest = &hostport[end + 1..];
        if let Some(port_str) = rest.strip_prefix(':') {
            let p = port_str.parse::<u16>().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid proxy port in: {hostport}"),
                )
            })?;
            return Ok((host.to_string(), p));
        }
        return Ok((host.to_string(), default_port));
    }
    match hostport.rfind(':') {
        Some(idx) => {
            let port_part = &hostport[idx + 1..];
            match port_part.parse::<u16>() {
                Ok(p) => Ok((hostport[..idx].to_string(), p)),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid proxy port in: {hostport}"),
                )),
            }
        }
        None => Ok((hostport.to_string(), default_port)),
    }
}

/// Back-compat helper: callers that only need (host, port) for the legacy
/// HTTP-CONNECT path. Prefer [`parse_proxy_url`] + [`establish_tunnel`].
pub fn parse_proxy_url_simple(proxy: &str) -> Result<(String, u16), std::io::Error> {
    let spec = parse_proxy_url(proxy)?;
    Ok((spec.host, spec.port))
}

// ═══════════════════════════════════════════════════════════════════════════
// Tunnel establishment
// ═══════════════════════════════════════════════════════════════════════════

/// Establish a tunnel to `(target_host, target_port)` through `proxy_url`,
/// dispatching to HTTP CONNECT or SOCKS5 based on the URL scheme.
///
/// The returned `TcpStream` is a transparent tunnel; the caller TLS-wraps it
/// and speaks the real protocol (WS / HTTPS) over it.
pub fn establish_tunnel(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
    timeout: Duration,
) -> Result<TcpStream, NetErr> {
    let spec = parse_proxy_url(proxy_url)
        .map_err(|e| NetErr::Proxy(format!("invalid proxy URL '{proxy_url}': {e}")))?;

    // Resolve and connect to the proxy itself.
    let mut sock = connect_tcp(&spec.host, spec.port, timeout)?;

    match spec.scheme {
        ProxyScheme::Connect => {
            http_connect_handshake(
                &mut sock,
                target_host,
                target_port,
                spec.auth.as_ref(),
                timeout,
            )?;
        }
        ProxyScheme::Socks5 => {
            let remote_dns = false;
            socks5_handshake(
                &mut sock,
                target_host,
                target_port,
                spec.auth.as_ref(),
                remote_dns,
            )?;
        }
        ProxyScheme::Socks5h => {
            let remote_dns = true;
            socks5_handshake(
                &mut sock,
                target_host,
                target_port,
                spec.auth.as_ref(),
                remote_dns,
            )?;
        }
    }

    Ok(sock)
}

/// TCP-connect to `host:port` using the system resolver.
fn connect_tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, NetErr> {
    let addr_str = format!("{host}:{port}");
    let mut addrs = addr_str.to_socket_addrs()?;
    let sock_addr = addrs
        .next()
        .ok_or_else(|| NetErr::Proxy(format!("proxy DNS returned no addresses: {addr_str}")))?;
    let sock = TcpStream::connect_timeout(&sock_addr, timeout)?;
    sock.set_read_timeout(Some(timeout))?;
    sock.set_write_timeout(Some(timeout))?;
    let _ = sock.set_nodelay(true);
    Ok(sock)
}

/// Perform an HTTP CONNECT handshake over `sock`, sending Proxy-Authorization
/// when credentials are present.
fn http_connect_handshake(
    sock: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    auth: Option<&(String, String)>,
    timeout: Duration,
) -> Result<(), NetErr> {
    sock.set_read_timeout(Some(timeout))?;
    sock.set_write_timeout(Some(timeout))?;

    let mut req = String::with_capacity(256);
    req.push_str(&format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nProxy-Connection: keep-alive\r\n"
    ));
    if let Some((user, pass)) = auth {
        let creds = format!("{user}:{pass}");
        req.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64_encode_var(creds.as_bytes())
        ));
    }
    req.push_str("\r\n");
    sock.write_all(req.as_bytes())?;
    sock.flush()?;

    // Read until end of headers.
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = sock.read(&mut byte)?;
        if n == 0 {
            return Err(NetErr::Proxy(
                "proxy closed connection during CONNECT".to_string(),
            ));
        }
        buf.push(byte[0]);
        let len = buf.len();
        if len >= 4 && &buf[len - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 8192 {
            return Err(NetErr::Proxy(
                "proxy CONNECT response too large".to_string(),
            ));
        }
    }

    let text = std::str::from_utf8(&buf)
        .map_err(|_| NetErr::Proxy("proxy CONNECT response is not valid UTF-8".into()))?;
    let status_line = text.lines().next().unwrap_or("");
    let code = status_line.split_whitespace().nth(1);
    match code.and_then(|c| c.parse::<u16>().ok()) {
        Some(200) => Ok(()),
        Some(407) => Err(NetErr::Proxy(format!(
            "proxy requires authentication (407): {status_line}"
        ))),
        Some(other) => Err(NetErr::Proxy(format!(
            "proxy CONNECT failed: {status_line} (status {other})"
        ))),
        None => Err(NetErr::Proxy(format!(
            "malformed proxy CONNECT status line: {status_line}"
        ))),
    }
}

/// Perform a SOCKS5 (RFC 1928) handshake, optionally with username/password
/// auth (RFC 1929). When `remote_dns` is true (socks5h), the hostname is sent
/// to the proxy for resolution (DOMAINNAME address type); otherwise we resolve
/// locally and send an IP literal.
fn socks5_handshake(
    sock: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    auth: Option<&(String, String)>,
    remote_dns: bool,
) -> Result<(), NetErr> {
    // 1. Greeting: offer "no auth" (0x00) and, if creds are present,
    //    "username/password" (0x02).
    let greeting: Vec<u8> = if auth.is_some() {
        vec![0x05, 0x02, 0x00, 0x02]
    } else {
        vec![0x05, 0x01, 0x00]
    };
    sock.write_all(&greeting)?;
    sock.flush()?;

    let mut method = [0u8; 2];
    read_exact_proxy(sock, &mut method)?;
    if method[0] != 0x05 {
        return Err(NetErr::Proxy(format!(
            "unexpected SOCKS version {} in greeting reply",
            method[0]
        )));
    }
    match method[1] {
        0x00 => { /* no auth */ }
        0x02 => {
            // Username/password sub-negotiation (RFC 1929).
            let (user, pass) = auth.expect("server picked user/pass but none offered");
            let mut req = Vec::with_capacity(3 + user.len() + pass.len());
            req.push(0x01);
            push_socks_field(&mut req, user.as_bytes());
            push_socks_field(&mut req, pass.as_bytes());
            sock.write_all(&req)?;
            sock.flush()?;
            let mut auth_resp = [0u8; 2];
            read_exact_proxy(sock, &mut auth_resp)?;
            if auth_resp[0] != 0x01 {
                return Err(NetErr::Proxy("malformed SOCKS auth reply".into()));
            }
            if auth_resp[1] != 0x00 {
                return Err(NetErr::Proxy(
                    "SOCKS username/password authentication failed".into(),
                ));
            }
        }
        0xFF => {
            return Err(NetErr::Proxy(
                "proxy offered no acceptable SOCKS auth method".into(),
            ));
        }
        other => {
            return Err(NetErr::Proxy(format!(
                "unsupported SOCKS auth method {other}"
            )));
        }
    }

    // 2. CONNECT request.
    let mut req: Vec<u8> = vec![0x05, 0x01, 0x00]; // VER, CMD=CONNECT, RSV
    if remote_dns {
        // DOMAINNAME: let the proxy resolve.
        req.push(0x03);
        push_socks_field(&mut req, target_host.as_bytes());
    } else {
        // Resolve locally and send IP literal(s). Try the first address.
        match resolve_first(target_host) {
            Some(IpAddr::V4(v4)) => {
                req.push(0x01);
                req.extend_from_slice(&v4.octets());
            }
            Some(IpAddr::V6(v6)) => {
                req.push(0x04);
                req.extend_from_slice(&v6.octets());
            }
            None => {
                // Fall back to remote DNS if local resolution failed.
                req.push(0x03);
                push_socks_field(&mut req, target_host.as_bytes());
            }
        }
    }
    req.extend_from_slice(&target_port.to_be_bytes());
    sock.write_all(&req)?;
    sock.flush()?;

    // 3. Read reply: VER REP RSV ATYP BND.ADDR BND.PORT
    let mut head = [0u8; 4];
    read_exact_proxy(sock, &mut head)?;
    if head[0] != 0x05 {
        return Err(NetErr::Proxy(format!(
            "unexpected SOCKS version {} in connect reply",
            head[0]
        )));
    }
    if head[1] != 0x00 {
        return Err(NetErr::Proxy(socks5_rep_error(head[1])));
    }
    // Drain the bound-address field so the stream is left at clean framing.
    let bnd_len = match head[3] {
        0x01 => 4,  // IPv4
        0x04 => 16, // IPv6
        0x03 => {
            let mut lenbuf = [0u8; 1];
            read_exact_proxy(sock, &mut lenbuf)?;
            lenbuf[0] as usize
        }
        other => return Err(NetErr::Proxy(format!("unknown SOCKS ATYP {other}"))),
    };
    let mut bnd = vec![0u8; bnd_len + 2]; // addr + port
    read_exact_proxy(sock, &mut bnd)?;

    Ok(())
}

/// Push a length-prefixed field (used by SOCKS5 username/password + domain).
fn push_socks_field(out: &mut Vec<u8>, data: &[u8]) {
    // SOCKS5 domains/auth fields use a single-byte length prefix (max 255).
    let len = data.len().min(255);
    out.push(len as u8);
    out.extend_from_slice(&data[..len]);
}

/// Read exactly `buf.len()` bytes from the proxy socket or error.
fn read_exact_proxy(sock: &mut TcpStream, buf: &mut [u8]) -> Result<(), NetErr> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = sock.read(&mut buf[filled..])?;
        if n == 0 {
            return Err(NetErr::Proxy(
                "proxy closed connection during SOCKS handshake".into(),
            ));
        }
        filled += n;
    }
    Ok(())
}

/// Map a SOCKS5 REP code to a human-readable error string.
fn socks5_rep_error(rep: u8) -> String {
    let s = match rep {
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown SOCKS error",
    };
    format!("SOCKS5 connect rejected: {s} (rep={rep})")
}

/// Resolve a hostname to its first address (for local-DNS SOCKS5 mode).
fn resolve_first(host: &str) -> Option<IpAddr> {
    let addr_str = format!("{host}:0");
    addr_str.to_socket_addrs().ok()?.next().map(|s| s.ip())
}

// ═══════════════════════════════════════════════════════════════════════════
// Legacy HTTP-CONNECT entry (kept for http.rs/ws.rs that pre-refactor called
// connect_via_proxy directly). Delegates to establish_tunnel with an explicit
// CONNECT-only URL.
// ═══════════════════════════════════════════════════════════════════════════

/// Establish an HTTP CONNECT tunnel through `proxy_host:proxy_port`.
///
/// Kept for call sites that resolved the proxy manually. New code should use
/// [`establish_tunnel`] with the raw proxy URL (handles scheme + auth).
pub fn connect_via_proxy(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    timeout: Duration,
) -> Result<TcpStream, std::io::Error> {
    let url = format!("http://{proxy_host}:{proxy_port}");
    establish_tunnel(&url, target_host, target_port, timeout).map_err(io_from_net)
}

fn io_from_net(e: NetErr) -> std::io::Error {
    match e {
        NetErr::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dialer — owns the full "reach (host, port)" decision
// ═══════════════════════════════════════════════════════════════════════════

/// Unified network dialer.
///
/// Encapsulates proxy handling (CONNECT / SOCKS5, NO_PROXY bypass, auth) and
/// custom-DNS / IP-version preference for the direct path. Created once from
/// the [`Config`] and threaded into every outbound connection so the entire
/// agent honors the same network policy.
#[derive(Clone)]
pub struct Dialer {
    prefer_ip_version: String,
    custom_dns: Vec<String>,
}

impl Dialer {
    /// Build a dialer from the agent configuration.
    pub fn from_config(config: &Config) -> Self {
        Self {
            prefer_ip_version: config.prefer_ip_version.clone(),
            custom_dns: config.custom_dns.clone(),
        }
    }

    /// A no-frills dialer using the system resolver and no proxy preference.
    /// Useful for tests and for paths that must not honor env proxy vars.
    pub fn system() -> Self {
        Self {
            prefer_ip_version: String::new(),
            custom_dns: Vec::new(),
        }
    }

    /// Connect a TCP stream to `(host, port)`.
    ///
    /// Resolution order:
    ///   1. If a proxy env var applies (and the host is not on NO_PROXY),
    ///      tunnel through it via [`establish_tunnel`].
    ///   2. Otherwise resolve via dns.rs (honoring `--custom-dns` and
    ///      `--prefer-ip-version`) and connect directly, trying each address.
    pub fn connect(&self, host: &str, port: u16, timeout: Duration) -> Result<TcpStream, NetErr> {
        if let Some(proxy_url) = get_proxy_for(host) {
            return establish_tunnel(&proxy_url, host, port, timeout);
        }

        let dial =
            crate::dns::make_dial_context(timeout, &self.prefer_ip_version, &self.custom_dns);
        let addr = format!("{host}:{port}");
        dial("tcp", &addr).map_err(NetErr::Dns)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Variable-length Base64 (for Proxy-Authorization). Standard alphabet.
// ═══════════════════════════════════════════════════════════════════════════

fn base64_encode_var(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 2 < input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | input[i + 2] as u32;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push_str("==");
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── proxy URL parsing ────────────────────────────────────────────────

    #[test]
    fn parse_host_port_bare() {
        let s = parse_proxy_url("127.0.0.1:7897").unwrap();
        assert_eq!(s.scheme, ProxyScheme::Connect);
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 7897);
        assert_eq!(s.auth, None);
    }

    #[test]
    fn parse_http_scheme_default_port() {
        let s = parse_proxy_url("http://proxy.example.com").unwrap();
        assert_eq!(s.host, "proxy.example.com");
        assert_eq!(s.port, 8080);
    }

    #[test]
    fn parse_http_scheme_with_auth() {
        let s = parse_proxy_url("http://bob:s3cr3t@proxy.example.com:8080").unwrap();
        assert_eq!(s.host, "proxy.example.com");
        assert_eq!(s.port, 8080);
        assert_eq!(s.auth, Some(("bob".into(), "s3cr3t".into())));
    }

    #[test]
    fn parse_auth_user_only() {
        let s = parse_proxy_url("http://token@proxy:3128").unwrap();
        assert_eq!(s.auth, Some(("token".into(), "".into())));
    }

    #[test]
    fn parse_socks5_default_port() {
        let s = parse_proxy_url("socks5://127.0.0.1").unwrap();
        assert_eq!(s.scheme, ProxyScheme::Socks5);
        assert_eq!(s.port, 1080);
    }

    #[test]
    fn parse_socks5h_with_auth() {
        let s = parse_proxy_url("socks5h://u:p@127.0.0.1:7890").unwrap();
        assert_eq!(s.scheme, ProxyScheme::Socks5h);
        assert_eq!(s.port, 7890);
        assert_eq!(s.auth, Some(("u".into(), "p".into())));
    }

    #[test]
    fn parse_ipv6_bracketed_host() {
        let s = parse_proxy_url("http://[::1]:8080").unwrap();
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, 8080);
    }

    #[test]
    fn parse_rejects_socks4() {
        assert!(parse_proxy_url("socks4://127.0.0.1:1080").is_err());
    }

    #[test]
    fn parse_strips_path() {
        let s = parse_proxy_url("http://127.0.0.1:7897/foo?bar=1").unwrap();
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 7897);
    }

    #[test]
    fn parse_rejects_bad_port() {
        assert!(parse_proxy_url("127.0.0.1:notaport").is_err());
    }

    #[test]
    fn parse_rejects_empty_host() {
        assert!(parse_proxy_url("http://:8080").is_err());
    }

    // ── NO_PROXY matching (pure, no env access — safe under cargo test's
    //    multi-threaded runner) ─────────────────────────────────────────────

    #[test]
    fn no_proxy_empty_never_bypasses() {
        assert!(!bypass_list_matches("example.com", ""));
        assert!(!bypass_list_matches("example.com", "   "));
    }

    #[test]
    fn no_proxy_star_bypasses_all() {
        assert!(bypass_list_matches("anything.com", "*"));
        assert!(bypass_list_matches("10.0.0.1", "*"));
    }

    #[test]
    fn no_proxy_exact_domain() {
        assert!(bypass_list_matches("example.com", "example.com"));
        assert!(bypass_list_matches("sub.example.com", "example.com"));
        assert!(!bypass_list_matches("other.com", "example.com"));
    }

    #[test]
    fn no_proxy_leading_dot_subdomain_only() {
        // A leading-dot entry matches subdomains and the apex (curl behavior).
        assert!(bypass_list_matches("host.internal.corp", ".internal.corp"));
        assert!(bypass_list_matches("internal.corp", ".internal.corp"));
        assert!(!bypass_list_matches("external.corp", ".internal.corp"));
    }

    #[test]
    fn no_proxy_multiple_entries() {
        let list = "localhost,127.0.0.1,10.0.0.0/8,.local";
        assert!(bypass_list_matches("localhost", list));
        assert!(bypass_list_matches("127.0.0.1", list));
        assert!(bypass_list_matches("10.5.6.7", list));
        assert!(!bypass_list_matches("8.8.8.8", list));
        assert!(bypass_list_matches("printer.local", list));
    }

    #[test]
    fn no_proxy_ipv4_cidr() {
        assert!(bypass_list_matches("192.168.1.100", "192.168.0.0/16"));
        assert!(!bypass_list_matches("192.169.1.1", "192.168.0.0/16"));
    }

    #[test]
    fn no_proxy_ipv6_literal() {
        assert!(bypass_list_matches("::1", "::1"));
        assert!(!bypass_list_matches("::2", "::1"));
    }

    #[test]
    fn no_proxy_ipv6_cidr() {
        assert!(bypass_list_matches("2001:db8::5", "2001:db8::/32"));
        assert!(!bypass_list_matches("2001:db9::5", "2001:db8::/32"));
    }

    #[test]
    fn no_proxy_case_insensitive() {
        assert!(bypass_list_matches("EXAMPLE.com", "Example.COM"));
        assert!(bypass_list_matches("sub.Example.Com", "Example.COM"));
    }

    #[test]
    fn no_proxy_strips_port() {
        // A host with a :port suffix should still match a port-less entry.
        assert!(bypass_list_matches("example.com:443", "example.com"));
        assert!(bypass_list_matches("127.0.0.1:8080", "127.0.0.1"));
    }

    #[test]
    fn no_proxy_bare_ipv4_entry() {
        assert!(bypass_list_matches("10.0.0.1", "10.0.0.1"));
        assert!(!bypass_list_matches("10.0.0.2", "10.0.0.1"));
    }

    #[test]
    fn host_matches_entry_domain_helpers() {
        let none: Option<IpAddr> = None;
        assert!(host_matches_entry("a.b.c", none, "b.c"));
        assert!(!host_matches_entry("x.b.c", none, "a.b.c"));
    }

    // ── CIDR arithmetic ───────────────────────────────────────────────────

    #[test]
    fn cidr_v4_match() {
        assert!(ip_in_cidr(
            "10.20.30.40".parse().unwrap(),
            "10.0.0.0".parse().unwrap(),
            8
        ));
        assert!(!ip_in_cidr(
            "11.0.0.1".parse().unwrap(),
            "10.0.0.0".parse().unwrap(),
            8
        ));
    }

    #[test]
    fn cidr_v4_zero_prefix() {
        assert!(ip_in_cidr(
            "8.8.8.8".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            0
        ));
    }

    #[test]
    fn cidr_v6_match() {
        assert!(ip_in_cidr(
            "2001:db8::1".parse().unwrap(),
            "2001:db8::".parse().unwrap(),
            32
        ));
        assert!(!ip_in_cidr(
            "2001:db9::1".parse().unwrap(),
            "2001:db8::".parse().unwrap(),
            32
        ));
    }

    #[test]
    fn cidr_mismatched_family() {
        assert!(!ip_in_cidr(
            "10.0.0.1".parse().unwrap(),
            "::".parse().unwrap(),
            0
        ));
    }

    // ── base64 ────────────────────────────────────────────────────────────

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode_var(b""), "");
        assert_eq!(base64_encode_var(b"f"), "Zg==");
        assert_eq!(base64_encode_var(b"fo"), "Zm8=");
        assert_eq!(base64_encode_var(b"foo"), "Zm9v");
        assert_eq!(base64_encode_var(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode_var(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode_var(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_userpass() {
        // "Aladdin:OpenSesame" — the RFC 2617 canonical example.
        assert_eq!(
            base64_encode_var(b"Aladdin:OpenSesame"),
            "QWxhZGRpbjpPcGVuU2VzYW1l"
        );
    }

    // ── ProxySpec default ports ───────────────────────────────────────────

    #[test]
    fn socks_field_length_prefix() {
        let mut out = Vec::new();
        push_socks_field(&mut out, b"example.com");
        assert_eq!(out[0], 11);
        assert_eq!(&out[1..], b"example.com");
    }

    #[test]
    fn socks_field_truncates_overlong() {
        let mut out = Vec::new();
        let long = vec![b'a'; 300];
        push_socks_field(&mut out, &long);
        assert_eq!(out.len(), 1 + 255);
        assert_eq!(out[0], 255);
    }

    // ── get_proxy_for runs without panic ──────────────────────────────────

    #[test]
    fn get_proxy_runs_clean() {
        // get_proxy_for must not panic regardless of the surrounding env state.
        let _ = get_proxy_for("example.com");
    }
}
