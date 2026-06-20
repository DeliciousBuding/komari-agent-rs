// komari-agent-rs: FreeBSD process count — sysctlbyname kern.proc.all.
#![cfg(target_os = "freebsd")]

use std::io;

/// Error type for process-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    Io(io::Error),
    Parse(String),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

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

// struct kinfo_proc size on FreeBSD 14+ amd64.
// sizeof(struct kinfo_proc) = 1088 bytes (varies by architecture/version).
// Using KERN_PROC_ALL returns an array of these structs.
const KINFO_PROC_SIZE: usize = 1088;

// ── collect_process_count ───────────────────────────────────────────────────

/// Count running processes via `sysctlbyname("kern.proc.all")`.
///
/// Queries the total buffer size first, then reads the packed array of
/// `kinfo_proc` structs.  Count = buffer_size / sizeof(kinfo_proc).
///
/// Returns `Ok(0)` when the sysctl fails because this metric is best-effort.
pub fn collect_process_count() -> Result<u32, MetricErr> {
    let name = "kern.proc.all\0";

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
