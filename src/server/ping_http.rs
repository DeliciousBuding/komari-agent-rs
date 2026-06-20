//! HTTP ping — GET/HEAD request, measure RTT, follow redirects.
//! Behind `#[cfg(feature = "ping")]`.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Parse `scheme://host[:port][/path]` into (host, port, path).
/// Defaults: port 80, path "/".
fn parse_http_target(target: &str) -> (&str, u16, &str) {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
        .unwrap_or(target);
    let (host_part, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };
    let (host, port) = match host_part.find(':') {
        Some(i) => (
            &host_part[..i],
            host_part[i + 1..].parse::<u16>().unwrap_or(80),
        ),
        None => (host_part, 80),
    };
    // Strip IPv6 brackets
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    (host, port, path)
}

/// Perform an HTTP GET (or HEAD) to `target`, measuring full round-trip time.
///
/// `target` may be `http://host[:port][/path]` or a bare `host[:port][/path]`
/// (defaults to `http://`).  Measures from TCP connect start to first response
/// byte, excluding DNS resolution.
///
/// Follows up to 3 redirects (301, 302, 303, 307, 308).  Returns RTT in
/// milliseconds, or -1 on any error.
pub fn ping_http(target: &str, timeout_ms: Option<u64>) -> i64 {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(3000));
    let mut url = if target.starts_with("http") {
        target.to_string()
    } else {
        format!("http://{}", target)
    };

    for _redirect in 0..4 {
        let (host_str, port, path_str) = parse_http_target(&url);
        // Convert to owned to break borrow on `url` before potential redirect
        let host = host_str.to_string();
        let path = path_str.to_string();

        // Resolve DNS first (excluded from timing, matching Go behaviour)
        let addr_str = format!("{}:{}", host, port);
        let sock_addrs: Vec<_> = match addr_str.as_str().to_socket_addrs() {
            Ok(iter) => iter.collect(),
            Err(_) => return -1,
        };
        let first_addr = match sock_addrs.first() {
            Some(a) => *a,
            None => return -1,
        };

        let start = Instant::now();
        let mut stream = match TcpStream::connect_timeout(&first_addr, timeout) {
            Ok(s) => s,
            Err(_) => return -1,
        };
        stream.set_read_timeout(Some(timeout)).ok();

        // Build HTTP/1.1 GET request
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: komari-agent-rs/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            path, host
        );
        if stream.write_all(req.as_bytes()).is_err() {
            return -1;
        }

        let mut resp = [0u8; 4096];
        let n = match stream.read(&mut resp) {
            Ok(0) => return -1,
            Ok(n) => n,
            Err(_) => return -1,
        };
        let rtt = start.elapsed().as_millis() as i64;

        // Parse status line for redirects
        let resp_str = std::str::from_utf8(&resp[..n]).unwrap_or("");
        let status = resp_str
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok());

        match status {
            Some(301 | 302 | 303 | 307 | 308) => {
                // Extract Location header
                if let Some(loc) = resp_str
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("location:"))
                    .and_then(|l| l.splitn(2, ':').nth(1))
                {
                    url = loc.trim().to_string();
                    if !url.starts_with("http") {
                        url = format!("http://{}:{}", host, url.trim_start_matches('/'));
                    }
                    continue;
                }
                return rtt;
            }
            Some(200..=399) => return if rtt == 0 { 1 } else { rtt },
            _ => return -1,
        }
    }
    -1 // Too many redirects
}
