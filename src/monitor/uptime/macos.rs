#![allow(dead_code)]
// komari-agent-rs: macOS system uptime — sysctlbyname kern.boottime.
#![cfg(target_os = "macos")]

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

// ── sysctl FFI (libSystem) ──────────────────────────────────────────────────

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

// struct timeval — Darwin 64-bit layout
#[repr(C)]
struct TimeVal {
    tv_sec: i64, // __darwin_time_t = i64 on 64-bit
    tv_usec: i32, // __darwin_suseconds_t = i32
                 // implicit 4-byte padding to 16 bytes total
}

// ── collect_uptime ──────────────────────────────────────────────────────────

/// Read system uptime via `sysctlbyname("kern.boottime")`.
///
/// `kern.boottime` returns a `struct timeval` containing the Unix timestamp of
/// the last boot.  Uptime = current_time - boot_time.
///
/// Falls back to 0 on any failure.
pub fn collect_uptime() -> Result<u64, io::Error> {
    let mut tv: TimeVal = TimeVal {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<TimeVal>();

    let ret = unsafe {
        sysctlbyname(
            "kern.boottime\0".as_ptr(),
            (&mut tv) as *mut TimeVal as *mut u8,
            &mut len,
            std::ptr::null(),
            0,
        )
    };

    if ret != 0 {
        return Err(io::Error::last_os_error());
    }

    let boot_time = tv.tv_sec as u64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok(now.saturating_sub(boot_time))
}
