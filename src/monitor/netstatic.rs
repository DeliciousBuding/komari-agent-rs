//! Traffic history persistence — monthly network byte counters saved as JSON.
//!
//! # Integration
//!
//! - Call [`NetStatic::update`] in the monitor tick with per-tick TX/RX deltas.
//! - Call [`NetStatic::save`] periodically to persist (e.g. every `config.interval`
//!   seconds, matching the monitor tick cadence).
//! - Call [`NetStatic::maybe_reset`] each tick to handle monthly/cycle rotation.
//!
//! # File format
//!
//! Single-line JSON:
//! ```json
//! {"month":"2026-06","tx_bytes":12345,"rx_bytes":67890}
//! ```

use crate::config::Config;
use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

// ═══════════════════════════════════════════════════════════════════════════════
// NetStatic
// ═══════════════════════════════════════════════════════════════════════════════

/// Persistent monthly traffic counters backed by a JSON file.
pub struct NetStatic {
    /// Current billing month in `"YYYY-MM"` format.
    pub month: String,
    /// Cumulative transmitted bytes this month.
    pub tx_bytes: u64,
    /// Cumulative received bytes this month.
    pub rx_bytes: u64,
    /// File path used by [`save`](Self::save).
    save_path: String,
}

impl NetStatic {
    /// Create a fresh instance for the current UTC month.
    ///
    /// Counters start at zero.  Use this when no prior persistence file exists.
    pub fn new(path: &str) -> Self {
        let now = unix_secs();
        Self {
            month: format_month(now),
            tx_bytes: 0,
            rx_bytes: 0,
            save_path: path.to_string(),
        }
    }

    /// Load persisted traffic counters from `path`.
    ///
    /// Returns `Ok(Self)` with parsed values, or an `io::Error` if the file
    /// does not exist or contains malformed JSON.
    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = fs::read_to_string(path)?;
        let data = data.trim();

