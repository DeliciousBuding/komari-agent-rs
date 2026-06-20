//! Exponential backoff for reconnection attempts.
//!
//! Mirrors the Go reconnection backoff: base 1 s, doubled on each failure,
//! capped at `reconnect_interval`, with an optional `max_retries` limit
//! (0 = unlimited — never give up).

use std::time::Duration;

/// Tracks consecutive failure count and computes backoff durations.
#[derive(Debug, Clone)]
pub struct Backoff {
    /// Number of consecutive failures so far.
    pub failures: u64,
    /// Maximum number of retries before giving up (0 = unlimited).
    pub max_retries: u64,
    /// Base backoff duration (doubled on each failure).
    pub base: Duration,
    /// Maximum backoff duration (capped here regardless of failures).
    pub cap: Duration,
}

impl Backoff {
    /// Create a new backoff tracker.
    ///
    /// - `max_retries` from config (0 means unlimited — never give up).
    /// - `cap_secs` is typically `config.reconnect_interval` in seconds.
    pub fn new(max_retries: u64, cap_secs: u64) -> Self {
        Self {
            failures: 0,
            max_retries,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(cap_secs),
        }
    }

    /// Whether we have exhausted retries (only when `max_retries > 0`).
    pub fn exhausted(&self) -> bool {
        self.max_retries > 0 && self.failures >= self.max_retries
    }

    /// Record a failure and return the duration to sleep before the next
    /// attempt.
    ///
    /// Doubles the base on each call, capped at `self.cap`.
    pub fn next_delay(&mut self) -> Duration {
        self.failures += 1;
        let raw = self.base * 2u32.pow(self.failures.min(31) as u32);
        raw.min(self.cap)
    }

    /// Reset failure count (called on successful connection).
    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_grows_and_caps() {
        let mut b = Backoff::new(10, 30);
        // 1st failure: 1 * 2^1 = 2 s
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        // 2nd: 1 * 2^2 = 4 s
        assert_eq!(b.next_delay(), Duration::from_secs(4));
        // 3rd: 8 s
        assert_eq!(b.next_delay(), Duration::from_secs(8));
        // 4th: 16 s
        assert_eq!(b.next_delay(), Duration::from_secs(16));
        // 5th: 32 s -> capped at 30 s
        assert_eq!(b.next_delay(), Duration::from_secs(30));
        // 6th: still capped
        assert_eq!(b.next_delay(), Duration::from_secs(30));
        assert!(!b.exhausted());
    }

    #[test]
    fn test_backoff_exhaustion() {
        let mut b = Backoff::new(3, 60);
        b.next_delay(); // 1st
        b.next_delay(); // 2nd
        b.next_delay(); // 3rd -> failures == 3 == max_retries
        assert!(b.exhausted());
    }

    #[test]
    fn test_backoff_unlimited() {
        let mut b = Backoff::new(0, 60);
        for _ in 0..100 {
            b.next_delay();
        }
        assert!(!b.exhausted());
    }

    #[test]
    fn test_backoff_reset() {
        let mut b = Backoff::new(10, 30);
        b.next_delay();
        b.next_delay();
        assert_eq!(b.failures, 2);
        b.reset();
        assert_eq!(b.failures, 0);
        // Fresh start: 2 s again
        assert_eq!(b.next_delay(), Duration::from_secs(2));
    }
}
