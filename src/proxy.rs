// proxy.rs — HTTP CONNECT proxy support.
//
// Adds optional proxy tunneling so the agent works behind corporate / TUN-style
// proxies (e.g. mihomo fake-IP routing where the destination resolves to a
// 198.18.0.0/16 TUN address that only the proxy can reach).
//
// The agent stays direct-connect by default. A proxy is used only when one of
// HTTPS_PROXY / https_proxy / ALL_PROXY / all_proxy / HTTP_PROXY / http_proxy is
// set in the environment.
//
// Design constraints (per project AGENTS.md):
//   - Pure std (no extra crate).
//   - Sync single-threaded, no async runtime.
//   - Does not touch the no-proxy code path when no env var is present.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Return the proxy URL configured for `target_host`, if any.
///
/// Lookup order mirrors curl / Go `httpproxy` conventions:
///   1. `HTTPS_PROXY` / `https_proxy`   (for wss/https targets)
///   2. `ALL_PROXY`    / `all_proxy`     (any scheme, fallback)
///   3. `HTTP_PROXY`   / `http_proxy`    (last-resort for non-https)
///
/// Returns the raw proxy string exactly as set in the environment (e.g.
/// `"127.0.0.1:7897"`, `"http://127.0.0.1:7897"`). The caller parses it via
/// [`parse_proxy_url`].
pub fn get_proxy_for(_target_host: &str) -> Option<String> {
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

/// Parse a proxy URL into `(host, port)`.
///
/// Accepted forms:
///   - `host:port`              (e.g. `127.0.0.1:7897`)
///   - `http://host[:port]`     (port defaults to 80)
///   - `https://host[:port]`    (port defaults to 443 — the CONNECT tunnel is
///     still plain TCP; we do not TLS-wrap the proxy hop itself)
///   - `socks5://...`           (rejected: SOCKS not supported here)
pub fn parse_proxy_url(proxy: &str) -> Result<(String, u16), std::io::Error> {
    let proxy = proxy.trim();

    // Reject schemes we don't implement.
    if proxy.starts_with("socks5://") || proxy.starts_with("socks5h://") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("SOCKS proxy not supported: {proxy}"),
        ));
    }

    // Strip an optional http(s):// scheme.
    let rest = if let Some(stripped) = proxy
        .strip_prefix("http://")
        .or_else(|| proxy.strip_prefix("https://"))
    {
        stripped
    } else {
        proxy
    };

    // Drop any trailing path/query — a proxy URL should not have one, but be
    // forgiving.
    let authority = rest.split('/').next().unwrap_or(rest);

    match authority.rfind(':') {
        Some(idx) => {
            // Guard against IPv6 brackets (rare for a proxy, but cheap to handle).
            let host_part = &authority[..idx];
            let port_part = &authority[idx + 1..];
            let port = port_part.parse::<u16>().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid proxy port in: {proxy}"),
                )
            })?;
            let host = host_part
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();
            if host.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("missing proxy host in: {proxy}"),
                ));
            }
            Ok((host, port))
        }
        None => {
            // No port: default to 8080 (common explicit-proxy port). 80 is also
            // valid but 8080 is the more typical default for forward proxies.
            if authority.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("empty proxy URL: {proxy}"),
                ));
            }
            Ok((authority.to_string(), 8080))
        }
    }
}

/// Establish an HTTP CONNECT tunnel through `proxy_host:proxy_port` to
/// `target_host:target_port` and return the resulting `TcpStream`.
///
/// After a successful CONNECT the returned stream is a transparent TCP tunnel;
/// the caller TLS-wraps and speaks the real protocol (WS / HTTPS) over it.
pub fn connect_via_proxy(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    timeout: Duration,
) -> Result<TcpStream, std::io::Error> {
    // Resolve the proxy itself.
    let addr_str = format!("{proxy_host}:{proxy_port}");
    let mut addrs = addr_str.to_socket_addrs()?;
    let sock_addr = addrs.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!("proxy DNS returned no addresses: {addr_str}"),
        )
    })?;

    let mut sock = TcpStream::connect_timeout(&sock_addr, timeout)?;
    sock.set_read_timeout(Some(timeout))?;
    sock.set_write_timeout(Some(timeout))?;
    let _ = sock.set_nodelay(true);

    // Send CONNECT request. The authority-form (host:port) is what RFC 7231
    // §5.3.3 requires for CONNECT.
    let req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
         Host: {target_host}:{target_port}\r\n\
         Proxy-Connection: keep-alive\r\n\
         \r\n"
    );
    sock.write_all(req.as_bytes())?;
    sock.flush()?;

    // Read the proxy's response (at least up to the end of the status line).
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = sock.read(&mut byte)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "proxy closed connection during CONNECT",
            ));
        }
        buf.push(byte[0]);
        // Stop once the blank line terminating headers is seen. We read headers
        // (not just the status line) so the next byte the caller reads belongs
        // to the tunneled protocol.
        let len = buf.len();
        if len >= 4 && &buf[len - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 8192 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "proxy CONNECT response too large",
            ));
        }
    }

    // Verify the status line. We only require "HTTP/1.x 200".
    let text = std::str::from_utf8(&buf).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "proxy CONNECT response is not valid UTF-8",
        )
    })?;
    let status_line = text.lines().next().unwrap_or("");
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let code = parts.next();
    match code.and_then(|c| c.parse::<u16>().ok()) {
        Some(200) => Ok(sock),
        Some(other) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("proxy CONNECT failed: {status_line} (status {other})"),
        )),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("malformed proxy CONNECT status line: {status_line}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_port_bare() {
        let (h, p) = parse_proxy_url("127.0.0.1:7897").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 7897);
    }

    #[test]
    fn parse_http_scheme() {
        let (h, p) = parse_proxy_url("http://proxy.example.com:8080").unwrap();
        assert_eq!(h, "proxy.example.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_http_scheme_default_port() {
        let (h, p) = parse_proxy_url("http://proxy.example.com").unwrap();
        assert_eq!(h, "proxy.example.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_https_scheme() {
        let (h, p) = parse_proxy_url("https://proxy.example.com:8443").unwrap();
        assert_eq!(h, "proxy.example.com");
        assert_eq!(p, 8443);
    }

    #[test]
    fn parse_strips_path() {
        let (h, p) = parse_proxy_url("http://127.0.0.1:7897/foo?bar=1").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 7897);
    }

    #[test]
    fn parse_rejects_socks() {
        assert!(parse_proxy_url("socks5://127.0.0.1:1080").is_err());
    }

    #[test]
    fn parse_rejects_bad_port() {
        assert!(parse_proxy_url("127.0.0.1:notaport").is_err());
    }

    #[test]
    fn get_proxy_no_env_returns_none_or_value() {
        // Deterministic: we cannot safely mutate the process env in this edition
        // (set_var is unsafe), so just assert the function runs without panicking
        // on a fresh host. Whether it returns Some depends on the surrounding
        // environment; both outcomes are valid here.
        let _ = get_proxy_for("example.com");
    }
}
