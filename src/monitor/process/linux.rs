// komari-agent-rs: Linux process count — enumerate /proc/[0-9]+/ directories.
//
// Best-effort metric.  No /proc → Ok(0), not an error.
#![allow(dead_code)]

use std::fs;
use std::io;

/// Error type for process-count metric collection failures.
#[derive(Debug)]
pub enum MetricErr {
    /// An I/O error occurred while reading /proc.
    Io(io::Error),
}

impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

/// Count running processes by enumerating `/proc/[0-9]+/` directories.
///
/// Returns the number of numeric-named directories found under `/proc`.
/// When `/proc` is not mounted the function returns `Ok(0)` — a missing
/// procfs is not an error because this metric is best-effort.
pub fn collect_process_count() -> Result<u32, MetricErr> {
    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(MetricErr::Io(e)),
    };

    let mut count: u32 = 0;
    for entry in entries {
        let entry = entry?;
        // Only count entries whose name is purely ASCII digits.
        if !entry
            .file_name()
            .to_string_lossy()
            .as_bytes()
            .iter()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        // Defensive: verify it is a directory (proc entries always are).
        if entry.file_type()?.is_dir() {
            count += 1;
        }
    }
    Ok(count)
}
