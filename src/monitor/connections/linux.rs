// Public API surface for the connections module; some fields/variants are part
// of the cross-platform MetricErr contract but not exercised on every build.
#![allow(dead_code)]

use std::fs;
use std::io;

/// Error type for metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    /// An I/O error occurred while reading a proc file.
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

/// Collected connection counts.
pub struct ConnectionsInfo {
    /// Number of active TCP connections (IPv4 + IPv6).
    pub tcp: u32,
    /// Number of active UDP sockets (IPv4 + IPv6).
    pub udp: u32,
}

/// Collect TCP and UDP connection counts from `/proc/net`.
///
/// Reads the four proc files and counts non-header (entry) lines.
/// An empty file or header-only file contributes 0 to the count.
pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = count_entries("/proc/net/tcp")? + count_entries("/proc/net/tcp6")?;
    let udp = count_entries("/proc/net/udp")? + count_entries("/proc/net/udp6")?;
    Ok(ConnectionsInfo { tcp, udp })
}

/// Count newlines in `path`, subtract 1 for the header line.
/// Returns 0 when the file is empty or contains only the header.
fn count_entries(path: &str) -> Result<u32, MetricErr> {
    let data = fs::read(path)?;
    let newlines = data.iter().filter(|&&b| b == b'\n').count() as u32;
    Ok(if newlines > 0 { newlines - 1 } else { 0 })
}
