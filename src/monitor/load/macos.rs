// komari-agent-rs: macOS load average — getloadavg FFI.
#![cfg(target_os = "macos")]

use super::LoadInfo;
use std::io;

// ── FFI: getloadavg (libSystem) ─────────────────────────────────────────────

unsafe extern "C" {
    fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
}

// ── collect ─────────────────────────────────────────────────────────────────

/// Collect 1, 5, and 15-minute load averages via `getloadavg`.
///
/// `getloadavg` returns the number of samples written (up to `nelem`).
/// On macOS it always returns 3 when the call succeeds.
pub fn collect() -> Result<LoadInfo, io::Error> {
    let mut load = [0.0f64; 3];
    let ret = unsafe { getloadavg(load.as_mut_ptr(), 3) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LoadInfo {
        load1: load[0],
        load5: load[1],
        load15: load[2],
    })
}
