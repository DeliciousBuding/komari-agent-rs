//! Linux disk metrics from `/proc/mounts` and `statfs`.
//!
//! Parses `/proc/mounts`, filters virtual/temporary filesystems, calls
//! `statfs` on each remaining mountpoint, and returns per-filesystem
//! [`DiskInfo`] records plus aggregate totals.  Respects
//! `Config::include_mountpoints` / `Config::exclude_mountpoints`.

use std::ffi::CString;
use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::arena::{SmallVec, MAX_DISKS};
use crate::config::Config;

// ── statfs FFI (x86_64 Linux) ─────────────────────────────────────────────────

#[repr(C)]
struct StatFs {
    f_type: i64,
    f_bsize: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_namelen: i64,
    f_frsize: i64,
    f_flags: i64,
    f_spare: [i64; 4],
}

unsafe extern "C" {
    fn statfs(path: *const i8, buf: *mut StatFs) -> i32;
}

// ── DiskInfo ──────────────────────────────────────────────────────────────────

/// Per-mountpoint disk usage snapshot.
///
/// Mountpoint and filesystem type are stored inline (`[u8; 192]` and `[u8; 32]`)
/// so the struct is self-contained — no arena borrow needed.  Access them via
/// [`mountpoint()`](DiskInfo::mountpoint) / [`fs_type()`](DiskInfo::fs_type).
pub struct DiskInfo {
    mp_buf: [u8; 192],
    mp_len: u8,
    fs_buf: [u8; 32],
    fs_len: u8,
    /// Total capacity, bytes.
    pub total: u64,
    /// Used bytes (`total - free_for_unprivileged`).
    pub used: u64,
}

impl DiskInfo {
    /// Mountpoint path (e.g. `/`, `/home`).
    #[inline]
    pub fn mountpoint(&self) -> &str {
        std::str::from_utf8(&self.mp_buf[..self.mp_len as usize]).unwrap_or("?")
    }

    /// Filesystem type string (e.g. `ext4`, `xfs`, `btrfs`).
    #[inline]
    pub fn fs_type(&self) -> &str {
        std::str::from_utf8(&self.fs_buf[..self.fs_len as usize]).unwrap_or("?")
    }
}

// ── Filesystem type filter ────────────────────────────────────────────────────

/// Filesystem types excluded from disk accounting.
const EXCLUDED_FS: &[&str] = &[
    "tmpfs", "devtmpfs", "proc", "sysfs", "cgroup", "devpts", "debugfs",
    "securityfs", "pstore", "efivarfs", "configfs", "fusectl", "hugetlbfs",
    "mqueue", "binfmt_misc", "overlay", "squashfs", "autofs", "nfsd",
    "rpc_pipefs", "tracefs", "nsfs", "bpf", "cgroup2", "selinuxfs",
];

fn is_excluded_fs(fstype: &str) -> bool {
    EXCLUDED_FS.iter().any(|&ex| fstype == ex)
}

// ── Mountpoint match helper ───────────────────────────────────────────────────

/// True when `mp` starts with one of the given patterns.
fn matches_patterns(mp: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| mp.starts_with(p.as_str()))
}

// ── statfs helper ─────────────────────────────────────────────────────────────

/// Call `statfs` on `path` and return `(total_bytes, used_bytes)`, or `None`
/// on failure.
fn statfs_usage(path: &str) -> Option<(u64, u64)> {
    let cpath = CString::new(path).ok()?;
    let mut st: StatFs = unsafe { std::mem::zeroed() };
    if unsafe { statfs(cpath.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_bsize as u64;
    Some((st.f_blocks * bs, (st.f_blocks - st.f_bavail) * bs))
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Collect disk usage for all eligible mountpoints.
///
/// Reads `/proc/mounts`, skips virtual/temporary filesystems (see
/// [`EXCLUDED_FS`]), and respects `config.include_mountpoints` (whitelist
/// mode: only listed prefixes) and `config.exclude_mountpoints` (excluded
/// prefixes when no whitelist is set).
pub fn collect(config: &Config) -> SmallVec<DiskInfo, MAX_DISKS> {
    let mut out = SmallVec::new();

    let file = match File::open("/proc/mounts") {
        Ok(f) => f,
        Err(_) => return out,
    };

    let use_whitelist = !config.include_mountpoints.is_empty();

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let mut parts = line.split_whitespace();
        let mp = match parts.nth(1) {
            Some(v) => v,
            None => continue,
        };
        let fs = match parts.next() {
            Some(v) => v,
            None => continue,
        };

        if is_excluded_fs(fs) {
            continue;
        }
        if use_whitelist {
            if !matches_patterns(mp, &config.include_mountpoints) {
                continue;
            }
        } else if matches_patterns(mp, &config.exclude_mountpoints) {
            continue;
        }

        let (total, used) = match statfs_usage(mp) {
            Some(v) => v,
            None => continue,
        };

        // Copy mountpoint into inline storage.
        let mp_b = mp.as_bytes();
        let mp_n = mp_b.len().min(191);
        let mut mp_buf = [0u8; 192];
        mp_buf[..mp_n].copy_from_slice(&mp_b[..mp_n]);

        let fs_b = fs.as_bytes();
        let fs_n = fs_b.len().min(31);
        let mut fs_buf = [0u8; 32];
        fs_buf[..fs_n].copy_from_slice(&fs_b[..fs_n]);

        let _ = out.push(DiskInfo {
            mp_buf,
            mp_len: mp_n as u8,
            fs_buf,
            fs_len: fs_n as u8,
            total,
            used,
        });
    }

    out
}

/// Sum `total` and `used` across all [`DiskInfo`] entries.
pub fn aggregate(disks: &[DiskInfo]) -> (u64, u64) {
    let mut total = 0u64;
    let mut used = 0u64;
    for d in disks {
        total += d.total;
        used += d.used;
    }
    (total, used)
}
