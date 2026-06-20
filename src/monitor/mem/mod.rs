//! Memory and swap metrics — Linux via `/proc/meminfo`, Windows via GlobalMemoryStatusEx.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::{collect, MemInfo};
#[cfg(windows)]
pub use windows::collect;
#[cfg(target_os = "macos")]
pub use macos::{collect, MemInfo};
