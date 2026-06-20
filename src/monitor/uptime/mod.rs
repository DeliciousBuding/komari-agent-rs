// komari-agent-rs: monitor::uptime — system uptime via /proc/uptime or GetTickCount64.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::collect_uptime;
#[cfg(windows)]
pub use windows::collect_uptime;
#[cfg(target_os = "macos")]
pub use macos::collect_uptime;
