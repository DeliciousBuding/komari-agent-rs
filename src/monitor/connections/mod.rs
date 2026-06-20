// komari-agent-rs: monitor::connections — TCP/UDP connection counting.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::{collect_connections, ConnectionsInfo, MetricErr};
#[cfg(windows)]
pub use windows::{collect_connections, ConnectionsInfo};
#[cfg(target_os = "macos")]
pub use macos::{collect_connections, ConnectionsInfo, MetricErr};
