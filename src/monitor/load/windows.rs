// komari-agent-rs: Windows load average — no native equivalent.
#![cfg(windows)]

use super::LoadInfo;
use std::io;

/// Windows has no native load average.  Returns `0.0` for all three fields.
///
/// An alternative approximation via `GetSystemTimes` + processor queue length
/// is possible but complex and unreliable; the Go komari-agent reference also
/// returns zeros on Windows.
pub fn collect() -> Result<LoadInfo, io::Error> {
    Ok(LoadInfo {
        load1: 0.0,
        load5: 0.0,
        load15: 0.0,
    })
}
