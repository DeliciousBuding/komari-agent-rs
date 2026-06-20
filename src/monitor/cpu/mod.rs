// komari-agent-rs: CPU metrics module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/cpu.go

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect_cpu, CpuInfo, MetricErr, PrevCpu};