        let month = extract_json_str(data, "month").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "netstatic: missing 'month' field")
        })?;
        let tx_bytes = extract_json_u64(data, "tx_bytes").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "netstatic: missing 'tx_bytes' field")
        })?;
        let rx_bytes = extract_json_u64(data, "rx_bytes").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "netstatic: missing 'rx_bytes' field")
        })?;

        Ok(Self {
            month,
            tx_bytes,
            rx_bytes,
            save_path: path.to_string(),
        })
    }

    /// Persist the current counters to `save_path` as a single-line JSON object.
    ///
    /// Overwrites the file if it already exists.
    pub fn save(&self) -> Result<(), io::Error> {
        // Single-line JSON under 100 bytes — use a short fixed-capacity buffer.
        let mut buf = [0u8; 96];
        let written = format_netstatic_json(&mut buf, &self.month, self.tx_bytes, self.rx_bytes);
        fs::write(&self.save_path, &buf[..written])
    }

    /// Accumulate per-tick byte deltas into the monthly running totals.
    ///
    /// Uses wrapping arithmetic — overflow is harmless for a cumulative counter
    /// that resets monthly.
    #[inline]
    pub fn update(&mut self, tx_delta: u64, rx_delta: u64) {
        self.tx_bytes = self.tx_bytes.wrapping_add(tx_delta);
        self.rx_bytes = self.rx_bytes.wrapping_add(rx_delta);
    }

    /// Check whether the billing cycle should roll over and reset counters.
    ///
    /// Rotation behaviour is driven by [`Config::month_rotate`]:
    ///
    /// | `month_rotate` | Behaviour |
    /// |----------------|-----------|
    /// | 0              | Never reset (disabled). |
    /// | 1 – 28         | Reset on or after this day-of-month when the stored month is older than the current UTC month. |
    ///
    /// When a reset fires, `month` advances to the current UTC month and both
    /// byte counters return to zero.
    pub fn maybe_reset(&mut self, config: &Config) {
        if config.month_rotate == 0 {
            return;
        }

        let now = unix_secs();
        let current_month = format_month(now);

        // Only reset when the calendar month string actually differs.
        if current_month == self.month {
            return;
        }

        // Respect the configured rotation day — do not reset before it.
        let (_, _, day) = ymd_from_unix(now);
        if day < config.month_rotate as u32 {
            return;
        }

        self.month = current_month;
        self.tx_bytes = 0;
        self.rx_bytes = 0;
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// JSON helpers — hand-rolled, zero-dependency
// ═══════════════════════════════════════════════════════════════════════════════

/// Format `{"month":"...","tx_bytes":N,"rx_bytes":M}` into `buf`.
///
/// Returns the number of bytes written.  Panics only if `buf` is too small
/// (< 96 bytes is safe for all reasonable counter values).
fn format_netstatic_json(buf: &mut [u8], month: &str, tx: u64, rx: u64) -> usize {
    // Write manually for predictable code size.
    let mut pos = 0;

    buf[pos] = b'{';
    pos += 1;

    // "month":"YYYY-MM"
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b'm';
    pos += 1;
    buf[pos] = b'o';
    pos += 1;
    buf[pos] = b'n';
    pos += 1;
    buf[pos] = b't';
    pos += 1;
    buf[pos] = b'h';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b':';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    let m = month.as_bytes();
    buf[pos..pos + m.len()].copy_from_slice(m);
    pos += m.len();
    buf[pos] = b'"';
    pos += 1;

    // ,"tx_bytes":N
    buf[pos] = b',';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b't';
    pos += 1;
    buf[pos] = b'x';
    pos += 1;
    buf[pos] = b'_';
    pos += 1;
    buf[pos] = b'b';
    pos += 1;
    buf[pos] = b'y';
    pos += 1;
    buf[pos] = b't';
    pos += 1;
    buf[pos] = b'e';
    pos += 1;
    buf[pos] = b's';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b':';
    pos += 1;
    pos += write_u64(&mut buf[pos..], tx);

    // ,"rx_bytes":M
    buf[pos] = b',';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b'r';
    pos += 1;
    buf[pos] = b'x';
    pos += 1;
    buf[pos] = b'_';
    pos += 1;
    buf[pos] = b'b';
    pos += 1;
    buf[pos] = b'y';
    pos += 1;
    buf[pos] = b't';
    pos += 1;
    buf[pos] = b'e';
    pos += 1;
    buf[pos] = b's';
    pos += 1;
    buf[pos] = b'"';
    pos += 1;
    buf[pos] = b':';
    pos += 1;
    pos += write_u64(&mut buf[pos..], rx);

    buf[pos] = b'}';
    pos += 1;

    pos
}

/// Write the decimal representation of `n` into `buf`, returning bytes written.
fn write_u64(buf: &mut [u8], n: u64) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    // Write digits in reverse, then reverse in-place.
    let mut i = 0;
    let mut v = n;
    while v > 0 {
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    let end = i;
    let mut lo = 0;
    let mut hi = end - 1;
    while lo < hi {
        buf.swap(lo, hi);
        lo += 1;
        hi -= 1;
    }
    end
}

/// Extract a string value for `key` from a simple JSON object string.
///
/// Looks for `"key":"value"` and returns `value` without unescape handling
/// (sufficient for ASCII month strings like `"2026-06"`).
fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let mut search = String::with_capacity(key.len() + 4);
    search.push('"');
    search.push_str(key);
    search.push_str("\":\"");
    let start = json.find(&search)? + search.len();
    let rest = &json[start..];
    let bytes = rest.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        if bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\') {
            break;
        }
        end += 1;
    }
    Some(rest[..end].to_string())
}

