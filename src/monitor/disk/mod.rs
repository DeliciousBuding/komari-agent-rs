// komari-agent-rs: monitor::disk — Linux statfs / Windows GetDiskFreeSpaceExW.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "freebsd")]
pub mod freebsd;

#[cfg(target_os = "linux")]
pub use linux::{collect, aggregate, DiskInfo};
#[cfg(windows)]
pub use windows::{collect, aggregate};
#[cfg(target_os = "macos")]
pub use macos::{collect, aggregate, DiskInfo};
#[cfg(target_os = "freebsd")]
pub use freebsd::{collect, aggregate, DiskInfo};

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
pub use stub::{collect, aggregate, DiskInfo};

#[cfg(not(any(target_os = "linux", windows, target_os = "macos", target_os = "freebsd")))]
mod stub {
    use crate::arena::{SmallVec, MAX_DISKS};
    use crate::config::Config;

    pub struct DiskInfo {
        pub mp_buf: [u8; 192],
        pub mp_len: u8,
        pub fs_buf: [u8; 32],
        pub fs_len: u8,
        pub total: u64,
        pub used: u64,
    }

    pub fn collect(_config: &Config) -> SmallVec<DiskInfo, MAX_DISKS> {
        SmallVec::new()
    }

    pub fn aggregate(disks: &[DiskInfo]) -> (u64, u64) {
        let mut total = 0u64;
        let mut used = 0u64;
        for d in disks {
            total += d.total;
            used += d.used;
        }
        (total, used)
    }
}
