// komari-agent-rs: Linux system uptime — reads /proc/uptime.
//
// /proc/uptime format: "uptime_seconds idle_seconds\n"
// Example: "12345.67 12000.12"
// We parse the first space-delimited f64 value and return it as u64 seconds.

use std::fs;
use std::io;

/// Read system uptime from /proc/uptime.
/// Returns the number of seconds since boot as a u64.
pub fn collect_uptime() -> Result<u64, io::Error> {
    let content = fs::read_to_string("/proc/uptime")?;
    let first = content
        .split_whitespace()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty /proc/uptime"))?;
    let secs: f64 = first
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad uptime: {e}")))?;
    Ok(secs as u64)
}
