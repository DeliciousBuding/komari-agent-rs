// terminal/windows.rs — Windows ConPTY stub via raw FFI to kernel32.dll.
//
// References:
//   - komari-agent-go/terminal/terminal_windows.go (UserExistsError/conpty model)
//   - docs/plan/spec.md DD11
//   - Microsoft ConPTY API (Windows 10 1809+)
//
// Builds only on `#[cfg(target_family = "windows")]` when `feature = "terminal"`.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use super::{Terminal, TerminalErr};

// ── Windows type aliases ────────────────────────────────────────────────────
//
// All-caps names match the Windows SDK conventions; suppress the Rust
// camel-case lint for this block.

#[allow(non_camel_case_types)]
mod types {
    pub type BOOL = i32;
    pub type HANDLE = isize;                        // matches monitor/process/windows.rs
    pub type HPCON = isize;                         // pseudoconsole handle
    pub type HRESULT = i32;
    pub type LPCWSTR = *const u16;
    pub type LPWSTR = *mut u16;
    pub type DWORD = u32;
    pub type LPVOID = *mut std::ffi::c_void;
    pub type LPPROC_THREAD_ATTRIBUTE_LIST = *mut std::ffi::c_void;
    pub type SIZE_T = usize;
    pub type DWORD_PTR = usize;
    pub type SHORT = i16;
    pub type WORD = u16;
}
use types::*;

// ── Constants ───────────────────────────────────────────────────────────────

const FALSE: BOOL = 0;
const TRUE: BOOL = 1;
const S_OK: HRESULT = 0;
const EXTENDED_STARTUPINFO_PRESENT: DWORD = 0x00080000;
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: DWORD_PTR = 0x00020016;

// ── Windows structs ─────────────────────────────────────────────────────────

#[repr(C)]
struct COORD {
    X: SHORT,
    Y: SHORT,
}

#[repr(C)]
struct SECURITY_ATTRIBUTES {
    nLength: DWORD,
    lpSecurityDescriptor: LPVOID,
    bInheritHandle: BOOL,
}

#[repr(C)]
struct STARTUPINFOW {
    cb: DWORD,
    _lpReserved: LPWSTR,
    _lpDesktop: LPWSTR,
    _lpTitle: LPWSTR,
    _dwX: DWORD,
    _dwY: DWORD,
    _dwXSize: DWORD,
    _dwYSize: DWORD,
    _dwXCountChars: DWORD,
    _dwYCountChars: DWORD,
    _dwFillAttribute: DWORD,
    dwFlags: DWORD,
    wShowWindow: WORD,
    _cbReserved2: WORD,
    _lpReserved2: *mut u8,
    _hStdInput: HANDLE,
    _hStdOutput: HANDLE,
    _hStdError: HANDLE,
}

#[repr(C)]
struct STARTUPINFOEXW {
    startup_info: STARTUPINFOW,
    lp_attribute_list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

#[repr(C)]
struct PROCESS_INFORMATION {
    hProcess: HANDLE,
    hThread: HANDLE,
    _dwProcessId: DWORD,
    _dwThreadId: DWORD,
}

// ── FFI (kernel32.dll) ──────────────────────────────────────────────────────

unsafe extern "system" {
    fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        dwFlags: DWORD,
        phPC: *mut HPCON,
    ) -> HRESULT;

    fn ClosePseudoConsole(hPC: HPCON);

    fn CreateProcessW(
        lpApplicationName: LPCWSTR,
        lpCommandLine: LPWSTR,
        lpProcessAttributes: *mut SECURITY_ATTRIBUTES,
        lpThreadAttributes: *mut SECURITY_ATTRIBUTES,
        bInheritHandles: BOOL,
        dwCreationFlags: DWORD,
        lpEnvironment: LPVOID,
        lpCurrentDirectory: LPCWSTR,
        lpStartupInfo: *mut STARTUPINFOW,
        lpProcessInformation: *mut PROCESS_INFORMATION,
    ) -> BOOL;

    fn CreatePipe(
        hReadPipe: *mut HANDLE,
        hWritePipe: *mut HANDLE,
        lpPipeAttributes: *mut SECURITY_ATTRIBUTES,
        nSize: DWORD,
    ) -> BOOL;

    fn ReadFile(
        hFile: HANDLE,
        lpBuffer: LPVOID,
        nNumberOfBytesToRead: DWORD,
        lpNumberOfBytesRead: *mut DWORD,
        lpOverlapped: LPVOID,
    ) -> BOOL;

    fn WriteFile(
        hFile: HANDLE,
        lpBuffer: LPVOID,
        nNumberOfBytesToWrite: DWORD,
        lpNumberOfBytesWritten: *mut DWORD,
        lpOverlapped: LPVOID,
    ) -> BOOL;

    fn CloseHandle(hObject: HANDLE) -> BOOL;

