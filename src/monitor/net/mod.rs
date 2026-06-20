//! Delta calculation helper for cumulative counters across monitoring ticks.
//!
//! Computes per-second rates from monotonically-increasing counters (e.g.
//! `/proc/net/dev` RX/TX bytes, `/proc/diskstats` sector counts).  On the
//! first call to [`update`](Delta::update) the return value is 0 — there is
//! no previous sample to compute a delta from.

#[cfg(target_os = "freebsd")]
pub mod freebsd;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "freebsd")]
pub use freebsd::{NetInfo, PrevNetSnapshot, collect};
#[cfg(target_os = "linux")]
pub use linux::{NetInfo, PrevNetSnapshot, collect};
#[cfg(target_os = "macos")]
pub use macos::{NetInfo, PrevNetSnapshot, collect};
#[cfg(windows)]
pub use windows::{PrevNetSnapshot, collect};

// ── Stub for unsupported platforms ──────────────────────────────────────────
#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub use stub::{NetInfo, PrevNetSnapshot, collect};

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
mod stub {
    use crate::arena::{MAX_NETWORKS, SmallVec};
    use crate::config::Config;
    use std::time::Instant;

    pub struct NetInfo {
        pub name_buf: [u8; 16],
        pub name_len: u8,
        pub up: u64,
        pub down: u64,
        pub total_up: u64,
        pub total_down: u64,
    }

    impl NetInfo {
        #[inline]
        pub fn name(&self) -> &str {
            std::str::from_utf8(&self.name_buf[..self.name_len as usize]).unwrap_or("?")
        }
    }

    pub struct PrevNetSnapshot {
        pub names: [[u8; 16]; MAX_NETWORKS],
        pub name_lens: [u8; MAX_NETWORKS],
        pub rx: [u64; MAX_NETWORKS],
        pub tx: [u64; MAX_NETWORKS],
        pub ts: Instant,
        pub len: u8,
    }

    impl PrevNetSnapshot {
        pub fn new() -> Self {
            Self {
                names: [[0u8; 16]; MAX_NETWORKS],
                name_lens: [0u8; MAX_NETWORKS],
                rx: [0u64; MAX_NETWORKS],
                tx: [0u64; MAX_NETWORKS],
                ts: Instant::now(),
                len: 0,
            }
        }
    }

    pub fn collect(
        _config: &Config,
        _prev: &mut PrevNetSnapshot,
    ) -> SmallVec<NetInfo, MAX_NETWORKS> {
        SmallVec::new()
    }
}

use std::time::Instant;

/// Tracks a single cumulative counter across ticks to compute per-second rates.
///
/// No heap allocation.  Create one instance per monitored counter (e.g. one
/// for RX bytes, one for TX bytes) or per interface when counters are stored
/// externally (see [`super::linux::PrevNetSnapshot`]).
pub(crate) struct Delta {
    prev: u64,
    ts: Instant,
    ready: bool,
}

impl Delta {
    /// Create a new delta tracker with no previous sample.
    #[inline]
    pub fn new() -> Self {
        Self {
            prev: 0,
            ts: Instant::now(),
            ready: false,
        }
    }

    /// Feed the current cumulative counter value.
    ///
    /// Returns bytes/sec (rounded toward zero).  On the very first call the
    /// return value is 0 — there is no baseline to compute a delta from.
    /// Counter wraps are handled via wrapping subtraction.
    pub fn update(&mut self, cur: u64) -> u64 {
        let now = Instant::now();
        if !self.ready {
            self.prev = cur;
            self.ts = now;
            self.ready = true;
            return 0;
        }
        let delta = cur.wrapping_sub(self.prev);
        let elapsed = now.duration_since(self.ts).as_secs_f64();
        self.prev = cur;
        self.ts = now;
        if elapsed < 0.001 {
            0
        } else {
            (delta as f64 / elapsed) as u64
        }
    }
}
