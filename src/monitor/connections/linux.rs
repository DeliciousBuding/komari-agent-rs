// Public API surface for the connections module; some fields/variants are part
// of the cross-platform MetricErr contract but not exercised on every build.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::process::Command;

use crate::monitor::run_with_timeout;

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
///
/// If `/proc/net` is unreadable (e.g. inside a container with a masked procfs),
/// falls back to running `ss -t -n` / `ss -u -n`, then `netstat -t -n` /
/// `netstat -u -n`, counting non-header output lines.
pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = count_entries("/proc/net/tcp")
        .map(|v4| v4 + count_entries("/proc/net/tcp6").unwrap_or(0))
        .or_else(|_| {
            count_via_subprocess(&["-t", "-n"])
                .ok_or_else(|| std::io::Error::other("ss/netstat unavailable"))
        })
        .unwrap_or(0);

    let udp = count_entries("/proc/net/udp")
        .map(|v4| v4 + count_entries("/proc/net/udp6").unwrap_or(0))
        .or_else(|_| {
            count_via_subprocess(&["-u", "-n"])
                .ok_or_else(|| std::io::Error::other("ss/netstat unavailable"))
        })
        .unwrap_or(0);

    Ok(ConnectionsInfo { tcp, udp })
}

/// Count non-header lines from `ss` or `netstat` subprocess output.
///
/// Tries `ss` first (faster, less overhead), then falls back to `netstat`.
/// Returns `None` when neither tool is available on the system.
fn count_via_subprocess(args: &[&str]) -> Option<u32> {
    let mut ss_cmd = Command::new("ss");
    ss_cmd.args(args);
    let output = run_with_timeout(&mut ss_cmd, 30)
        .or_else(|_| {
            let mut ns_cmd = Command::new("netstat");
            ns_cmd.args(args);
            run_with_timeout(&mut ns_cmd, 30)
        })
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line is a header (State / Recv-Q / Send-Q …); count the rest.
    let lines: u32 = stdout
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count() as u32;
    Some(lines)
}

/// Count newlines in `path`, subtract 1 for the header line.
/// Returns 0 when the file is empty or contains only the header.
fn count_entries(path: &str) -> Result<u32, MetricErr> {
    let data = fs::read(path)?;
    let newlines = data.iter().filter(|&&b| b == b'\n').count() as u32;
    Ok(if newlines > 0 { newlines - 1 } else { 0 })
}
