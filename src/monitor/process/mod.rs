// komari-agent-rs: monitor::process — process count metric.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::{collect_process_count, MetricErr};
#[cfg(windows)]
pub use windows::collect_process_count;
#[cfg(target_os = "macos")]
pub use macos::{collect_process_count, MetricErr};
