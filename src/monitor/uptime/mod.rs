// komari-agent-rs: monitor::uptime — system uptime via /proc/uptime on Linux.

pub mod linux;

pub use linux::collect_uptime;