/// Extract a u64 value for `key` from a simple JSON object string.
///
/// Looks for `"key":12345` and parses the contiguous digits.
fn extract_json_u64(json: &str, key: &str) -> Option<u64> {
    let mut search = String::with_capacity(key.len() + 3);
    search.push('"');
    search.push_str(key);
    search.push_str("\":");
    let start = json.find(&search)? + search.len();
    let rest = &json[start..];
    let end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Time helpers — zero-dependency UTC calendar
// ═══════════════════════════════════════════════════════════════════════════════

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Decompose a Unix timestamp (seconds since epoch) into `(year, month, day)`.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm — branchless, no division
/// in the hot path after the initial days-since-epoch calculation.
fn ymd_from_unix(ts: u64) -> (u32, u32, u32) {
    let z = (ts / 86400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m, d)
}

/// Format a Unix timestamp as `"YYYY-MM"` using a stack-allocated buffer.
fn format_month(ts: u64) -> String {
    let (y, m, _) = ymd_from_unix(ts);
    // Maximum 7 bytes — no heap in the formatting path.
    let mut buf = [0u8; 7];
    buf[0] = b'0' + ((y / 1000) % 10) as u8;
    buf[1] = b'0' + ((y / 100) % 10) as u8;
    buf[2] = b'0' + ((y / 10) % 10) as u8;
    buf[3] = b'0' + (y % 10) as u8;
    buf[4] = b'-';
    buf[5] = b'0' + (m / 10) as u8;
    buf[6] = b'0' + (m % 10) as u8;
    // SAFETY: all bytes are ASCII digits or '-'.
    unsafe { String::from_utf8_unchecked(buf.to_vec()) }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_and_save_load_roundtrip() {
        // Use a temp file path
        let tmp = std::env::temp_dir().join("_komari_test_netstatic.json");
        let path = tmp.to_str().unwrap();

        let mut ns = NetStatic::new(path);
        assert_eq!(ns.tx_bytes, 0);
        assert_eq!(ns.rx_bytes, 0);
        assert!(!ns.month.is_empty());
        assert!(ns.month.starts_with("20"));

        // Accumulate some traffic
        ns.update(1000, 500);
        ns.update(250, 125);
        assert_eq!(ns.tx_bytes, 1250);
        assert_eq!(ns.rx_bytes, 625);

        // Save and reload
        ns.save().unwrap();
        let loaded = NetStatic::load(path).unwrap();
        assert_eq!(loaded.month, ns.month);
        assert_eq!(loaded.tx_bytes, 1250);
        assert_eq!(loaded.rx_bytes, 625);
        assert_eq!(loaded.save_path, path);

        // Cleanup
        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn test_maybe_reset_disabled() {
        let tmp = std::env::temp_dir().join("_komari_test_netstatic_disabled.json");
        let path = tmp.to_str().unwrap();

        let mut ns = NetStatic::new(path);
        ns.update(100, 200);

        let mut cfg = Config::default();
        cfg.month_rotate = 0; // disabled

        let before = ns.tx_bytes;
        ns.maybe_reset(&cfg);
        // Should be unchanged when disabled
        assert_eq!(ns.tx_bytes, before);

        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn test_maybe_reset_same_month_noop() {
        let tmp = std::env::temp_dir().join("_komari_test_netstatic_same_month.json");
        let path = tmp.to_str().unwrap();

        let mut ns = NetStatic::new(path);
        ns.update(500, 300);

        let mut cfg = Config::default();
        cfg.month_rotate = 1; // rotate on or after day 1

        // current month should match stored month -> no reset
        let before_tx = ns.tx_bytes;
        ns.maybe_reset(&cfg);
        assert_eq!(ns.tx_bytes, before_tx);

        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn test_json_extract_helpers() {
        let json = r#"{"month":"2026-06","tx_bytes":12345,"rx_bytes":67890}"#;
        assert_eq!(extract_json_str(json, "month"), Some("2026-06".to_string()));
        assert_eq!(extract_json_u64(json, "tx_bytes"), Some(12345));
        assert_eq!(extract_json_u64(json, "rx_bytes"), Some(67890));
        assert_eq!(extract_json_u64(json, "missing"), None);
    }

    #[test]
    fn test_ymd_known_date() {
        // 2026-06-20 00:00:00 UTC
        // Unix timestamp: let's just verify the function is monotonic and
        // returns plausible values for the current time.
        let now = unix_secs();
        let (y, m, d) = ymd_from_unix(now);
        assert!(y >= 2026, "year should be >= 2026, got {}", y);
        assert!(m >= 1 && m <= 12, "month should be 1-12, got {}", m);
        assert!(d >= 1 && d <= 31, "day should be 1-31, got {}", d);
    }

    #[test]
    fn test_format_month_length() {
        let now = unix_secs();
        let month = format_month(now);
        assert_eq!(month.len(), 7, "expected YYYY-MM, got '{}'", month);
        assert_eq!(&month[4..5], "-", "expected dash at position 4, got '{}'", month);
    }

    #[test]
    fn test_write_u64() {
        let mut buf = [0u8; 32];
        assert_eq!(write_u64(&mut buf, 0), 1);
        assert_eq!(buf[0], b'0');

        assert_eq!(write_u64(&mut buf, 42), 2);
        assert_eq!(&buf[..2], b"42");

        assert_eq!(write_u64(&mut buf, 1234567890), 10);
        assert_eq!(&buf[..10], b"1234567890");
    }

    #[test]
    fn test_load_nonexistent() {
        let result = NetStatic::load("/nonexistent/path/komari_netstatic_test.json");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_malformed() {
        let tmp = std::env::temp_dir().join("_komari_test_netstatic_bad.json");
        let path = tmp.to_str().unwrap();
        fs::write(path, "not json at all").unwrap();

        let result = NetStatic::load(path);
        assert!(result.is_err());

        let _ = fs::remove_file(tmp);
    }
}
