//! Delta calculation helper for cumulative counters across monitoring ticks.
//!
//! Computes per-second rates from monotonically-increasing counters (e.g.
//! `/proc/net/dev` RX/TX bytes, `/proc/diskstats` sector counts).  On the
//! first call to [`update`](Delta::update) the return value is 0 — there is
//! no previous sample to compute a delta from.

pub mod linux;

#[allow(unused_imports)]
pub use linux::{collect, NetInfo, PrevNetSnapshot};

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
