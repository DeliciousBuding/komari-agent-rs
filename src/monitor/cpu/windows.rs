// komari-agent-rs: Windows CPU metrics — GetSystemTimes + registry CPU name.
#![cfg(windows)]

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

// ── FFI: kernel32.dll ────────────────────────────────────────────────────────

#[repr(C)]
struct FileTime {
    dwLowDateTime: u32,
    dwHighDateTime: u32,
}

unsafe extern "system" {
    fn GetSystemTimes(
        lpIdleTime: *mut FileTime,
        lpKernelTime: *mut FileTime,
        lpUserTime: *mut FileTime,
    ) -> i32;
}

// ── FFI: advapi32.dll (registry) ─────────────────────────────────────────────

unsafe extern "system" {
    fn RegOpenKeyExW(
        hKey: isize,
        lpSubKey: *const u16,
        ulOptions: u32,
        samDesired: u32,
        phkResult: *mut isize,
    ) -> i32;

    fn RegQueryValueExW(
        hKey: isize,
        lpValueName: *const u16,
        lpReserved: *const u8,
        lpType: *mut u32,
        lpData: *mut u8,
        lpcbData: *mut u32,
    ) -> i32;

    fn RegCloseKey(hKey: isize) -> i32;
}

const HKEY_LOCAL_MACHINE: isize = (0x80000002u32 as i32) as isize;
const KEY_READ: u32 = 0x20019;
const REG_SZ: u32 = 1;
const ERROR_SUCCESS: i32 = 0;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn filetime_to_u64(ft: &FileTime) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

fn read_cpu_name_from_registry() -> Result<String, MetricErr> {
    let subkey = to_wide("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
    let value_name = to_wide("ProcessorNameString");

    let mut hkey: isize = 0;
    let ret = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut hkey) };
    if ret != ERROR_SUCCESS {
        return Err(MetricErr::Parse("failed to open CPU registry key".into()));
    }

    let mut data_type: u32 = 0;
    let mut data_size: u32 = 256;
    let mut buf = vec![0u8; 256];

    let ret = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut data_type,
            buf.as_mut_ptr(),
            &mut data_size,
        )
    };

    unsafe {
        RegCloseKey(hkey);
    }

    if ret != ERROR_SUCCESS || data_type != REG_SZ || data_size < 2 {
        return Err(MetricErr::Parse(
            "failed to read ProcessorNameString".into(),
        ));
    }

    let wide: &[u16] =
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, (data_size as usize) / 2) };
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16(&wide[..end])
        .map_err(|_| MetricErr::Parse("invalid UTF-16 in CPU name".into()))
}

// ── collect_cpu ──────────────────────────────────────────────────────────────

/// Collect CPU metrics: model name from registry, core counts, arch, and usage
/// percentage via GetSystemTimes delta.
pub fn collect_cpu<'a>(
    arena: &'a mut ScratchArena,
    prev: &mut PrevCpu,
) -> Result<CpuInfo<'a>, MetricErr> {
    let mut idle = FileTime {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FileTime {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FileTime {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };

    let ret = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ret == 0 {
        return Err(MetricErr::Io(io::Error::last_os_error()));
    }

    let idle_u64 = filetime_to_u64(&idle);
    let total_u64 = filetime_to_u64(&kernel) + filetime_to_u64(&user);

    let usage = if prev.total > 0 && total_u64 > prev.total {
        let td = (total_u64 - prev.total) as f64;
        ((td - (idle_u64 - prev.idle) as f64) / td) * 100.0
    } else {
        0.0
    };
    prev.total = total_u64;
    prev.idle = idle_u64;

    let cpu_name = read_cpu_name_from_registry().unwrap_or_else(|_| "Unknown".to_string());

    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let physical_cores = cores;

    let nb = cpu_name.as_bytes();
    let ab = std::env::consts::ARCH.as_bytes();
    let buf = arena.alloc_bytes(nb.len() + ab.len());
    buf[..nb.len()].copy_from_slice(nb);
    buf[nb.len()..].copy_from_slice(ab);
    let name_ref = unsafe { std::str::from_utf8_unchecked(&buf[..nb.len()]) };
    let arch_ref = unsafe { std::str::from_utf8_unchecked(&buf[nb.len()..]) };

    Ok(CpuInfo {
        name: name_ref,
        cores,
        physical_cores,
        arch: arch_ref,
        usage,
    })
}
