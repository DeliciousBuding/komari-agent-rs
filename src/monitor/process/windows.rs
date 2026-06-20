// komari-agent-rs: Windows process count — CreateToolhelp32Snapshot.
#![cfg(windows)]

use std::io;

/// Error type for process-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    /// An I/O error occurred.
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

// ── FFI: kernel32.dll ────────────────────────────────────────────────────────

const TH32CS_SNAPPROCESS: u32 = 0x00000002;
const INVALID_HANDLE_VALUE: isize = -1;
const MAX_PATH: usize = 260;

#[repr(C)]
struct ProcessEntry32W {
    dwSize: u32,
    cntUsage: u32,
    th32ProcessID: u32,
    th32DefaultHeapID: usize,
    th32ModuleID: u32,
    cntThreads: u32,
    th32ParentProcessID: u32,
    pcPriClassBase: i32,
    dwFlags: u32,
    szExeFile: [u16; MAX_PATH],
}

unsafe extern "system" {
    fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> isize;
    fn Process32FirstW(hSnapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(hSnapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(hObject: isize) -> i32;
}

// ── collect_process_count ────────────────────────────────────────────────────

/// Count running processes by enumerating the system process list via
/// `CreateToolhelp32Snapshot` + `Process32FirstW` / `Process32NextW`.
///
/// Returns `Ok(0)` when snapshot creation fails because this metric is
/// best-effort.
pub fn collect_process_count() -> Result<u32, MetricErr> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Ok(0);
    }

    let mut pe = ProcessEntry32W {
        dwSize: std::mem::size_of::<ProcessEntry32W>() as u32,
        cntUsage: 0,
        th32ProcessID: 0,
        th32DefaultHeapID: 0,
        th32ModuleID: 0,
        cntThreads: 0,
        th32ParentProcessID: 0,
        pcPriClassBase: 0,
        dwFlags: 0,
        szExeFile: [0u16; MAX_PATH],
    };

    let mut count: u32 = 0;

    let first_ret = unsafe { Process32FirstW(snapshot, &mut pe) };
    if first_ret != 0 {
        count += 1;
        while unsafe { Process32NextW(snapshot, &mut pe) } != 0 {
            count += 1;
        }
    }

    unsafe {
        CloseHandle(snapshot);
    }

    Ok(count)
}
