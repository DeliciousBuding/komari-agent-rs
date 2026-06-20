// komari-agent-rs: Windows GPU detection via raw DXGI COM FFI.
// No windows-rs / windows crate — bare LoadLibraryW + GetProcAddress + vtable calls.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/gpu_windows.go

use crate::arena::{SmallVec, MAX_GPUS};
use super::{GpuBackend, GpuDetectErr, GpuInfo};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

// ── COM types (C ABI compatible) ───────────────────────────────────────────

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct Luid {
    low_part: u32,
    high_part: i32,
}

/// DXGI_ADAPTER_DESC1 — matches the C++ struct layout exactly.
#[repr(C)]
struct DxgiAdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    sub_sys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    adapter_luid: Luid,
    flags: u32,
}

impl DxgiAdapterDesc1 {
    /// Convert the wide-char description field to a Rust String (UTF-8).
    fn description_string(&self) -> String {
        let end = self.description.iter().position(|&c| c == 0).unwrap_or(128);
        String::from_utf16_lossy(&self.description[..end])
    }
}

// ── COM interface vtables ──────────────────────────────────────────────────
//
// IDXGIFactory1 inherits: IUnknown → IDXGIObject → IDXGIFactory → IDXGIFactory1
// Slot 0: QueryInterface     (IUnknown)
// Slot 1: AddRef             (IUnknown)
// Slot 2: Release            (IUnknown)
// Slot 3: SetPrivateData          (IDXGIObject)
// Slot 4: SetPrivateDataInterface (IDXGIObject)
// Slot 5: GetPrivateData          (IDXGIObject)
// Slot 6: GetParent               (IDXGIObject)
// Slot 7: EnumAdapters            (IDXGIFactory)
// Slot 8: MakeWindowAssociation   (IDXGIFactory)
// Slot 9: GetWindowAssociation    (IDXGIFactory)
// Slot 10: CreateSwapChain         (IDXGIFactory)
// Slot 11: CreateSoftwareAdapter   (IDXGIFactory)
// Slot 12: EnumAdapters1           (IDXGIFactory1)
// Slot 13: IsCurrent               (IDXGIFactory1)

#[repr(C)]
struct IDXGIFactory1Vtbl {
    query_interface: usize,
    add_ref: unsafe extern "system" fn(*mut IDXGIFactory1) -> u32,
    release: unsafe extern "system" fn(*mut IDXGIFactory1) -> u32,
    _pad_obj: [usize; 4],                // slots 3-6: IDXGIObject
    _pad_factory: [usize; 5],            // slots 7-11: IDXGIFactory
    enum_adapters1: unsafe extern "system" fn(*mut IDXGIFactory1, u32, *mut *mut IDXGIAdapter1) -> i32,
    _pad_is_current: usize,              // slot 13: IsCurrent
}

#[repr(C)]
struct IDXGIFactory1 {
    lp_vtbl: *const IDXGIFactory1Vtbl,
}

/// IDXGIAdapter1 inherits: IUnknown → IDXGIObject → IDXGIAdapter → IDXGIAdapter1
/// Slot 0: QueryInterface             (IUnknown)
/// Slot 1: AddRef                     (IUnknown)
/// Slot 2: Release                    (IUnknown)
/// Slot 3: SetPrivateData             (IDXGIObject)
/// Slot 4: SetPrivateDataInterface    (IDXGIObject)
/// Slot 5: GetPrivateData             (IDXGIObject)
/// Slot 6: GetParent                  (IDXGIObject)
/// Slot 7: EnumOutputs                (IDXGIAdapter)
/// Slot 8: GetDesc                    (IDXGIAdapter)
/// Slot 9: CheckInterfaceSupport      (IDXGIAdapter)
/// Slot 10: GetDesc1                  (IDXGIAdapter1)

#[repr(C)]
struct IDXGIAdapter1Vtbl {
    query_interface: usize,
    add_ref: unsafe extern "system" fn(*mut IDXGIAdapter1) -> u32,
    release: unsafe extern "system" fn(*mut IDXGIAdapter1) -> u32,
    _pad_obj: [usize; 4],                // slots 3-6: IDXGIObject
    _pad_adapter: [usize; 3],            // slots 7-9: IDXGIAdapter
    get_desc1: unsafe extern "system" fn(*mut IDXGIAdapter1, *mut DxgiAdapterDesc1) -> i32,
}

#[repr(C)]
struct IDXGIAdapter1 {
    lp_vtbl: *const IDXGIAdapter1Vtbl,
}

// ── HRESULT constants ─────────────────────────────────────────────────────

const S_OK: i32 = 0;
// DXGI_ERROR_NOT_FOUND = 0x887A0002 — cast as i32 for HRESULT comparison
const DXGI_ERROR_NOT_FOUND: u32 = 0x887A0002;

