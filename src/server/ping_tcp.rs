//! TCP ping — connect to host:port, measure handshake RTT.
//! Behind `#[cfg(feature = "ping")]`.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// TCP-connect ping.
///
/// Resolves `target` (which may be `host` or `host:port`), opens a TCP
/// connection, and returns the connect duration in milliseconds.  If no
/// port is specified, defaults to 80.
///
/// Returns -1 on DNS failure, connection refused, or timeout.
pub fn ping_tcp(target: &str, timeout_ms: Option<u64>) -> i64 {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(3000));

    // Parse host:port — if no port, default to 80
    let addr = if target.contains(':') {
        target.to_string()
    } else {
        format!("{}:80", target)
    };

    // Resolve (use system resolver via ToSocketAddrs — DNS time is included,
    // matching Go behaviour for the TCP path)
    let sock_addrs: Vec<_> = match addr.to_socket_addrs() {
        Ok(iter) => iter.collect(),
        Err(_) => return -1,
    };

    let start = Instant::now();
    for sa in &sock_addrs {
        match TcpStream::connect_timeout(sa, timeout) {
            Ok(stream) => {
                // Clean close
                drop(stream);
                let rtt = start.elapsed().as_millis() as i64;
                return if rtt == 0 { 1 } else { rtt };
            }
            Err(_) => continue,
        }
    }
    -1
}
