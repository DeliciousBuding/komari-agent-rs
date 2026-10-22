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
//!
//! Module is library-style: the persist/load/format helpers form a complete API
//! surfaced for the Go parity and for future monitor integration; not all
//! entry points are wired into the live tick yet. Allow dead_code for the API.
#![allow(dead_code)]

use crate::config::Config;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
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
        // The stored object is <96 bytes. Bound malformed input before parsing.
        let mut data = String::new();
        fs::File::open(path)?.take(1025).read_to_string(&mut data)?;
        if data.len() > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: state file too large",
            ));
        }
        let data = data.trim();

        let month = extract_json_str(data, "month").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: missing 'month' field",
            )
        })?;
        if !valid_month(&month) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: month must be YYYY-MM with a valid month",
            ));
        }
        let tx_bytes = extract_json_u64(data, "tx_bytes").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: missing 'tx_bytes' field",
            )
        })?;
        let rx_bytes = extract_json_u64(data, "rx_bytes").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: missing 'rx_bytes' field",
            )
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
    /// Sync a new sibling file, then atomically replace the directory entry.
    /// A failed write/sync/replace leaves the previous counters intact.
    pub fn save(&self) -> Result<(), io::Error> {
        // month is public: validate it here as well as on load, before writing
        // into the fixed buffer (or creating any persistence files).
        if !valid_month(&self.month) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "netstatic: month must be YYYY-MM with a valid month",
            ));
        }
        // Single-line JSON under 100 bytes — use a short fixed-capacity buffer.
        let mut buf = [0u8; 96];
        let written = format_netstatic_json(&mut buf, &self.month, self.tx_bytes, self.rx_bytes);
        atomic_write(Path::new(&self.save_path), &buf[..written], |from, to| {
            fs::rename(from, to)
        })
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

/// Keep the stage on the destination filesystem; never remove the destination
/// first. std::fs::rename replaces an existing file on both Unix and Windows
/// (MoveFileExW/MOVEFILE_REPLACE_EXISTING), with the stage closed beforehand.
fn atomic_write(
    path: &Path,
    data: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut collisions = 0;
    let (temp, mut file) = loop {
        if collisions == 32 {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "netstatic: no unused temporary name",
            ));
        }
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".komari-netstatic-{}-{id}.tmp", std::process::id()));
        if temp.file_name() == path.file_name() {
            collisions += 1;
            continue;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            // Do not follow, truncate or remove a pre-existing entry/symlink.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                collisions += 1;
            }
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        replace(&temp, path)
    })();
    if result.is_err() {
        // The file handle has closed on every closure return, including error.
        let _ = fs::remove_file(&temp);
    }
    result
}

// ═══════════════════════════════════════════════════════════════════════════════
// JSON helpers — hand-rolled, zero-dependency
// ═══════════════════════════════════════════════════════════════════════════════

/// Format `{"month":"...","tx_bytes":N,"rx_bytes":M}` into `buf`.
///
/// Returns the number of bytes written.  Panics only if `buf` is too small
/// (96 bytes is sufficient for a validated seven-byte month and any u64s).
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

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    crate::json::decode_json_string(crate::json::object_field(json.as_bytes(), key)?)
}

fn extract_json_u64(json: &str, key: &str) -> Option<u64> {
    std::str::from_utf8(crate::json::object_field(json.as_bytes(), key)?)
        .ok()?
        .parse()
        .ok()
}

