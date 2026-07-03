// komari-agent-rs: CPU metrics module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/cpu.go

#![allow(unused_imports)]

pub(crate) fn usage_from_ticks(prev_total: u64, prev_idle: u64, total: u64, idle: u64) -> f64 {
    let total_delta = match total.checked_sub(prev_total) {
        Some(delta) if prev_total > 0 && delta > 0 => delta,
        _ => return 0.0,
    };
    let idle_delta = idle.saturating_sub(prev_idle).min(total_delta);
    ((total_delta - idle_delta) as f64 / total_delta as f64) * 100.0
}

#[cfg(target_os = "freebsd")]
pub mod freebsd;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "freebsd")]
pub use freebsd::{CpuInfo, MetricErr, PrevCpu, collect_cpu};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use linux::{CpuInfo, MetricErr, PrevCpu, collect_cpu};
#[cfg(target_os = "macos")]
pub use macos::{CpuInfo, MetricErr, PrevCpu, collect_cpu};
#[cfg(windows)]
pub use windows::{CpuInfo, PrevCpu, collect_cpu};

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub use stub::{CpuInfo, PrevCpu, collect_cpu};

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
mod stub {
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

    pub fn collect_cpu<'a>(
        arena: &'a mut ScratchArena,
        prev: &mut PrevCpu,
    ) -> Result<CpuInfo<'a>, MetricErr> {
        *prev = PrevCpu { total: 1, idle: 0 };
        let name = arena.alloc_str("Unknown");
        Ok(CpuInfo {
            name,
            cores: 0,
            physical_cores: 0,
            arch: name,
            usage: 0.001,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::usage_from_ticks;

    #[test]
    fn usage_from_ticks_bounds_normal_delta() {
        assert_eq!(usage_from_ticks(100, 40, 200, 70), 70.0);
    }

    #[test]
    fn usage_from_ticks_handles_counter_reset_or_idle_anomaly() {
        assert_eq!(usage_from_ticks(100, 40, 90, 45), 0.0);
        assert_eq!(usage_from_ticks(100, 80, 200, 60), 100.0);
        assert_eq!(usage_from_ticks(100, 10, 200, 500), 0.0);
    }
}
