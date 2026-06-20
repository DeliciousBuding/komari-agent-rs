// terminal/unix.rs — Unix PTY stub via raw POSIX FFI (posix_openpt / fork / execvp).
//
// References:
//   - komari-agent-go/terminal/terminal_unix.go (creack/pty model)
//   - docs/plan/spec.md DD11
//
// Builds only on `#[cfg(target_family = "unix")]` when `feature = "terminal"`.

use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int, c_ulong};
use std::os::unix::io::RawFd;

use super::{Terminal, TerminalErr};

// ── Constants ───────────────────────────────────────────────────────────────

/// `TIOCSWINSZ` ioctl request code (portable across Linux arches and FreeBSD).
const TIOCSWINSZ: c_ulong = 0x5414;
/// `WNOHANG` for waitpid — return immediately if child has not exited.
const WNOHANG: c_int = 1;

const O_RDWR: c_int = 0o2;
#[cfg(target_os = "linux")]
const O_NOCTTY: c_int = 0o100;
#[cfg(not(target_os = "linux"))]
const O_NOCTTY: c_int = 0x20000; // macOS / FreeBSD

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

// ── FFI ─────────────────────────────────────────────────────────────────────
//
// We use #[link_name] for POSIX functions whose names would collide with
// the Terminal trait methods (read / write / close / open).

unsafe extern "C" {
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    /// Returns a pointer to a static buffer (not thread-safe — we are ST).
    fn ptsname(fd: c_int) -> *mut c_char;
    fn fork() -> c_int;
    fn setsid() -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, arg: *mut Winsize) -> c_int;
    fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn _exit(status: c_int) -> !;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;

    #[link_name = "read"]
    fn sys_read(fd: c_int, buf: *mut u8, count: usize) -> isize;
    #[link_name = "write"]
    fn sys_write(fd: c_int, buf: *const u8, count: usize) -> isize;
    #[link_name = "close"]
    fn sys_close(fd: c_int) -> c_int;
    #[link_name = "open"]
    fn sys_open(pathname: *const c_char, flags: c_int) -> c_int;
}

// ── UnixTerminal ────────────────────────────────────────────────────────────

/// A Unix pseudo-terminal wrapping a child shell process.
///
/// Created via [`UnixTerminal::spawn`], which forks a child, opens the PTY
/// slave, and execs the requested shell.  I/O is routed through the PTY
/// master file descriptor.
pub struct UnixTerminal {
    /// PTY master file descriptor (-1 after close).
    pty_fd: RawFd,
    /// PID of the child shell process (0 after reap).
    child_pid: c_int,
}

impl UnixTerminal {
    /// Spawn a new Unix PTY with the given shell.
    ///
    /// On success returns a fully-initialised `UnixTerminal` ready for
    /// bidirectional I/O.  The child process is already running.
    pub fn spawn(shell: &str) -> Result<Self, TerminalErr> {
        // ── 1. Open PTY master ──────────────────────────────────────────
        let pty_fd = unsafe { posix_openpt(O_RDWR | O_NOCTTY) };
        if pty_fd < 0 {
            return Err(TerminalErr::PtyOpen("posix_openpt"));
        }

        // ── 2. Grant / unlock the slave ─────────────────────────────────
        if unsafe { grantpt(pty_fd) } < 0 {
            unsafe { sys_close(pty_fd) };
            return Err(TerminalErr::PtyOpen("grantpt"));
        }
        if unsafe { unlockpt(pty_fd) } < 0 {
            unsafe { sys_close(pty_fd) };
            return Err(TerminalErr::PtyOpen("unlockpt"));
        }

        // ── 3. Resolve slave device name ────────────────────────────────
        let slave_ptr = unsafe { ptsname(pty_fd) };
        if slave_ptr.is_null() {
            unsafe { sys_close(pty_fd) };
            return Err(TerminalErr::PtyOpen("ptsname returned null"));
        }
        let slave_name = unsafe { CStr::from_ptr(slave_ptr) };

        // ── 4. Set initial window size (80×24) ──────────────────────────
        let ws = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            ioctl(pty_fd, TIOCSWINSZ, &ws as *const Winsize as *mut Winsize);
        }

        // ── 5. Fork ─────────────────────────────────────────────────────
        let shell_c =
            CString::new(shell).map_err(|_| TerminalErr::Exec("shell path contains NUL byte"))?;
        let shell_ptr = shell_c.as_ptr();
        let argv: [*const c_char; 2] = [shell_ptr, std::ptr::null()];

        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe { sys_close(pty_fd) };
            return Err(TerminalErr::Fork("fork() returned -1"));
        }

        if pid == 0 {
            // ── CHILD ───────────────────────────────────────────────────
            unsafe {
                setsid();
                let slave_fd = sys_open(slave_name.as_ptr(), O_RDWR);
                if slave_fd >= 0 {
                    dup2(slave_fd, 0); // stdin
                    dup2(slave_fd, 1); // stdout
                    dup2(slave_fd, 2); // stderr
                    if slave_fd > 2 {
                        sys_close(slave_fd);
                    }
                }
                sys_close(pty_fd); // child doesn't need the master
                execvp(shell_ptr, argv.as_ptr());
                _exit(127);
            }
        }

        // ── PARENT ──────────────────────────────────────────────────────
        Ok(UnixTerminal {
            pty_fd,
            child_pid: pid,
        })
    }
}

impl Terminal for UnixTerminal {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalErr> {
        if self.pty_fd < 0 {
            return Err(TerminalErr::Io(io::Error::new(
                io::ErrorKind::NotConnected,
                "PTY already closed",
            )));
        }
        let n = unsafe { sys_read(self.pty_fd, buf.as_mut_ptr(), buf.len()) };
        if n < 0 {
            Err(TerminalErr::Io(io::Error::last_os_error()))
        } else {
            Ok(n as usize)
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TerminalErr> {
        if self.pty_fd < 0 {
            return Err(TerminalErr::Io(io::Error::new(
                io::ErrorKind::NotConnected,
                "PTY already closed",
            )));
        }
        let n = unsafe { sys_write(self.pty_fd, data.as_ptr(), data.len()) };
        if n < 0 {
            Err(TerminalErr::Io(io::Error::last_os_error()))
        } else {
            Ok(n as usize)
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Result<(), TerminalErr> {
        if self.pty_fd < 0 {
            return Err(TerminalErr::Resize("PTY already closed"));
        }
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe {
            ioctl(
                self.pty_fd,
                TIOCSWINSZ,
                &ws as *const Winsize as *mut Winsize,
            )
        };
        if ret < 0 {
            Err(TerminalErr::Resize("ioctl TIOCSWINSZ failed"))
        } else {
            Ok(())
        }
    }

    fn close(&mut self) -> Result<(), TerminalErr> {
        if self.child_pid == 0 {
            return Ok(()); // already reaped
        }
        unsafe {
            sys_close(self.pty_fd);
            self.pty_fd = -1;
            let mut status: c_int = 0;
            waitpid(self.child_pid, &mut status, WNOHANG);
        }
        self.child_pid = 0;
        Ok(())
    }
}

impl Drop for UnixTerminal {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
