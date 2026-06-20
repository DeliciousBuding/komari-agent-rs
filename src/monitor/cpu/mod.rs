// komari-agent-rs: CPU metrics module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/cpu.go

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::{collect_cpu, CpuInfo, MetricErr, PrevCpu};
#[cfg(windows)]
pub use windows::{collect_cpu, CpuInfo, PrevCpu};
#[cfg(target_os = "macos")]
pub use macos::{collect_cpu, CpuInfo, MetricErr, PrevCpu};
