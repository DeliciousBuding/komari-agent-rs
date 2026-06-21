// komari-agent-rs: CPU metrics module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/cpu.go

#![allow(unused_imports)]

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
