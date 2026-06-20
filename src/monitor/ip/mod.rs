// komari-agent-rs: public/private IP detection module.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/ip.go

pub mod linux;

pub use linux::collect_ip;
