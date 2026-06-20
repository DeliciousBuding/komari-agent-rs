// komari-agent-rs: monitor::load — system load average.

use std::io;

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadInfo {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "freebsd")]
pub mod freebsd;

/// Collect system load average. Returns zeros on unsupported platforms.
pub fn collect() -> Result<LoadInfo, io::Error> {
    #[cfg(target_os = "linux")]
    {
        return linux::collect();
    }
    #[cfg(windows)]
    {
        return windows::collect();
    }
    #[cfg(target_os = "macos")]
    {
        return macos::collect();
    }
    #[cfg(target_os = "freebsd")]
    {
        return freebsd::collect();
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
    {
        Ok(LoadInfo::default())
    }
}
