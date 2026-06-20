// komari-agent-rs: FreeBSD system uptime — sysctlbyname kern.boottime.
#![cfg(target_os = "freebsd")]

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

// ── sysctl FFI (libc) ──────────────────────────────────────────────────────

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

// struct timeval — FreeBSD 64-bit layout
#[repr(C)]
struct TimeVal {
    tv_sec: i64,  // time_t is i64 on 64-bit FreeBSD
    tv_usec: i64, // suseconds_t is i64 on 64-bit FreeBSD (unlike Darwin where it's i32)
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
