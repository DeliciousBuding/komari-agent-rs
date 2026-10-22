//! Linux disk metrics from `/proc/mounts` and `statfs`.
//!
//! Parses `/proc/mounts`, filters virtual/temporary filesystems, calls
//! `statfs` on each remaining mountpoint, and returns per-filesystem
//! [`DiskInfo`] records plus aggregate totals.  Respects
//! `Config::include_mountpoints` / `Config::exclude_mountpoints`.

// DiskInfo exposes its inline buffers through public accessors; the raw fields
// are part of the self-contained struct layout and not read directly.
#![allow(dead_code)]

use std::ffi::CString;
use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::arena::{MAX_DISKS, SmallVec};
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
    fn statfs(path: *const core::ffi::c_char, buf: *mut StatFs) -> i32;
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
    "tmpfs",
    "devtmpfs",
    "proc",
    "sysfs",
    "cgroup",
    "devpts",
    "debugfs",
    "securityfs",
    "pstore",
    "efivarfs",
    "configfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "binfmt_misc",
    "overlay",
    "squashfs",
    "autofs",
    "nfsd",
    "rpc_pipefs",
    "tracefs",
    "nsfs",
    "bpf",
    "cgroup2",
    "selinuxfs",
    "nfs",
    "cifs",
    "smb",
    "vboxsf",
    "9p",
    "fuse",
    "nullfs",
];

fn is_excluded_fs(fstype: &str) -> bool {
    // fuseblk (ntfs-3g) is a physical disk — always include.
    if fstype == "fuseblk" {
        return false;
    }
    EXCLUDED_FS.iter().any(|prefix| fstype.starts_with(prefix))
}

fn is_physical_mount(dev: &str, mp: &str, fs: &str) -> bool {
    // Root can itself be a loop/overlay filesystem (LXC and containers).
    if mp == "/" {
        return true;
    }
    if is_excluded_mountpoint(mp) {
        return false;
    }
    if dev == "/dev/fuse" && fs == "fuse" && mp == "/sdcard" {
        return true;
    }
    !dev.starts_with("/dev/loop") && !is_excluded_fs(fs)
}

// ── Mountpoint prefix exclusion ───────────────────────────────────────────────

/// Mountpoint prefixes that should be excluded from disk accounting.
const MOUNTPOINT_EXCLUDE_PREFIXES: &[&str] = &[
    "/tmp",
    "/var/tmp",
    "/dev",
    "/run",
    "/var/lib/containers",
    "/var/lib/docker",
    "/proc",
    "/sys",
    "/sys/fs/cgroup",
    "/etc/resolv.conf",
    "/etc/host",
    "/nix/store",
];

fn is_excluded_mountpoint(mp: &str) -> bool {
    if mp == "/" {
        return false; // always include root
    }
    MOUNTPOINT_EXCLUDE_PREFIXES
        .iter()
        .any(|p| mp.starts_with(p))
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
    let file = match File::open("/proc/mounts") {
        Ok(f) => f,
        Err(_) => return SmallVec::new(),
    };
    collect_mounts(BufReader::new(file), config, statfs_usage)
}

fn collect_mounts(
    mounts: impl BufRead,
    config: &Config,
    mut usage: impl FnMut(&str) -> Option<(u64, u64)>,
) -> SmallVec<DiskInfo, MAX_DISKS> {
    let mut out: SmallVec<DiskInfo, MAX_DISKS> = SmallVec::new();
    let use_whitelist = !config.include_mountpoints.is_empty();
    // Identity, not equal usage values, determines whether mounts share a disk.
    // Keep keys aligned with the bounded output, including when it is full.
    let mut devices: Vec<(bool, String)> = Vec::new();
    for line in mounts.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let mut parts = line.split_whitespace();
        let dev = match parts.next() {
            Some(v) => v,
            None => continue,
        };
        let mp = match parts.next() {
            Some(v) => v,
            None => continue,
        };
        let fs = match parts.next() {
            Some(v) => v,
            None => continue,
        };

        if !is_physical_mount(dev, mp, fs) {
            continue;
        }

        if use_whitelist {
            if !matches_patterns(mp, &config.include_mountpoints) {
                continue;
            }
        } else if matches_patterns(mp, &config.exclude_mountpoints) {
            continue;
        }

        let (total, used) = match usage(mp) {
            Some(v) => v,
            None => continue,
        };
        let is_zfs = fs == "zfs";
        let device = if is_zfs {
            dev.split('/').next().unwrap_or(dev)
        } else {
            dev
        };
        let existing = devices
            .iter()
            .position(|(zfs, key)| *zfs == is_zfs && key == device);
        // A quota-limited dataset or bind mount must not hide the full disk.
        if existing.is_some_and(|index| out[index].total >= total) {
            continue;
        }

        // Copy mountpoint into inline storage.
        let mp_b = mp.as_bytes();
        let mp_n = mp_b.len().min(191);
        let mut mp_buf = [0u8; 192];
        mp_buf[..mp_n].copy_from_slice(&mp_b[..mp_n]);

        let fs_b = fs.as_bytes();
        let fs_n = fs_b.len().min(31);
        let mut fs_buf = [0u8; 32];
        fs_buf[..fs_n].copy_from_slice(&fs_b[..fs_n]);

        let disk = DiskInfo {
            mp_buf,
            mp_len: mp_n as u8,
            fs_buf,
            fs_len: fs_n as u8,
            total,
            used,
        };
        if let Some(index) = existing {
            out[index] = disk;
        } else if out.push(disk).is_ok() {
            devices.push((is_zfs, device.to_string()));
        }
    }

    out
}

