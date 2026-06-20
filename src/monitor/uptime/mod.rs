// komari-agent-rs: monitor::uptime — system uptime via /proc/uptime or GetTickCount64.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "freebsd")]
pub mod freebsd;

#[cfg(target_os = "linux")]
pub use linux::collect_uptime;
#[cfg(windows)]
pub use windows::collect_uptime;
#[cfg(target_os = "macos")]
pub use macos::collect_uptime;
#[cfg(target_os = "freebsd")]
pub use freebsd::collect_uptime;

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
pub use stub::collect_uptime;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
mod stub {
    use std::io;

    pub fn collect_uptime() -> Result<u64, io::Error> {
        Ok(0)
    }
}
