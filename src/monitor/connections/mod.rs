// komari-agent-rs: monitor::connections — TCP/UDP connection counting via /proc/net.

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect_connections, ConnectionsInfo, MetricErr};
