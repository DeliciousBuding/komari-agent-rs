//! Bounded synchronous subprocess execution. No pipe-reader threads are needed.
//!
//! A command owns its process group (Unix) or job (Windows), including ordinary
//! descendants. They are stopped when the root exits, times out, or fills the
//! capture budget. This is lifecycle management, not a sandbox: a Unix command
//! deliberately calling setsid/setpgid can escape its original process group.

use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Combined stdout/stderr budget; collectors must never parse truncated output.
pub const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum StopReason {
    Exited,
    TimedOut,
    OutputLimit,
}

#[derive(Debug)]
pub struct RunOutput {
    pub output: Output,
    pub reason: StopReason,
}

struct ManagedChild {
    child: Child,
    scope: platform::Scope,
    stopped: bool,
}

impl ManagedChild {
    fn stop(&mut self) -> io::Result<()> {
        if !self.stopped {
            self.scope.terminate()?;
            self.stopped = true;
        }
        Ok(())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // Cover setup and I/O errors as well as the normal return path. Reap
        // the direct child; an ordinary Child drop alone does not do that.
        let _ = self.stop();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Capture one command within a shared input/execution/output deadline.
/// No stdin is inherited from the agent. Partial output is returned on timeout
/// or overflow, along with an explicit reason. Spawn/I/O failures are errors.
pub fn run(cmd: &mut Command, input: Option<&[u8]>, timeout: Duration) -> io::Result<RunOutput> {
    let deadline = Instant::now() + timeout;
    platform::prepare_command(cmd, input)?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let spawned = platform::spawn(cmd);
    cmd.stdin(Stdio::null()); // release a Windows file-backed stdin's parent handle
    let mut process = spawned?;
    let mut stdout = process
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout pipe"))?;
    let mut stderr = process
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr pipe"))?;
    platform::nonblocking(&stdout)?;
    platform::nonblocking(&stderr)?;
    #[cfg(unix)]
    let mut stdin = process.child.stdin.take();
    #[cfg(unix)]
    if let Some(pipe) = &stdin {
        platform::nonblocking(pipe)?;
    }
    #[cfg(unix)]
    let mut input_offset = 0;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut reason;
    loop {
        if Instant::now() >= deadline {
            reason = StopReason::TimedOut;
            break;
        }
        // One bounded read per stream per turn: neither a busy stdout nor a
        // busy stderr can starve the other stream or the deadline check.
        let nout = capture(&mut stdout, &mut out, err.len())?;
        let nerr = capture(&mut stderr, &mut err, out.len())?;
        if out.len() + err.len() > OUTPUT_LIMIT {
            reason = StopReason::OutputLimit;
            break;
        }
        #[cfg(unix)]
        if let Some(pipe) = stdin.as_mut() {
            use std::io::Write;
            let bytes = input.unwrap_or_default();
            let end = bytes.len().min(input_offset + 8192);
            match pipe.write(&bytes[input_offset..end]) {
                Ok(n) => input_offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                // Commands may intentionally stop consuming stdin early.
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => input_offset = bytes.len(),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
            if input_offset == bytes.len() {
                stdin.take(); // EOF without waiting for the command to exit.
            }
        }
        if process.child.try_wait()?.is_some() {
            reason = StopReason::Exited;
            break;
        }
        if nout + nerr == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[cfg(unix)]
    drop(stdin);
    // Stop descendants even after an ordinary root exit: an inherited output
    // handle must not keep the agent waiting for EOF indefinitely.
    process.stop()?;
    let status = process.child.wait()?;
    // Read only already-available bytes, never wait for an inherited pipe's
    // EOF. The extra byte is a sentinel distinguishing an exact fit from loss.
    loop {
        let nout = capture(&mut stdout, &mut out, err.len())?;
        let nerr = capture(&mut stderr, &mut err, out.len())?;
        if out.len() + err.len() > OUTPUT_LIMIT {
            reason = StopReason::OutputLimit;
            break;
        }
        if nout + nerr == 0 {
            break;
        }
    }
    if out.len() + err.len() > OUTPUT_LIMIT {
        // At most one stream contains the sentinel; keep the shared cap exact.
        err.truncate(OUTPUT_LIMIT.saturating_sub(out.len()));
        out.truncate(OUTPUT_LIMIT);
    }
    Ok(RunOutput {
        output: Output {
            status,
            stdout: out,
            stderr: err,
        },
        reason,
    })
}

fn capture<T: Read + platform::PipeHandle>(
    pipe: &mut T,
    bytes: &mut Vec<u8>,
    other_len: usize,
) -> io::Result<usize> {
    let remaining = (OUTPUT_LIMIT + 1).saturating_sub(bytes.len() + other_len);
    if remaining == 0 {
        return Ok(0);
    }
    let mut buf = [0; 8192];
    let len = remaining.min(buf.len());
    let n = platform::read_available(pipe, &mut buf[..len])?;
    bytes.extend_from_slice(&buf[..n]);
    Ok(n)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    pub use std::os::fd::AsRawFd as PipeHandle;

    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
        fn kill(pid: i32, signal: i32) -> i32;
    }

    pub struct Scope(i32);

    impl Scope {
        pub fn terminate(&self) -> io::Result<()> {
            // SIGKILL is 9 on every supported Unix. Negative PID addresses the
            // group, not an unrelated positive PID; ESRCH means it already left.
            if unsafe { kill(-self.0, 9) } == -1 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(3) {
                    return Err(e);
                }
            }
            Ok(())
        }
    }

    pub fn prepare_command(cmd: &mut Command, input: Option<&[u8]>) -> io::Result<()> {
        cmd.process_group(0).stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        Ok(())
    }

    pub fn spawn(cmd: &mut Command) -> io::Result<ManagedChild> {
        let child = cmd.spawn()?;
        let scope = Scope(child.id() as i32);
        Ok(ManagedChild {
            child,
            scope,
            stopped: false,
        })
    }

    pub fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
        // F_GETFL/F_SETFL are shared by Linux/Darwin/FreeBSD; O_NONBLOCK is not.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const NONBLOCK: i32 = 0x800;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        const NONBLOCK: i32 = 4;
        let fd = pipe.as_raw_fd();
        let flags = unsafe { fcntl(fd, 3) };
        if flags == -1 || unsafe { fcntl(fd, 4, flags | NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn read_available<T: Read + AsRawFd>(pipe: &mut T, buf: &mut [u8]) -> io::Result<usize> {
        match pipe.read(buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            result => result,
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::fs::OpenOptions;
    use std::io::{Seek, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub use std::os::windows::io::AsRawHandle as PipeHandle;
    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ThreadEntry {
        size: u32,
        usage: u32,
        thread_id: u32,
        process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, data: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
        fn PeekNamedPipe(
            pipe: Handle,
            buffer: *mut c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }

    fn owned(handle: Handle) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == -1isize as Handle {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: successful Win32 constructors return an owned HANDLE.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    pub struct Scope(OwnedHandle);

    impl Scope {
        pub fn terminate(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    pub fn prepare_command(cmd: &mut Command, input: Option<&[u8]>) -> io::Result<()> {
        // A synchronous anonymous-pipe write cannot be given a deadline on
        // Windows. File-backed stdin avoids a blocking writer thread entirely.
        // The exclusively opened file disappears after its last inherited
        // handle closes; create_new prevents following a planted existing file.
        if let Some(bytes) = input {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let mut file = loop {
                let n = SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("komari-stdin-{}-{n}", std::process::id()));
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .share_mode(0)
                    .custom_flags(0x0400_0100)
                    .open(path)
                {
                    Ok(file) => break file,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                }
            };
            // FILE_FLAG_DELETE_ON_CLOSE | FILE_ATTRIBUTE_TEMPORARY above.
            file.write_all(bytes)?;
            file.rewind()?;
            cmd.stdin(Stdio::from(file));
        } else {
            cmd.stdin(Stdio::null());
        }
        // CREATE_NO_WINDOW | CREATE_SUSPENDED: no code can spawn a descendant
        // before the child belongs to our job. Fail closed on job setup errors.
        cmd.creation_flags(0x0800_0004);
        Ok(())
    }

    pub fn spawn(cmd: &mut Command) -> io::Result<ManagedChild> {
        let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
        let limits = ExtendedLimits {
            basic: BasicLimits {
                flags: 0x2000,
                ..BasicLimits::default()
            }, // KILL_ON_JOB_CLOSE
            ..ExtendedLimits::default()
        };
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                9,
                (&limits as *const ExtendedLimits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let child = cmd.spawn()?;
        let process = ManagedChild {
            child,
            scope: Scope(job),
            stopped: false,
        };
        if unsafe {
            AssignProcessToJobObject(
                process.scope.0.as_raw_handle(),
                process.child.as_raw_handle(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error()); // guard kills even an unassigned suspended child
        }
        // Command does not expose its primary thread HANDLE. Enumerate while
        // it is suspended, before its loader/user code can create other threads.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(4, 0) } as Handle)?; // SNAPTHREAD
        let mut entry = ThreadEntry {
            size: std::mem::size_of::<ThreadEntry>() as u32,
            ..ThreadEntry::default()
        };
        let mut present = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
        while present {
            if entry.process_id == process.child.id() {
                let thread = owned(unsafe { OpenThread(2, 0, entry.thread_id) })?; // SUSPEND_RESUME
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(process);
            }
            present = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
        }
        Err(io::Error::other("suspended child has no primary thread"))
    }

    pub fn nonblocking(_pipe: &impl AsRawHandle) -> io::Result<()> {
        Ok(()) // PeekNamedPipe below avoids a blocking read on anonymous pipes.
    }

    pub fn read_available<T: Read + AsRawHandle>(
        pipe: &mut T,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(109) {
                // ERROR_BROKEN_PIPE is EOF.
                return Ok(0);
            }
            return Err(e);
        }
        if available == 0 {
            return Ok(0);
        }
        let len = buf.len().min(available as usize);
        // This is the only reader, so the bytes just peeked remain available.
        pipe.read(&mut buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(mode: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--ignored",
            "--exact",
            "subprocess::tests::child_fixture",
            "--nocapture",
        ])
        .env("KOMARI_PROCESS_FIXTURE", mode);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd
    }

    #[test]
    #[ignore = "subprocess fixture, invoked only by the runner tests"]
    #[allow(clippy::zombie_processes)] // deliberately orphan a child to test scope cleanup
    fn child_fixture() {
        let mode = std::env::var("KOMARI_PROCESS_FIXTURE").unwrap();
        println!("ROOT_PID={}", std::process::id());
        match mode.as_str() {
            "unicode" => {
                std::io::stdout()
                    .write_all("héllo 世界\n".as_bytes())
                    .unwrap();
                std::io::stderr().write_all("错误\n".as_bytes()).unwrap();
                std::process::exit(7);
            }
            "sleep" => std::thread::sleep(Duration::from_secs(20)),
            "background" | "tree-sleep" => {
                let child = fixture("sleep").spawn().unwrap();
                println!("CHILD_PID={}", child.id());
                if mode == "tree-sleep" {
                    std::thread::sleep(Duration::from_secs(20));
                }
            }
            "streams" | "stdin" => {
                for _ in 0..12 {
                    std::io::stdout().write_all(&[0xff; 8192]).unwrap();
                    std::io::stderr().write_all(&[b'e'; 8192]).unwrap();
                }
                if mode == "stdin" {
                    let mut input = Vec::new();
                    std::io::stdin().read_to_end(&mut input).unwrap();
                    println!("INPUT_LEN={}", input.len());
                }
            }
            "overflow" => loop {
                std::io::stdout().write_all(&[b'x'; 8192]).unwrap();
                std::io::stderr().write_all(&[b'y'; 8192]).unwrap();
            },
            _ => panic!("unknown fixture"),
        }
    }

    fn pid(output: &[u8], key: &str) -> u32 {
        String::from_utf8_lossy(output)
            .lines()
            .find_map(|s| s.strip_prefix(key))
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn exit_code_unicode_and_direct_child_reaped() {
        let result = run(&mut fixture("unicode"), None, Duration::from_secs(5)).unwrap();
        assert_eq!(result.reason, StopReason::Exited);
        assert_eq!(result.output.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&result.output.stdout).contains("héllo 世界"));
        assert!(String::from_utf8_lossy(&result.output.stderr).contains("错误"));
        assert_stopped(pid(&result.output.stdout, "ROOT_PID="));
    }

    #[test]
    fn timeout_kills_and_reaps() {
        let start = Instant::now();
        let result = run(&mut fixture("tree-sleep"), None, Duration::from_millis(500)).unwrap();
        assert_eq!(result.reason, StopReason::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_stopped(pid(&result.output.stdout, "ROOT_PID="));
        assert_stopped(pid(&result.output.stdout, "CHILD_PID="));
    }

    #[test]
    fn root_exit_stops_inherited_pipe_descendant() {
        let start = Instant::now();
        let result = run(&mut fixture("background"), None, Duration::from_secs(5)).unwrap();
        assert_eq!(result.reason, StopReason::Exited);
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_stopped(pid(&result.output.stdout, "ROOT_PID="));
        assert_stopped(pid(&result.output.stdout, "CHILD_PID="));
    }

    #[test]
    fn large_both_streams_are_drained_without_deadlock() {
        let result = run(&mut fixture("streams"), None, Duration::from_secs(5)).unwrap();
        assert_eq!(result.reason, StopReason::Exited);
        assert_eq!(
            result.output.stdout.iter().filter(|&&b| b == 0xff).count(),
            12 * 8192
        );
        assert_eq!(result.output.stderr, vec![b'e'; 12 * 8192]);
    }

    #[test]
    fn large_stdin_and_early_output_cannot_deadlock() {
        let input = vec![b'i'; 512 * 1024];
        let result = run(&mut fixture("stdin"), Some(&input), Duration::from_secs(5)).unwrap();
        assert_eq!(result.reason, StopReason::Exited);
        assert!(String::from_utf8_lossy(&result.output.stdout).contains("INPUT_LEN=524288"));
    }

    #[test]
    fn output_budget_is_shared_observable_and_reaps() {
        let result = run(&mut fixture("overflow"), None, Duration::from_secs(5)).unwrap();
        assert_eq!(result.reason, StopReason::OutputLimit);
        assert_eq!(
            result.output.stdout.len() + result.output.stderr.len(),
            OUTPUT_LIMIT
        );
        assert_stopped(pid(&result.output.stdout, "ROOT_PID="));
    }

    #[test]
    fn spawn_error_is_preserved() {
        let missing = std::env::temp_dir().join("komari-missing-subprocess-executable");
        assert_eq!(
            run(&mut Command::new(missing), None, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    fn assert_stopped(pid: u32) {
        // Termination of a Windows job is asynchronous; Unix orphaned children
        // may briefly remain as zombies until the system reaper collects them.
        for _ in 0..100 {
            if !is_running(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("subprocess {pid} is still running");
    }

    #[cfg(unix)]
    fn is_running(pid: u32) -> bool {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && stat
                .rsplit_once(") ")
                .is_some_and(|(_, fields)| fields.starts_with('Z'))
        {
            return false;
        }
        unsafe { kill(pid as i32, 0) == 0 }
    }

    #[cfg(windows)]
    fn is_running(pid: u32) -> bool {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        }
        let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) }; // SYNCHRONIZE
        if handle.is_null() {
            return false;
        }
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        let _owned = unsafe { OwnedHandle::from_raw_handle(handle) };
        unsafe { WaitForSingleObject(handle, 0) == 258 }
    }
}
