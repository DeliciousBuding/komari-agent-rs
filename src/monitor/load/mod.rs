// komari-agent-rs: monitor::load — system load average.

use std::io;

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadInfo {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

pub mod linux;

/// Collect system load average. Returns zeros on unsupported platforms.
pub fn collect() -> Result<LoadInfo, io::Error> {
    #[cfg(target_os = "linux")]
    {
        return linux::collect();
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(LoadInfo::default())
    }
}
