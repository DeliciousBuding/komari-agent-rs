// komari-agent-rs: Windows system uptime — GetTickCount64.
#![cfg(windows)]

use std::io;

/// Read system uptime via `GetTickCount64`.
///
/// Returns the number of seconds since boot as a `u64`.
/// `GetTickCount64` returns milliseconds since system start; wraps after
/// 584 million years so this is safe for any practical uptime.
pub fn collect_uptime() -> Result<u64, io::Error> {
    let ms = unsafe { GetTickCount64() };
    Ok(ms / 1000)
}

// ── FFI: kernel32.dll ────────────────────────────────────────────────────────

unsafe extern "system" {
    fn GetTickCount64() -> u64;
}
