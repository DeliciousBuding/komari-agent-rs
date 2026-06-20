// komari-agent-rs: FreeBSD memory metrics — sysctlbyname vm.stats + hw.physmem + kvm_getswapinfo.
#![cfg(target_os = "freebsd")]

use crate::config::Config;

/// Collected memory metrics (bytes).
#[derive(Debug, Default, Clone, Copy)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

// ── sysctl FFI ──────────────────────────────────────────────────────────────

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

unsafe fn sysctl_u64(name: &str) -> Option<u64> {
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let ret = sysctlbyname(
        name.as_ptr(),
        (&mut val) as *mut u64 as *mut u8,
        &mut len,
        std::ptr::null(),
        0,
    );
    if ret == 0 { Some(val) } else { None }
}

unsafe fn sysctl_u32(name: &str) -> Option<u32> {
    let mut val: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    let ret = sysctlbyname(
        name.as_ptr(),
        (&mut val) as *mut u32 as *mut u8,
        &mut len,
        std::ptr::null(),
        0,
    );
    if ret == 0 { Some(val) } else { None }
}

// ── kvm_getswapinfo FFI (libkvm) ────────────────────────────────────────────
//
// kvm_getswapinfo returns total/used swap per device in kilobytes (or pages,
// depending on FreeBSD version).  We multiply by 1024 to get bytes.

type KvmT = *mut std::ffi::c_void;

// struct kvm_swap — matches FreeBSD <kvm.h>
// SPECNAMELEN = 255 on FreeBSD 13+, so ksw_devname is 256 bytes.
#[repr(C)]
struct KvmSwap {
    ksw_devname: [u8; 256], // SPECNAMELEN + 1
    ksw_total: u32,         // total swap in KB (or pages — see manpage)
    ksw_used: u32,          // used swap in KB
    ksw_flags: i32,
}

unsafe extern "C" {
    fn kvm_openfiles(
        execfile: *const u8,
        corefile: *const u8,
        swapfile: *const u8,
        flags: i32,
        errbuf: *mut u8,
    ) -> KvmT;

    fn kvm_getswapinfo(
        kd: KvmT,
        kswap: *mut KvmSwap,
        maxswap: i32,
        flags: i32,
    ) -> i32;

    fn kvm_close(kd: KvmT) -> i32;
}

const O_RDONLY: i32 = 0x0000; // POSIX

/// Query swap statistics via kvm_getswapinfo.
///
/// Opens /dev/mem read-only, queries swap info, closes.  Returns (total, used)
/// in bytes.  On failure returns (0, 0).
unsafe fn swap_via_kvm() -> (u64, u64) {
    let mut errbuf = [0u8; 256];
    let kd = kvm_openfiles(
        std::ptr::null(),
        "/dev/null\0".as_ptr(),
        std::ptr::null(),
        O_RDONLY,
        errbuf.as_mut_ptr(),
    );
    if kd.is_null() {
        return (0, 0);
    }

    let mut kswap = KvmSwap {
        ksw_devname: [0u8; 256],
        ksw_total: 0,
        ksw_used: 0,
        ksw_flags: 0,
    };

    let n = kvm_getswapinfo(kd, &mut kswap, 1, 0);
    kvm_close(kd);

    if n < 1 {
        return (0, 0);
    }

    // kvm_getswapinfo reports in kilobytes → convert to bytes
    let total = (kswap.ksw_total as u64).saturating_mul(1024);
    let used = (kswap.ksw_used as u64).saturating_mul(1024);
    (total, used)
}

// ── collect ─────────────────────────────────────────────────────────────────

/// Collect memory and swap statistics via sysctl and kvm_getswapinfo.
///
/// Memory used = (page_count - free_count - inactive_count - cache_count) * page_size.
/// This matches the standard FreeBSD "used" formula (total minus free/inactive/cache).
///
/// Total RAM from `hw.physmem`.
/// Swap from `kvm_getswapinfo` (primary) with sysctl fallback.
///
/// `config` is accepted for signature compatibility; the FreeBSD implementation
/// ignores `memory_include_cache` / `memory_report_raw_used`.
pub fn collect(_config: &Config) -> MemInfo {
    // ── Page size ──
    let page_size = unsafe { sysctl_u32("vm.stats.vm.v_page_size\0") }
        .unwrap_or(4096) as u64;

    // ── Page counts ──
    let page_count = unsafe { sysctl_u32("vm.stats.vm.v_page_count\0") }.unwrap_or(0) as u64;
    let free_count = unsafe { sysctl_u32("vm.stats.vm.v_free_count\0") }.unwrap_or(0) as u64;
    let inactive_count =
        unsafe { sysctl_u32("vm.stats.vm.v_inactive_count\0") }.unwrap_or(0) as u64;
    let cache_count = unsafe { sysctl_u32("vm.stats.vm.v_cache_count\0") }.unwrap_or(0) as u64;

    let total_pages = page_count;
    let free_pages = free_count + inactive_count + cache_count; // "available-ish" pages
    let used_pages = total_pages.saturating_sub(free_pages);

    // ── Total RAM via hw.physmem (bytes) ──
    let total_ram = unsafe { sysctl_u64("hw.physmem\0") }.unwrap_or(total_pages * page_size);
    let used = (used_pages * page_size).min(total_ram);

    // ── Swap via kvm_getswapinfo ──
    let (swap_total, swap_used) = unsafe { swap_via_kvm() };

    MemInfo {
        total: total_ram,
        used,
        swap_total,
        swap_used,
    }
}
