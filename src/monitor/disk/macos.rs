// komari-agent-rs: macOS disk metrics — getmntinfo FFI.
#![cfg(target_os = "macos")]

use crate::arena::{SmallVec, MAX_DISKS};
use crate::config::Config;

// ── getmntinfo FFI (libSystem) ──────────────────────────────────────────────

const MNT_NOWAIT: i32 = 2;

// struct statfs64 — Darwin 64-bit layout
#[repr(C)]
struct StatFs {
    f_bsize: u32,       // offset 0
    f_iosize: i32,      // offset 4
    f_blocks: u64,      // offset 8
    f_bfree: u64,       // offset 16
    f_bavail: u64,      // offset 24
    f_files: u64,       // offset 32
    f_ffree: u64,       // offset 40
    f_fsid: [i32; 2],  // offset 48
    f_owner: u32,       // offset 56
    f_type: u32,        // offset 60
    f_flags: u32,       // offset 64
    f_fssubtype: u32,   // offset 68
    f_fstypename: [u8; 16], // offset 72
    f_mntonname: [u8; 1024], // offset 88
    f_mntfromname: [u8; 1024], // offset 1112
    f_flags_ext: u32,   // offset 2136
    f_owner_ext: u32,   // offset 2140
    // total size = 2168 on macOS 14 x86_64
    // On arm64 it's similar with some padding differences
}

unsafe extern "C" {
    fn getmntinfo(mntbufp: *mut *mut StatFs, flags: i32) -> i32;
}

// ── DiskInfo ────────────────────────────────────────────────────────────────

/// Per-mountpoint disk usage snapshot.
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
    #[inline]
    pub fn mountpoint(&self) -> &str {
        std::str::from_utf8(&self.mp_buf[..self.mp_len as usize]).unwrap_or("?")
    }

    #[inline]
    pub fn fs_type(&self) -> &str {
        std::str::from_utf8(&self.fs_buf[..self.fs_len as usize]).unwrap_or("?")
    }
}

// ── Filesystem type filter ──────────────────────────────────────────────────

/// Filesystem types excluded from disk accounting (virtual / synthetic).
const EXCLUDED_FS: &[&str] = &[
    "devfs", "autofs", "tmpfs", "fdesc", "nullfs", "unionfs",
    "ctfs", "deadfs", "specfs", "synthfs", "volfs", "lifs",
];

fn is_excluded_fs(fstype: &str) -> bool {
    EXCLUDED_FS.iter().any(|&ex| fstype == ex)
}

fn matches_patterns(mp: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| mp.starts_with(p.as_str()))
}

// ── collect ─────────────────────────────────────────────────────────────────

/// Collect disk usage for all eligible mountpoints via `getmntinfo`.
///
/// Skips virtual/synthetic filesystems and respects
/// `Config::include_mountpoints` / `Config::exclude_mountpoints`.
pub fn collect(config: &Config) -> SmallVec<DiskInfo, MAX_DISKS> {
    let mut out = SmallVec::new();

    let mut mntbuf: *mut StatFs = std::ptr::null_mut();
    let count = unsafe { getmntinfo(&mut mntbuf, MNT_NOWAIT) };
    if count <= 0 || mntbuf.is_null() {
        return out;
    }

    let use_whitelist = !config.include_mountpoints.is_empty();
    let mounts = unsafe { std::slice::from_raw_parts(mntbuf, count as usize) };

    for mnt in mounts {
        // Extract filesystem type as &str (null-terminated or 16-byte fixed)
        let fs_end = mnt.f_fstypename
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(mnt.f_fstypename.len());
        let fs_str = match std::str::from_utf8(&mnt.f_fstypename[..fs_end]) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if is_excluded_fs(fs_str) {
            continue;
        }

        // Extract mountpoint
        let mp_end = mnt.f_mntonname
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(mnt.f_mntonname.len());
        let mp_str = match std::str::from_utf8(&mnt.f_mntonname[..mp_end]) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if use_whitelist {
            if !matches_patterns(mp_str, &config.include_mountpoints) {
                continue;
            }
        } else if matches_patterns(mp_str, &config.exclude_mountpoints) {
            continue;
        }

        let bs = mnt.f_bsize as u64;
        let total = mnt.f_blocks.saturating_mul(bs);
        let used = (mnt.f_blocks - mnt.f_bavail).saturating_mul(bs);

        // Copy mountpoint into inline storage
        let mp_b = mp_str.as_bytes();
        let mp_n = mp_b.len().min(191);
        let mut mp_buf = [0u8; 192];
        mp_buf[..mp_n].copy_from_slice(&mp_b[..mp_n]);

        let fs_b = fs_str.as_bytes();
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
