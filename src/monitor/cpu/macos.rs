#![allow(dead_code)]
// komari-agent-rs: macOS CPU metrics — host_processor_info + sysctlbyname FFI.
#![cfg(target_os = "macos")]

use super::usage_from_ticks;
use crate::arena::ScratchArena;
use std::io;

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

#[derive(Debug, Clone, Copy, Default)]
pub struct PrevCpu {
    pub total: u64,
    pub idle: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo<'a> {
    pub name: &'a str,
    pub cores: u32,
    pub physical_cores: u32,
    pub arch: &'a str,
    pub usage: f64,
}

// ── Mach FFI (libSystem) ─────────────────────────────────────────────────────

type MachPort = u32;
type NaturalT = u32;
type IntegerT = i32;
type MachMsgTypeNumberT = NaturalT;
type KernReturnT = i32;
type VmSizeT = u64;

const PROCESSOR_CPU_LOAD_INFO: i32 = 2;
const CPU_STATE_MAX: usize = 4;

unsafe extern "C" {
    fn mach_host_self() -> MachPort;
    fn host_processor_info(
        host: MachPort,
        flavor: i32,
        out_processor_count: *mut NaturalT,
        out_processor_info: *mut *mut IntegerT,
        out_processor_infoCnt: *mut MachMsgTypeNumberT,
    ) -> KernReturnT;
    fn vm_deallocate(target: MachPort, address: usize, size: VmSizeT) -> KernReturnT;
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

// ── Helpers ─────────────────────────────────────────────────────────────────

unsafe fn sysctl_str<'a>(name: &str, buf: &'a mut [u8]) -> Option<&'a str> {
    let mut len = buf.len();
    let ret = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if ret != 0 || len == 0 {
        return None;
    }
    let end = buf[..len.min(buf.len())]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(len);
    std::str::from_utf8(&buf[..end]).ok()
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

// ── collect_cpu ─────────────────────────────────────────────────────────────

/// Collect CPU metrics: model name via sysctl, core counts via sysctl, and
/// usage percentage via host_processor_info delta.
pub fn collect_cpu<'a>(
    arena: &'a mut ScratchArena,
    prev: &mut PrevCpu,
) -> Result<CpuInfo<'a>, MetricErr> {
    // ── CPU usage via host_processor_info (Mach) ──
    let host = unsafe { mach_host_self() };

    let mut cpu_count: NaturalT = 0;
    let mut cpu_info: *mut IntegerT = std::ptr::null_mut();
    let mut info_count: MachMsgTypeNumberT = 0;

    let kr = unsafe {
        host_processor_info(
            host,
            PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            &mut cpu_info,
            &mut info_count,
        )
    };

    let usage = if kr == 0 && !cpu_info.is_null() && cpu_count > 0 {
        // cpu_info layout per processor: [CPU_STATE_USER, CPU_STATE_SYSTEM,
        // CPU_STATE_IDLE, CPU_STATE_NICE]
        let slots = cpu_count as usize * CPU_STATE_MAX;
        let slice = unsafe { std::slice::from_raw_parts(cpu_info, slots) };

        let mut total_ticks: u64 = 0;
        let mut idle_ticks: u64 = 0;
        for i in 0..cpu_count as usize {
            let base = i * CPU_STATE_MAX;
            // USER + SYSTEM + IDLE + NICE
            let user = slice[base] as u64;
            let sys = slice[base + 1] as u64;
            let idle = slice[base + 2] as u64;
            let nice = slice[base + 3] as u64;
            total_ticks += user + sys + idle + nice;
            idle_ticks += idle;
        }

        let u = usage_from_ticks(prev.total, prev.idle, total_ticks, idle_ticks);

        prev.total = total_ticks;
        prev.idle = idle_ticks;

        // Free the processor info array
        unsafe {
            vm_deallocate(
                mach_host_self(),
                cpu_info as usize,
                (slots as u64) * std::mem::size_of::<IntegerT>() as u64,
            );
        }

        u
    } else {
        // Even on error, don't leave stale data
        if !cpu_info.is_null() {
            unsafe {
                vm_deallocate(
                    mach_host_self(),
                    cpu_info as usize,
                    (info_count as u64) * std::mem::size_of::<IntegerT>() as u64,
                );
            }
        }
        0.0
    };

    // ── CPU name via sysctl ──
    let mut name_buf = [0u8; 256];
    let cpu_name = unsafe { sysctl_str("machdep.cpu.brand_string", &mut name_buf) }
        .unwrap_or("Unknown")
        .to_string();

    // ── Core counts via sysctl ──
    let physical_cores = unsafe { sysctl_u32("hw.physicalcpu") }.unwrap_or(1);
    let logical_cores = unsafe { sysctl_u32("hw.logicalcpu") }.unwrap_or(physical_cores);

    // Arena-alloc strings (same pattern as Linux/Windows)
    let nb = cpu_name.as_bytes();
    let ab = std::env::consts::ARCH.as_bytes();
    let buf = arena.alloc_bytes(nb.len() + ab.len());
    buf[..nb.len()].copy_from_slice(nb);
    buf[nb.len()..].copy_from_slice(ab);
    let name_ref = unsafe { std::str::from_utf8_unchecked(&buf[..nb.len()]) };
    let arch_ref = unsafe { std::str::from_utf8_unchecked(&buf[nb.len()..]) };

    Ok(CpuInfo {
        name: name_ref,
        cores: logical_cores,
        physical_cores,
        arch: arch_ref,
        usage,
    })
}