/// Sum the already device-deduplicated [`DiskInfo`] entries from [`collect`].
pub fn aggregate(disks: &[DiskInfo]) -> (u64, u64) {
    let mut total = 0u64;
    let mut used = 0u64;
    for d in disks {
        total += d.total;
        used += d.used;
    }
    (total, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(mp: &str, fs: &str, total: u64, used: u64) -> DiskInfo {
        let mut mp_buf = [0u8; 192];
        let mp_b = mp.as_bytes();
        mp_buf[..mp_b.len()].copy_from_slice(mp_b);

        let mut fs_buf = [0u8; 32];
        let fs_b = fs.as_bytes();
        fs_buf[..fs_b.len()].copy_from_slice(fs_b);

        DiskInfo {
            mp_buf,
            mp_len: mp_b.len() as u8,
            fs_buf,
            fs_len: fs_b.len() as u8,
            total,
            used,
        }
    }

    #[test]
    fn aggregate_preserves_distinct_disks_with_equal_usage() {
        let disks = [
            disk("/", "ext4", 31, 12),
            disk("/data", "ext4", 31, 12),
            disk("/boot/efi", "vfat", 1, 0),
        ];

        assert_eq!(aggregate(&disks), (63, 24));
    }

    #[test]
    fn physical_mount_filter_keeps_root_and_local_fuse_exceptions() {
        assert!(is_physical_mount("overlay", "/", "overlay"));
        assert!(is_physical_mount("/dev/loop0", "/", "ext4"));
        assert!(!is_physical_mount("/dev/loop0", "/snap/app", "ext4"));
        assert!(is_physical_mount("/dev/fuse", "/sdcard", "fuse"));
        assert!(is_physical_mount("/dev/sda1", "/media/ntfs", "fuseblk"));
        for fs in ["nfs", "nfs4", "cifs", "smb3", "fuse.sshfs", "nullfs"] {
            assert!(!is_physical_mount("remote", "/data", fs), "{fs}");
        }
        assert!(!is_physical_mount("/dev/fuse", "/other", "fuse"));
    }

    #[test]
    fn mounts_deduplicate_devices_and_choose_largest_zfs_quota() {
        let mounts = b"/dev/sda1 / ext4 rw 0 0\n/dev/sda1 /data ext4 rw 0 0\npool/small /small zfs rw 0 0\npool/full /full zfs rw 0 0\npool/other /other zfs rw 0 0\n/dev/sdb1 /independent ext4 rw 0 0\n";
        let disks = collect_mounts(&mounts[..], &Config::default(), |mp| {
            Some(match mp {
                "/small" => (10, 3),
                "/full" => (100, 30),
                "/other" => (20, 5),
                _ => (50, 15),
            })
        });
        assert_eq!(disks.len(), 3);
        assert_eq!(aggregate(&disks), (200, 60));
        assert_eq!(disks[1].mountpoint(), "/full");
    }

    #[test]
    fn mount_filters_run_before_device_deduplication() {
        let mounts = b"/dev/sda1 /ignored ext4 rw 0 0\n/dev/sda1 /selected ext4 rw 0 0\n/dev/sdb1 /failed ext4 rw 0 0\n/dev/sdb1 /working ext4 rw 0 0\n";
        let mut config = Config::default();
        config.exclude_mountpoints = vec!["/ignored".into()];
        let disks = collect_mounts(&mounts[..], &config, |mp| {
            (mp != "/failed").then_some((100, 40))
        });
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].mountpoint(), "/selected");
        assert_eq!(disks[1].mountpoint(), "/working");
        config.include_mountpoints = vec!["/ignored".into()];
        let disks = collect_mounts(&mounts[..], &config, |_| Some((100, 40)));
        assert_eq!(disks.len(), 1, "whitelist retains precedence over excludes");
        assert_eq!(disks[0].mountpoint(), "/ignored");
    }
}
