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

// ── Stub for unsupported platforms (FreeBSD, etc.) ──────────────────────────
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub use stub::{collect_process_count, MetricErr};

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod stub {
    use std::io;

    #[derive(Debug)]
    pub enum MetricErr { Io(io::Error), Parse(String) }
    impl From<io::Error> for MetricErr {
        fn from(e: io::Error) -> Self { MetricErr::Io(e) }
    }

    pub fn collect_process_count() -> Result<u32, MetricErr> {
        Ok(0)
    }
}
