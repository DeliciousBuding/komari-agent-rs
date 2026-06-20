//! Memory and swap metrics — Linux via `/proc/meminfo`, Windows via GlobalMemoryStatusEx.

#[cfg(target_os = "freebsd")]
pub mod freebsd;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "freebsd")]
pub use freebsd::{MemInfo, collect};
#[cfg(target_os = "linux")]
pub use linux::{MemInfo, collect};
#[cfg(target_os = "macos")]
pub use macos::{MemInfo, collect};
#[cfg(windows)]
pub use windows::collect;

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub use stub::{MemInfo, collect};

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
mod stub {
    use crate::config::Config;

    #[derive(Debug, Default, Clone, Copy)]
    pub struct MemInfo {
        pub total: u64,
        pub used: u64,
        pub swap_total: u64,
        pub swap_used: u64,
    }

    pub fn collect(_config: &Config) -> MemInfo {
        MemInfo::default()
    }
}