    fn InitializeProcThreadAttributeList(
        lpAttributeList: LPPROC_THREAD_ATTRIBUTE_LIST,
        dwAttributeCount: DWORD,
        dwFlags: DWORD,
        lpSize: *mut SIZE_T,
    ) -> BOOL;

    fn UpdateProcThreadAttribute(
        lpAttributeList: LPPROC_THREAD_ATTRIBUTE_LIST,
        dwFlags: DWORD,
        attribute: DWORD_PTR,
        lpValue: LPVOID,
        cbSize: SIZE_T,
        lpPreviousValue: LPVOID,
        lpReturnSize: *mut SIZE_T,
    ) -> BOOL;
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Encode a Rust string as a null-terminated wide (UTF-16) string.
fn to_utf16(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

// ── WindowsTerminal ─────────────────────────────────────────────────────────

/// A Windows ConPTY wrapping a child shell process.
///
/// Owns the pseudoconsole handle, I/O pipe handles, and the child process
/// handle.  Created via [`WindowsTerminal::spawn`].
pub struct WindowsTerminal {
    hpc: HPCON,
    /// Our read-side pipe — ConPTY writes output here, we read from it.
    output_read: HANDLE,
    /// Our write-side pipe — we write user input here, ConPTY reads from it.
    input_write: HANDLE,
    /// Handle to the child process (used for lifecycle / wait).
    process: HANDLE,
}

impl WindowsTerminal {
    /// Spawn a new ConPTY session with the given shell command line.
    ///
    /// On success returns a `WindowsTerminal` ready for bidirectional I/O.
    /// Requires Windows 10 1809+ (or equivalent Server 2019+).
    pub fn spawn(shell: &str) -> Result<Self, TerminalErr> {
        // ── 1. Create I/O pipes ─────────────────────────────────────────
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: TRUE,
        };

        let mut input_read: HANDLE = ptr::null_mut();
        let mut input_write: HANDLE = ptr::null_mut();
        let mut output_read: HANDLE = ptr::null_mut();
        let mut output_write: HANDLE = ptr::null_mut();

        let sa_ptr = &sa as *const SECURITY_ATTRIBUTES as *mut SECURITY_ATTRIBUTES;
        unsafe {
            if CreatePipe(&mut input_read, &mut input_write, sa_ptr, 0) == FALSE
                || CreatePipe(&mut output_read, &mut output_write, sa_ptr, 0) == FALSE
            {
                cleanup_pipes(input_read, input_write, output_read, output_write);
                return Err(TerminalErr::PtyOpen("CreatePipe"));
            }
        }

        // ── 2. Create pseudoconsole ─────────────────────────────────────
        let size = COORD { X: 80, Y: 24 };
        let mut hpc: HPCON = ptr::null_mut();
        let hr = unsafe {
            CreatePseudoConsole(size, input_read, output_write, 0, &mut hpc)
        };
        if hr != S_OK || hpc.is_null() {
            unsafe {
                cleanup_pipes(input_read, input_write, output_read, output_write);
            }
            return Err(TerminalErr::PtyOpen("CreatePseudoConsole"));
        }

        // ── 3. Build STARTUPINFOEX with pseudoconsole attribute ─────────
        let mut size_attr: SIZE_T = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size_attr);
        }

        let mut attr_buf: Vec<u8> = vec![0u8; size_attr];
        let attr_list = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        unsafe {
            if InitializeProcThreadAttributeList(attr_list, 1, 0, &mut size_attr) == FALSE {
                cleanup(hpc, input_read, input_write, output_read, output_write,
                        ptr::null_mut(), ptr::null_mut());
                return Err(TerminalErr::PtyOpen("InitializeProcThreadAttributeList"));
            }
            if UpdateProcThreadAttribute(
                attr_list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                hpc, std::mem::size_of::<HPCON>(),
                ptr::null_mut(), ptr::null_mut(),
            ) == FALSE
            {
                cleanup(hpc, input_read, input_write, output_read, output_write,
                        ptr::null_mut(), ptr::null_mut());
                return Err(TerminalErr::PtyOpen("UpdateProcThreadAttribute"));
            }
        }

        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.startup_info.cb = std::mem::size_of::<STARTUPINFOEXW>() as DWORD;
        startup.lp_attribute_list = attr_list;

        // ── 4. Create the child process ─────────────────────────────────
        let mut cmd_wide = to_utf16(shell);
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let ok = unsafe {
            CreateProcessW(
                ptr::null(),                                     // lpApplicationName
                cmd_wide.as_mut_ptr(),                           // lpCommandLine
                ptr::null_mut(),                                 // lpProcessAttributes
                ptr::null_mut(),                                 // lpThreadAttributes
                FALSE,                                           // bInheritHandles
                EXTENDED_STARTUPINFO_PRESENT,                    // dwCreationFlags
                ptr::null_mut(),                                 // lpEnvironment
                ptr::null(),                                     // lpCurrentDirectory
                &mut startup.startup_info,                       // lpStartupInfo
                &mut pi,                                         // lpProcessInformation
            )
        };

