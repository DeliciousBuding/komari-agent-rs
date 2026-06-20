// komari-agent-rs: monitor::disk — Linux statfs / Windows GetDiskFreeSpaceExW.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::{collect, aggregate, DiskInfo};
#[cfg(windows)]
pub use windows::{collect, aggregate};
#[cfg(target_os = "macos")]
pub use macos::{collect, aggregate, DiskInfo};
