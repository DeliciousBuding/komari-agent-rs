// komari-agent-rs: Windows memory metrics — GlobalMemoryStatusEx.
#![cfg(windows)]

use crate::config::Config;

/// Collected memory metrics (bytes).
#[derive(Debug, Default, Clone, Copy)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

// ── FFI: kernel32.dll ────────────────────────────────────────────────────────

#[repr(C)]
struct MemoryStatusEx {
    dwLength: u32,
    dwMemoryLoad: u32,
    ullTotalPhys: u64,
    ullAvailPhys: u64,
    ullTotalPageFile: u64,
    ullAvailPageFile: u64,
    ullTotalVirtual: u64,
    ullAvailVirtual: u64,
    ullAvailExtendedVirtual: u64,
}

unsafe extern "system" {
    fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
}

// ── collect ──────────────────────────────────────────────────────────────────

/// Collect memory and swap statistics via `GlobalMemoryStatusEx`.
///
/// - used = total_phys - avail_phys
/// - swap_total = total_page_file
/// - swap_used = total_page_file - avail_page_file
///
/// `config` is accepted for signature compatibility; the Windows implementation
/// ignores `memory_include_cache` / `memory_report_raw_used` because
/// GlobalMemoryStatusEx does not expose per-cache breakdowns.
pub fn collect(_config: &Config) -> MemInfo {
    let mut msex = MemoryStatusEx {
        dwLength: std::mem::size_of::<MemoryStatusEx>() as u32,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };

    let ret = unsafe { GlobalMemoryStatusEx(&mut msex) };
    if ret == 0 {
        return MemInfo::default();
    }

    MemInfo {
        total: msex.ullTotalPhys,
        used: msex.ullTotalPhys.saturating_sub(msex.ullAvailPhys),
        swap_total: msex.ullTotalPageFile,
        swap_used: msex.ullTotalPageFile.saturating_sub(msex.ullAvailPageFile),
    }
}
