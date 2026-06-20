// komari-agent-rs: Linux load average from /proc/loadavg.
//
// Format: "0.25 0.32 0.28 1/456 12345\n"
// Fields: load1 load5 load15 running/total pid

use super::LoadInfo;
use std::fs;
use std::io;

/// Parse `/proc/loadavg` and return 1, 5, 15-minute load averages.
///
/// `/proc/loadavg` contains three space-separated f64 values
/// followed by process counts and the most recent PID.
pub fn collect() -> Result<LoadInfo, io::Error> {
    let contents = fs::read_to_string("/proc/loadavg")?;
    parse_loadavg(&contents)
}

/// Parse a `/proc/loadavg` line.  Handles high-load values (>100) and
/// any whitespace separation between fields.
fn parse_loadavg(s: &str) -> Result<LoadInfo, io::Error> {
    let err = || io::Error::new(io::ErrorKind::InvalidData, "malformed /proc/loadavg");
    let mut fields = s.split_whitespace();
    let mut next_f64 = || {
        fields
            .next()
            .ok_or_else(err)
            .and_then(|v| v.parse::<f64>().map_err(|_| err()))
    };
    Ok(LoadInfo {
        load1: next_f64()?,
        load5: next_f64()?,
        load15: next_f64()?,
    })
}
