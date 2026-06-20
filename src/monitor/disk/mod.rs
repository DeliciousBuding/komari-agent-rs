// komari-agent-rs: monitor::disk — Linux disk metrics (statfs + /proc/mounts)

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect, aggregate, DiskInfo};
