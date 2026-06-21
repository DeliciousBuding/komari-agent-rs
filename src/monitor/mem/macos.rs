#![allow(dead_code)]
// komari-agent-rs: macOS memory metrics — host_statistics64 + sysctlbyname FFI.
#![cfg(target_os = "macos")]

use crate::config::Config;

/// Collected memory metrics (bytes).
#[derive(Debug, Default, Clone, Copy)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

// ── Mach FFI (libSystem) ─────────────────────────────────────────────────────

type MachPort = u32;
type NaturalT = u32;
type IntegerT = i32;
type MachMsgTypeNumberT = NaturalT;
type KernReturnT = i32;
type VmSizeT = u64;

const HOST_VM_INFO64: i32 = 4;

// vm_statistics64_data_t — must match Darwin xnu layout exactly.
// Fields are packed: natural_t (u32) fields interleaved with u64 fields.
// HOST_VM_INFO64_COUNT = sizeof(struct) / sizeof(integer_t) = 38.
#[repr(C)]
struct VmStatistics64 {
    free_count: NaturalT,                        // offset 0
    active_count: NaturalT,                      // offset 4
    inactive_count: NaturalT,                    // offset 8
    wire_count: NaturalT,                        // offset 12
    zero_fill_count: u64,                        // offset 16
    reactivations: u64,                          // offset 24
    pageins: u64,                                // offset 32
    pageouts: u64,                               // offset 40
    faults: u64,                                 // offset 48
    cow_faults: u64,                             // offset 56
    lookups: u64,                                // offset 64
    hits: u64,                                   // offset 72
    purges: u64,                                 // offset 80
    purgeable_count: NaturalT,                   // offset 88
    speculative_count: NaturalT,                 // offset 92
    decompressions: u64,                         // offset 96
    compressions: u64,                           // offset 104
    swapins: u64,                                // offset 112
    swapouts: u64,                               // offset 120
    compressor_page_count: NaturalT,             // offset 128
    throttled_count: NaturalT,                   // offset 132
    external_page_count: NaturalT,               // offset 136
    internal_page_count: NaturalT,               // offset 140
    total_uncompressed_pages_in_compressor: u64, // offset 144
}

unsafe extern "C" {
    fn mach_host_self() -> MachPort;
    fn host_statistics64(
        host_priv: MachPort,
        flavor: i32,
        host_info64_out: *mut VmStatistics64,
        host_info64_outCnt: *mut MachMsgTypeNumberT,
    ) -> KernReturnT;
}

// ── sysctl FFI (libSystem) ──────────────────────────────────────────────────

#[repr(C)]
struct XswUsage {
    xsu_total: u64,
    xsu_avail: u64,
    xsu_used: u64,
    xsu_pagesize: u32,
    xsu_encrypted: u32,
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const u8,
        oldp: *mut u8,
        oldlenp: *mut usize,
        newp: *const u8,
        newlen: usize,
    ) -> i32;
}

// ── Helpers ─────────────────────────────────────────────────────────────────

unsafe fn sysctl_u64(name: &str) -> Option<u64> {
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&mut val) as *mut u64 as *mut u8,
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if ret == 0 { Some(val) } else { None }
}

unsafe fn sysctl_u32(name: &str) -> Option<u32> {
    let mut val: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&mut val) as *mut u32 as *mut u8,
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if ret == 0 { Some(val) } else { None }
}

unsafe fn sysctl_struct<T>(name: &str, out: &mut T) -> bool {
    let mut len = std::mem::size_of::<T>();
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            out as *mut T as *mut u8,
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    ret == 0
}

// ── collect ─────────────────────────────────────────────────────────────────

/// Collect memory and swap statistics via Mach host_statistics64 and sysctl.
///
/// Memory used = (wire_count + active_count + inactive_count +
///                compressor_page_count) * page_size
///
/// This mirrors the "App Memory + Wired Memory + Compressed" formula used by
/// Activity Monitor and is the closest macOS equivalent to Linux "used".
///
/// Total RAM from `sysctlbyname("hw.memsize")`.
/// Swap from `sysctlbyname("vm.swapusage")`.
///
/// `config` is accepted for signature compatibility; the macOS implementation
/// ignores `memory_include_cache` / `memory_report_raw_used`.
pub fn collect(_config: &Config) -> MemInfo {
    let host = unsafe { mach_host_self() };

    // ── VM statistics ──
    let mut vm_stat: VmStatistics64 = unsafe { std::mem::zeroed() };
    let mut count: MachMsgTypeNumberT =
        (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<IntegerT>()) as u32;

    let kr = unsafe { host_statistics64(host, HOST_VM_INFO64, &mut vm_stat, &mut count) };
    if kr != 0 {
        return MemInfo::default();
    }

    // ── Page size (bytes) via sysctl ──
    let page_size = unsafe { sysctl_u32("hw.pagesize") }.unwrap_or(16384) as u64; // Apple Silicon default = 16 KiB

    // ── Total RAM via sysctl ──
    let total_ram = unsafe { sysctl_u64("hw.memsize") }.unwrap_or(0);

    // Used = wired + active + inactive + compressed (Activity Monitor formula)
    let used_pages = vm_stat.wire_count as u64
        + vm_stat.active_count as u64
        + vm_stat.inactive_count as u64
        + vm_stat.compressor_page_count as u64;
    let used = used_pages.saturating_mul(page_size).min(total_ram);

    // ── Swap via sysctl ──
    let mut xsu: XswUsage = XswUsage {
        xsu_total: 0,
        xsu_avail: 0,
        xsu_used: 0,
        xsu_pagesize: 0,
        xsu_encrypted: 0,
    };
    let swap_ok = unsafe { sysctl_struct("vm.swapusage", &mut xsu) };

    let (swap_total, swap_used) = if swap_ok {
        (xsu.xsu_total, xsu.xsu_used)
    } else {
        (0, 0)
    };

    MemInfo {
        total: total_ram,
        used,
        swap_total,
        swap_used,
    }
}
