//! Delta calculation helper for cumulative counters across monitoring ticks.
//!
//! Computes per-second rates from monotonically-increasing counters (e.g.
//! `/proc/net/dev` RX/TX bytes, `/proc/diskstats` sector counts).  On the
//! first call to [`update`](Delta::update) the return value is 0 — there is
//! no previous sample to compute a delta from.

#![allow(unused_imports)]

/// Upstream Go `monitoring/unit/net.go:shouldInclude`: built-in exclusions
/// cannot be overridden, then an include match wins even over an exclude.
pub(crate) fn include_nic(name: &str, config: &crate::config::Config) -> bool {
    const EXCLUDED_PREFIXES: &[&str] = &[
        "br", "cni", "docker", "podman", "flannel", "lo", "veth", "virbr", "vmbr", "tap", "fwbr",
        "fwpr",
    ];
    if EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return false;
    }
    if config
        .include_nics
        .iter()
        .any(|p| nic_pattern_match(p, name))
    {
        return true;
    }
    if config
        .exclude_nics
        .iter()
        .any(|p| nic_pattern_match(p, name))
    {
        return false;
    }
    config.include_nics.is_empty()
}

enum PatternTerm {
    Star,
    Any,
    Literal(char),
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

fn class_char(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, windows: bool) -> Option<char> {
    let ch = chars.next()?;
    if ch == '-' || ch == ']' {
        return None;
    }
    let value = if ch == '\\' && !windows {
        chars.next()?
    } else {
        ch
    };
    // Go's getEsc requires another character (at least the closing bracket).
    chars.peek()?;
    Some(value)
}

/// Go filepath.Match syntax, including Unicode ranges and native separator /
/// escape rules. Invalid patterns never match. Dynamic programming keeps an
/// adversarial sequence of stars bounded by O(pattern length * name length),
/// rather than recursive backtracking. Parsed terms and two rows need O(pattern
/// length + name length) memory.
fn nic_pattern_match(pattern: &str, name: &str) -> bool {
    pattern_match_for_platform(pattern, name, cfg!(windows))
}

fn pattern_match_for_platform(pattern: &str, name: &str, windows: bool) -> bool {
    let mut chars = pattern.chars().peekable();
    let mut terms = Vec::new();
    while let Some(ch) = chars.next() {
        let term = match ch {
            '*' => PatternTerm::Star,
            '?' => PatternTerm::Any,
            '\\' if !windows => {
                let Some(literal) = chars.next() else {
                    return false;
                };
                PatternTerm::Literal(literal)
            }
            '[' => {
                let negated = chars.next_if_eq(&'^').is_some();
                let mut ranges = Vec::new();
                loop {
                    if !ranges.is_empty() && chars.next_if_eq(&']').is_some() {
                        break;
                    }
                    let Some(lo) = class_char(&mut chars, windows) else {
                        return false;
                    };
                    let hi = if chars.next_if_eq(&'-').is_some() {
                        let Some(hi) = class_char(&mut chars, windows) else {
                            return false;
                        };
                        hi
                    } else {
                        lo
                    };
                    ranges.push((lo, hi));
                }
                PatternTerm::Class { negated, ranges }
            }
            literal => PatternTerm::Literal(literal),
        };
        terms.push(term);
    }
    let name: Vec<char> = name.chars().collect();
    let separator = if windows { '\\' } else { '/' };
    let mut previous = vec![false; name.len() + 1];
    let mut next = vec![false; name.len() + 1];
    previous[0] = true;
    for term in terms {
        next.fill(false);
        if matches!(term, PatternTerm::Star) {
            next[0] = previous[0];
        }
        for (index, &ch) in name.iter().enumerate() {
            next[index + 1] = match &term {
                PatternTerm::Star => previous[index + 1] || (next[index] && ch != separator),
                PatternTerm::Any => previous[index] && ch != separator,
                PatternTerm::Literal(literal) => previous[index] && *literal == ch,
                // Go permits a separator explicitly matched by a class.
                PatternTerm::Class { negated, ranges } => {
                    previous[index]
                        && (ranges.iter().any(|&(lo, hi)| lo <= ch && ch <= hi) != *negated)
                }
            };
        }
        std::mem::swap(&mut previous, &mut next);
    }
    previous[name.len()]
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn go_selection_precedence() {
        for (name, include, exclude, expected) in [
            ("eth0", "eth*", "eth0", true),
            ("eth0", "", "eth*", false),
            ("wlan0", "eth*", "", false),
            ("tun0", "tun*", "tun*", true),
            ("vnet0", "", "", true),
            ("utun0", "", "", true),
            ("docker0", "*", "", false),
            ("bridge0", "*", "", false),
            ("lo0", "*", "", false),
            ("eth0", "[", "", false),
            ("eth0", "", "[", true),
            ("eth1", "eth[01]", "*", true),
        ] {
            let mut config = Config::default();
            if !include.is_empty() {
                config.include_nics.push(include.into());
            }
            if !exclude.is_empty() {
                config.exclude_nics.push(exclude.into());
            }
            assert_eq!(
                include_nic(name, &config),
                expected,
                "{name}/{include}/{exclude}"
            );
        }
    }

    #[test]
    fn filepath_match_syntax_and_unicode() {
        for windows in [false, true] {
            for (pattern, name, expected) in [
                ("", "", true),
                ("", "eth0", false),
                ("eth?", "eth0", true),
                ("网?", "网卡", true),
                ("[网-龥]?", "网卡", true),
                ("e*[ht]*0", "ethernet0", true),
                ("e**0", "eth0", true),
                ("[^a-c]0", "e0", true),
                ("[^a-c]0", "b0", false),
                ("[!a]", "!", true),
                ("[z-a]", "z", false),
                ("[]", "", false),
                ("[^]", "x", false),
                ("[-a]", "a", false),
                ("[a-]", "a", false),
                ("[a", "a", false),
                ("[", "", false),
                ("[abc", "x", false),
                ("[a-b-c]", "a", false),
                ("x*[", "other", false),
                ("[/]", "/", true),
            ] {
                assert_eq!(
                    pattern_match_for_platform(pattern, name, windows),
                    expected,
                    "{pattern}/{name}/windows={windows}"
                );
            }
            let separator = if windows { "\\" } else { "/" };
            assert!(!pattern_match_for_platform("*", separator, windows));
            assert!(!pattern_match_for_platform("?", separator, windows));
        }
        assert!(pattern_match_for_platform(r"eth\?", "eth?", false));
        assert!(!pattern_match_for_platform(r"eth\", "eth", false));
        assert!(pattern_match_for_platform(r"[\-]", "-", false));
        assert!(pattern_match_for_platform(r"[\]a]", "]", false));
        assert!(pattern_match_for_platform(r"[x\-]", "-", false));
        assert!(pattern_match_for_platform(r"[\-x]", "x", false));
        assert!(!pattern_match_for_platform(r"eth\?", "eth?", true));
        assert!(pattern_match_for_platform(r"eth\?", r"eth\0", true));
        assert!(pattern_match_for_platform("*", "/", true));
    }

    #[test]
    fn many_stars_do_not_backtrack_exponentially() {
        let pattern = format!("{}b", "*a".repeat(256));
        assert!(!pattern_match_for_platform(
            &pattern,
            &"a".repeat(512),
            false
        ));
        assert!(pattern_match_for_platform(
            &pattern,
            &format!("{}b", "a".repeat(512)),
            false
        ));
    }
}

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
#[allow(unused_imports)]
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
#[allow(dead_code)]
pub(crate) struct Delta {
    prev: u64,
    ts: Instant,
    ready: bool,
}

#[allow(dead_code)]
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
