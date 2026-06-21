#![allow(dead_code)]
// komari-agent-rs: macOS process count — sysctlbyname kern.proc.all.
#![cfg(target_os = "macos")]

use std::io;

/// Error type for process-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

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

// struct kinfo_proc — external process info (xnu bsd/sys/sysctl.h)
// Size varies by macOS version.  On macOS 14+ (arm64/x86_64) it is 680 bytes.
// Using PROC_ALL means we get an array of these structs.
const KINFO_PROC_SIZE: usize = 680; // struct kinfo_proc on macOS 14+

// The sysctl MIB for "kern.proc.all" uses CTL_KERN=1, KERN_PROC=14, KERN_PROC_ALL=0
// but sysctlbyname handles the name-to-MIB translation.

// ── collect_process_count ───────────────────────────────────────────────────

/// Count running processes via `sysctlbyname("kern.proc.all")`.
///
/// Queries the total buffer size first, then reads the packed array of
/// `kinfo_proc` structs.  Count = buffer_size / sizeof(kinfo_proc).
///
/// Returns `Ok(0)` when the sysctl fails because this metric is best-effort.
pub fn collect_process_count() -> Result<u32, MetricErr> {
    let name = "kern.proc.all";

    // First call: get required buffer size
    let mut len: usize = 0;
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if ret != 0 || len == 0 {
        return Ok(0);
    }

    // Allocate and fetch
    let mut buf: Vec<u8> = vec![0u8; len];
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if ret != 0 || len < KINFO_PROC_SIZE {
        return Ok(0);
    }

    Ok((len / KINFO_PROC_SIZE) as u32)
}
