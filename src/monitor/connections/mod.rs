// komari-agent-rs: monitor::connections — TCP/UDP connection counting.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "freebsd")]
pub mod freebsd;

#[cfg(target_os = "linux")]
pub use linux::{collect_connections, ConnectionsInfo, MetricErr};
#[cfg(windows)]
pub use windows::{collect_connections, ConnectionsInfo};
#[cfg(target_os = "macos")]
pub use macos::{collect_connections, ConnectionsInfo, MetricErr};
#[cfg(target_os = "freebsd")]
pub use freebsd::{collect_connections, ConnectionsInfo, MetricErr};

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
pub use stub::{collect_connections, ConnectionsInfo, MetricErr};

#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
mod stub {
    use std::io;

    #[derive(Debug)]
    pub enum MetricErr { Io(io::Error), Parse(String) }
    impl From<io::Error> for MetricErr {
        fn from(e: io::Error) -> Self { MetricErr::Io(e) }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct ConnectionsInfo {
        pub tcp: u64,
        pub udp: u64,
    }

    pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
        Ok(ConnectionsInfo { tcp: 0, udp: 0 })
    }
}
