// komari-agent-rs: FreeBSD CPU metrics — sysctlbyname kern.cp_times + hw.model + hw.ncpu.
#![cfg(target_os = "freebsd")]

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

// ── sysctl FFI (libc) ──────────────────────────────────────────────────────

// FreeBSD CPUSTATES = 5: CP_USER=0, CP_NICE=1, CP_SYS=2, CP_INTR=3, CP_IDLE=4
const CPUSTATES: usize = 5;

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

// ── collect_cpu ─────────────────────────────────────────────────────────────

/// Collect CPU metrics: model name via `hw.model`, core count via `hw.ncpu`,
/// and usage percentage via `kern.cp_times` delta.
///
/// On FreeBSD, `kern.cp_times` returns an array of `ncpu * CPUSTATES` longs
/// (8 bytes each on 64-bit).  States are user, nice, sys, intr, idle.
pub fn collect_cpu<'a>(
    arena: &'a mut ScratchArena,
    prev: &mut PrevCpu,
) -> Result<CpuInfo<'a>, MetricErr> {
    // ── CPU usage via kern.cp_times ──
    let ncpu = unsafe { sysctl_u32("hw.ncpu") }.unwrap_or(1) as usize;
    let slots = ncpu * CPUSTATES; // 5 longs per CPU

    let mut len: usize = 0;
    // First call: get required buffer size
    let ret = unsafe {
        sysctlbyname(
            "kern.cp_times\0".as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };

    let mut usage = 0.0;

    if ret == 0 && len >= slots * std::mem::size_of::<u64>() {
        let mut buf: Vec<u64> = vec![0u64; slots];
        let ret2 = unsafe {
            sysctlbyname(
                "kern.cp_times\0".as_ptr(),
                buf.as_mut_ptr() as *mut u8,
                &mut len,
                std::ptr::null(),
                0,
            )
        };

        if ret2 == 0 {
            let mut total_ticks: u64 = 0;
            let mut idle_ticks: u64 = 0;

            for i in 0..ncpu {
                let base = i * CPUSTATES;
                // CP_USER(0) + CP_NICE(1) + CP_SYS(2) + CP_INTR(3) + CP_IDLE(4)
                total_ticks +=
                    buf[base] + buf[base + 1] + buf[base + 2] + buf[base + 3] + buf[base + 4];
                idle_ticks += buf[base + 4]; // CP_IDLE
            }

            if prev.total > 0 && total_ticks > prev.total {
                let td = (total_ticks - prev.total) as f64;
                usage = ((td - (idle_ticks - prev.idle) as f64) / td) * 100.0;
            }

            prev.total = total_ticks;
            prev.idle = idle_ticks;
        }
    }

    // ── CPU name via hw.model ──
    let mut name_buf = [0u8; 256];
    let cpu_name = unsafe { sysctl_str("hw.model\0", &mut name_buf) }
        .unwrap_or("Unknown")
        .to_string();

    // ── Core counts: hw.ncpu = logical cores ──
    let logical_cores = unsafe { sysctl_u32("hw.ncpu\0") }.unwrap_or(1);

    // FreeBSD doesn't expose hw.physicalcpu — use logical_cores as fallback.
    let physical_cores = logical_cores;

    // Arena-alloc strings (same pattern as Linux/macOS/Windows)
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