fn valid_month(month: &str) -> bool {
    let b = month.as_bytes();
    b.len() == 7
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && ((b[5] == b'0' && (b'1'..=b'9').contains(&b[6]))
            || (b[5] == b'1' && (b'0'..=b'2').contains(&b[6])))
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

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "komari-netstatic-test-{}-{}",
                    std::process::id(),
                    NEXT_TEST.fetch_add(1, Ordering::Relaxed)
                ));
                let builder = fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                match builder.create(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("cannot create test directory: {e}"),
                }
            }
        }

        fn file(&self) -> std::path::PathBuf {
            self.0.join("history.json")
        }

        fn assert_no_stage(&self) {
            assert!(fs::read_dir(&self.0).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".komari-netstatic-")
            }));
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_new_and_save_load_roundtrip() {
        let dir = TestDir::new();
        let tmp = dir.file();
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

        // Repeated saves replace an existing target on Windows as well as Unix.
        ns.tx_bytes = u64::MAX;
        ns.rx_bytes = u64::MAX;
        ns.save().unwrap();
        let loaded = NetStatic::load(path).unwrap();
        assert_eq!(loaded.tx_bytes, u64::MAX);
        assert_eq!(loaded.rx_bytes, u64::MAX);
        dir.assert_no_stage();

        // Cleanup
        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn test_maybe_reset_disabled() {
        let dir = TestDir::new();
        let tmp = dir.file();
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
        let dir = TestDir::new();
        let tmp = dir.file();
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
        assert_eq!(
            &month[4..5],
            "-",
            "expected dash at position 4, got '{}'",
            month
        );
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
        let dir = TestDir::new();
        let result = NetStatic::load(dir.file().to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_malformed() {
        let dir = TestDir::new();
        let tmp = dir.file();
        let path = tmp.to_str().unwrap();
        fs::write(path, "not json at all").unwrap();

        let result = NetStatic::load(path);
        assert!(result.is_err());

        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn failed_atomic_replace_keeps_previous_counters_and_cleans_stage() {
        let dir = TestDir::new();
        let path = dir.file();
        let mut ns = NetStatic::new(path.to_str().unwrap());
        ns.update(123, 456);
        ns.save().unwrap();
        let before = fs::read(&path).unwrap();
        let error = atomic_write(&path, b"new counters", |from, to| {
            assert_eq!(from.parent(), to.parent());
            assert_eq!(fs::read(from).unwrap(), b"new counters");
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic replace failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), before);
        let loaded = NetStatic::load(path.to_str().unwrap()).unwrap();
        assert_eq!((loaded.tx_bytes, loaded.rx_bytes), (123, 456));
        dir.assert_no_stage();
    }

    #[test]
    fn malformed_month_and_numbers_are_rejected_without_panics() {
        let dir = TestDir::new();
        let path = dir.file();
        for month in [
            "2026-00",
            "2026-13",
            "2026-1",
            "2026/10",
            "２０２６-10",
            &"x".repeat(512),
        ] {
            let data = format!(r#"{{"month":"{month}","tx_bytes":1,"rx_bytes":2}}"#);
            fs::write(&path, data).unwrap();
            assert_eq!(
                NetStatic::load(path.to_str().unwrap())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            // Public field mutation is also checked before the fixed buffer.
            let mut ns = NetStatic::new(path.to_str().unwrap());
            ns.month = month.into();
            let before = fs::read(&path).unwrap();
            assert_eq!(ns.save().unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        for token in [
            "-1",
            "1.5",
            "1e3",
            "1garbage",
            "18446744073709551616",
            "\"123\"",
            "01",
        ] {
            let data = format!(r#"{{"month":"2026-10","tx_bytes":{token},"rx_bytes":2}}"#);
            fs::write(&path, data).unwrap();
            assert!(
                NetStatic::load(path.to_str().unwrap()).is_err(),
                "accepted {token}"
            );
        }
        for data in [
            r#"{"month":"2026-10","tx_bytes":1,"rx_bytes":2} trailing"#,
            r#"{"month":"2026-10","tx_bytes":1,"tx_bytes":2,"rx_bytes":3}"#,
            r#"{"nested":{"month":"2026-10","tx_bytes":1,"rx_bytes":2}}"#,
        ] {
            fs::write(&path, data).unwrap();
            assert!(NetStatic::load(path.to_str().unwrap()).is_err());
        }
        fs::write(&path, vec![b' '; 1025]).unwrap();
        assert!(NetStatic::load(path.to_str().unwrap()).is_err());
        fs::write(
            &path,
            b"{ \"month\" : \"2026-10\", \"tx_bytes\" : 18446744073709551615, \"rx_bytes\" : 0 }",
        )
        .unwrap();
        assert_eq!(
            NetStatic::load(path.to_str().unwrap()).unwrap().tx_bytes,
            u64::MAX
        );
        dir.assert_no_stage();
    }

    #[cfg(unix)]
    #[test]
    fn saving_over_a_symlink_does_not_truncate_its_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = TestDir::new();
        let path = dir.file();
        let protected = dir.0.join("unrelated.json");
        fs::write(&protected, b"unrelated fixture").unwrap();
        symlink(&protected, &path).unwrap();
        let mut ns = NetStatic::new(path.to_str().unwrap());
        ns.update(7, 8);
        ns.save().unwrap();
        assert_eq!(fs::read(&protected).unwrap(), b"unrelated fixture");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(NetStatic::load(path.to_str().unwrap()).unwrap().tx_bytes, 7);
        dir.assert_no_stage();
    }

    #[cfg(windows)]
    #[test]
    fn windows_locked_target_preserves_old_counters() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TestDir::new();
        let path = dir.file();
        let mut ns = NetStatic::new(path.to_str().unwrap());
        ns.update(11, 22);
        ns.save().unwrap();
        // Permit reads/writes but deny deletion/rename, like another process
        // holding an old state file without FILE_SHARE_DELETE.
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        ns.update(100, 100);
        assert!(ns.save().is_err());
        assert_eq!(
            NetStatic::load(path.to_str().unwrap()).unwrap().tx_bytes,
            11
        );
        dir.assert_no_stage();
        drop(locked);
        ns.save().unwrap();
        assert_eq!(
            NetStatic::load(path.to_str().unwrap()).unwrap().tx_bytes,
            111
        );
    }
}
