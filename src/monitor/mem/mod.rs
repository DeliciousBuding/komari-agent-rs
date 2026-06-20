//! Memory and swap metrics — Linux via `/proc/meminfo`.

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect, MemInfo};
