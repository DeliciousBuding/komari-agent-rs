// komari-agent-rs: public/private IP detection module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/ip.go

#[cfg(target_os = "freebsd")]
pub mod freebsd;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "freebsd")]
pub use freebsd::collect_ip;
#[cfg(target_os = "linux")]
pub use linux::collect_ip;
#[cfg(target_os = "macos")]
pub use macos::collect_ip;
#[cfg(windows)]
pub use windows::collect_ip;

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub use stub::collect_ip;

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
mod stub {
    use crate::config::Config;
    use std::fmt;
    use std::io;

    #[derive(Debug)]
    pub enum MetricErr {
        Io(io::Error),
        Parse(String),
    }

    impl fmt::Display for MetricErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Io(e) => write!(f, "IO error: {}", e),
                Self::Parse(s) => write!(f, "parse error: {}", s),
            }
        }
    }

    impl std::error::Error for MetricErr {}

    impl From<io::Error> for MetricErr {
        fn from(e: io::Error) -> Self {
            Self::Io(e)
        }
    }

    pub fn collect_ip(_config: &Config) -> Result<(Option<String>, Option<String>), MetricErr> {
        Ok((None, None))
    }
}
