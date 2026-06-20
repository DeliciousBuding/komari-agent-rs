// komari-agent-rs: monitor::process — process count metric.

#[cfg(target_os = "freebsd")]
pub mod freebsd;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "freebsd")]
pub use freebsd::{MetricErr, collect_process_count};
#[cfg(target_os = "linux")]
pub use linux::{MetricErr, collect_process_count};
#[cfg(target_os = "macos")]
pub use macos::{MetricErr, collect_process_count};
#[cfg(windows)]
pub use windows::collect_process_count;

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub use stub::{MetricErr, collect_process_count};

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
mod stub {
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

    pub fn collect_process_count() -> Result<u32, MetricErr> {
        Ok(0)
    }
}
