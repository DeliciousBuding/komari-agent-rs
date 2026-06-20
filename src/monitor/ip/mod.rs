// komari-agent-rs: public/private IP detection module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/ip.go

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::collect_ip;
#[cfg(windows)]
pub use windows::collect_ip;
#[cfg(target_os = "macos")]
pub use macos::collect_ip;