        // attr_buf drops here — attribute list memory is freed.
        // (We don't call DeleteProcThreadAttributeList to avoid double-free.)

        if ok == FALSE {
            unsafe {
                cleanup(hpc, input_read, input_write, output_read, output_write,
                        ptr::null_mut(), ptr::null_mut());
            }
            return Err(TerminalErr::Exec("CreateProcessW"));
        }

        // ── 5. Close handles we no longer need ──────────────────────────
        unsafe {
            CloseHandle(input_read);   // ConPTY owns the read side of the input pipe
            CloseHandle(output_write); // ConPTY owns the write side of the output pipe
            CloseHandle(pi.hThread);   // Don't need the thread handle
        }

        Ok(WindowsTerminal {
            hpc,
            output_read,
            input_write,
            process: pi.hProcess,
        })
    }
}

impl Terminal for WindowsTerminal {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalErr> {
        if self.output_read.is_null() {
            return Err(TerminalErr::Io(io::Error::new(
                io::ErrorKind::NotConnected,
                "ConPTY pipe already closed",
            )));
        }
        let mut nread: DWORD = 0;
        let ok = unsafe {
            ReadFile(
                self.output_read,
                buf.as_mut_ptr() as LPVOID,
                buf.len() as DWORD,
                &mut nread,
                ptr::null_mut(),
            )
        };
        if ok == FALSE {
            Err(TerminalErr::Io(io::Error::last_os_error()))
        } else {
            Ok(nread as usize)
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TerminalErr> {
        if self.input_write.is_null() {
            return Err(TerminalErr::Io(io::Error::new(
                io::ErrorKind::NotConnected,
                "ConPTY pipe already closed",
            )));
        }
        let mut nwritten: DWORD = 0;
        let ok = unsafe {
            WriteFile(
                self.input_write,
                data.as_ptr() as LPVOID,
                data.len() as DWORD,
                &mut nwritten,
                ptr::null_mut(),
            )
        };
        if ok == FALSE {
            Err(TerminalErr::Io(io::Error::last_os_error()))
        } else {
            Ok(nwritten as usize)
        }
    }

    fn resize(&mut self, _cols: u16, _rows: u16) -> Result<(), TerminalErr> {
        // TODO: ResizePseudoConsole (Windows 10 1903+).
        // Signature: HRESULT ResizePseudoConsole(HPCON hPC, COORD size);
        // Not yet wired — add to the extern "system" block and call here.
        Err(TerminalErr::Resize(
            "ConPTY resize not yet implemented (requires Win10 1903+ ResizePseudoConsole)",
        ))
    }

    fn close(&mut self) -> Result<(), TerminalErr> {
        unsafe {
            if !self.hpc.is_null() {
                ClosePseudoConsole(self.hpc);
                self.hpc = ptr::null_mut();
            }
            if !self.output_read.is_null() {
                CloseHandle(self.output_read);
                self.output_read = ptr::null_mut();
            }
            if !self.input_write.is_null() {
                CloseHandle(self.input_write);
                self.input_write = ptr::null_mut();
            }
            if !self.process.is_null() {
                CloseHandle(self.process);
                self.process = ptr::null_mut();
            }
        }
        Ok(())
    }
}

impl Drop for WindowsTerminal {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

// ── Internal cleanup helpers ────────────────────────────────────────────────

unsafe fn cleanup_pipes(
    input_read: HANDLE, input_write: HANDLE,
    output_read: HANDLE, output_write: HANDLE,
) {
    for h in [input_read, input_write, output_read, output_write] {
        if !h.is_null() {
            // SAFETY: h is a valid, non-null pipe handle from a prior CreatePipe call.
            unsafe { CloseHandle(h) };
        }
    }
}

unsafe fn cleanup(
    hpc: HPCON,
    input_read: HANDLE, input_write: HANDLE,
    output_read: HANDLE, output_write: HANDLE,
    process: HANDLE, thread: HANDLE,
) {
    if !hpc.is_null() {
        // SAFETY: hpc was created by CreatePseudoConsole and not yet closed.
        unsafe { ClosePseudoConsole(hpc) };
    }
    if !input_read.is_null()  { unsafe { CloseHandle(input_read) }; }
    if !input_write.is_null() { unsafe { CloseHandle(input_write) }; }
    if !output_read.is_null() { unsafe { CloseHandle(output_read) }; }
    if !output_write.is_null(){ unsafe { CloseHandle(output_write) }; }
    if !process.is_null()     { unsafe { CloseHandle(process) }; }
    if !thread.is_null()      { unsafe { CloseHandle(thread) }; }
}