// ── IID_IDXGIFactory1 ─────────────────────────────────────────────────────
// {770AAE78-F26F-4DBA-A829-253C83D1B387}

const IID_IDXGIFACTORY1: Guid = Guid {
    data1: 0x770AAE78,
    data2: 0xF26F,
    data3: 0x4DBA,
    data4: [0xA8, 0x29, 0x25, 0x3C, 0x83, 0xD1, 0xB3, 0x87],
};

// ── Win32 FFI declarations ─────────────────────────────────────────────────

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> isize;
    fn FreeLibrary(h_lib_module: isize) -> i32;
    fn GetProcAddress(h_module: isize, lp_proc_name: *const u8) -> *const std::ffi::c_void;
}

type CreateDXGIFactory1Fn = unsafe extern "system" fn(*const Guid, *mut *mut IDXGIFactory1) -> i32;

/// Convert a Rust string slice to a null-terminated wide string (UTF-16LE).
fn to_wide_null(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = OsStr::new(s).encode_wide().collect();
    v.push(0);
    v
}

// ── Entry point ────────────────────────────────────────────────────────────

/// Detect GPUs via DXGI COM.  This is the sole entry point on Windows.
pub fn detect() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr> {
    let gpus = detect_dxgi()?;
    Ok((GpuBackend::Dxgi, gpus))
}

fn detect_dxgi() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    // Load dxgi.dll
    let dll_name = to_wide_null("dxgi.dll");
    let h_module = unsafe { LoadLibraryW(dll_name.as_ptr()) };
    if h_module == 0 {
        return Err(GpuDetectErr::Subprocess("LoadLibraryW dxgi.dll failed".into()));
    }

    // Get CreateDXGIFactory1
    let proc_name = b"CreateDXGIFactory1\0";
    let proc_addr = unsafe { GetProcAddress(h_module, proc_name.as_ptr()) };
    if proc_addr.is_null() {
        unsafe { FreeLibrary(h_module); }
        return Err(GpuDetectErr::Subprocess("GetProcAddress CreateDXGIFactory1 failed".into()));
    }

    let create_factory: CreateDXGIFactory1Fn = unsafe { std::mem::transmute(proc_addr) };

    // Create factory
    let mut factory: *mut IDXGIFactory1 = std::ptr::null_mut();
    let hr = unsafe { create_factory(&IID_IDXGIFACTORY1, &mut factory) };
    if hr != S_OK || factory.is_null() {
        unsafe { FreeLibrary(h_module); }
        return Err(GpuDetectErr::Subprocess(format!("CreateDXGIFactory1 HRESULT: 0x{:08X}", hr as u32)));
    }

    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    // Enumerate adapters
    for idx in 0u32.. {
        let mut adapter: *mut IDXGIAdapter1 = std::ptr::null_mut();
        let vtbl = unsafe { &*((*factory).lp_vtbl) };
        let hr = unsafe { (vtbl.enum_adapters1)(factory, idx, &mut adapter) };

        if (hr as u32) == DXGI_ERROR_NOT_FOUND {
            break;
        }

        if hr != S_OK {
            if !adapter.is_null() {
                let avtbl = unsafe { &*((*adapter).lp_vtbl) };
                unsafe { (avtbl.release)(adapter); }
            }
            continue;
        }

        if adapter.is_null() {
            continue;
        }

        // GetDesc1
        let mut desc: DxgiAdapterDesc1 = unsafe { std::mem::zeroed() };
        let adapter_vtbl = unsafe { &*((*adapter).lp_vtbl) };
        let desc_hr = unsafe { (adapter_vtbl.get_desc1)(adapter, &mut desc) };

        // Release adapter
        unsafe { (adapter_vtbl.release)(adapter); }

        if desc_hr != S_OK {
            continue;
        }

        // Skip software adapters (DXGI_ADAPTER_FLAG_SOFTWARE = 1 << 1)
        if (desc.flags & (1 << 1)) != 0 {
            continue;
        }

        let name = desc.description_string();
        if name.is_empty() {
            continue;
        }

        let _ = gpus.push(GpuInfo {
            name,
            memory_total: desc.dedicated_video_memory as u64,
            memory_used: 0,    // DXGI doesn't report per-adapter usage
            utilization: 0.0,  // DXGI doesn't report utilization
            temperature: 0,    // DXGI doesn't report temperature
        });
    }

    // Release factory
    unsafe {
        let vtbl = &*((*factory).lp_vtbl);
        (vtbl.release)(factory);
    }
    unsafe { FreeLibrary(h_module); }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("DXGI: no hardware adapters found".into()))
    } else {
        Ok(gpus)
    }
}
