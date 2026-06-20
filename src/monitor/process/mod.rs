// komari-agent-rs: monitor::process — process count metric.

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect_process_count, MetricErr};
