// komari-agent-rs: Windows disk metrics — GetLogicalDriveStringsW + GetDiskFreeSpaceExW.
#![cfg(windows)]

use crate::arena::{SmallVec, MAX_DISKS};
use crate::config::Config;

/// Per-drive disk usage snapshot.
pub struct DiskInfo {
    mp_buf: [u8; 192],
    mp_len: u8,
    fs_buf: [u8; 32],
    fs_len: u8,
    pub total: u64,
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

// ── FFI: kernel32.dll ────────────────────────────────────────────────────────

unsafe extern "system" {
    fn GetLogicalDriveStringsW(nBufferLength: u32, lpBuffer: *mut u16) -> u32;
    fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        lpDirectoryName: *const u16,
        lpFreeBytesAvailableToCaller: *mut u64,
        lpTotalNumberOfBytes: *mut u64,
        lpTotalNumberOfFreeBytes: *mut u64,
    ) -> i32;
    fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: u32,
        lpVolumeSerialNumber: *mut u32,
        lpMaximumComponentLength: *mut u32,
        lpFileSystemFlags: *mut u32,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: u32,
    ) -> i32;
}

const DRIVE_FIXED: u32 = 3;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn get_fs_type(root: &str) -> String {
    let wide_root = to_wide(root);
    let mut fs_name_buf = [0u16; 32];
    let ret = unsafe {
        GetVolumeInformationW(
            wide_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name_buf.as_mut_ptr(),
            fs_name_buf.len() as u32,
        )
    };
    if ret == 0 {
        return "NTFS".to_string();
    }
    let end = fs_name_buf
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name_buf.len());
    String::from_utf16(&fs_name_buf[..end]).unwrap_or_else(|_| "NTFS".to_string())
}

pub fn collect(config: &Config) -> SmallVec<DiskInfo, MAX_DISKS> {
    let mut out = SmallVec::new();

    let mut buf = vec![0u16; 512];
    let len = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if len == 0 || len > buf.len() as u32 {
        return out;
    }

    let use_whitelist = !config.include_mountpoints.is_empty();
    let mut pos = 0usize;

    while pos < len as usize {
        let end = buf[pos..]
            .iter()
            .position(|&c| c == 0)
            .map(|p| pos + p)
            .unwrap_or(buf.len());
        if end <= pos {
            break;
        }

        let wide_drive = &buf[pos..end];
        let drive = String::from_utf16(wide_drive).unwrap_or_default();
        let drive_trimmed = drive.trim_end_matches('\\').to_string();

        let drive_type = unsafe { GetDriveTypeW(wide_drive.as_ptr()) };
        if drive_type != DRIVE_FIXED {
            pos = end + 1;
            continue;
        }

        let included = if use_whitelist {
            config
                .include_mountpoints
                .iter()
                .any(|p| drive_trimmed.starts_with(p.as_str()))
        } else {
            true
        };
        let excluded = config
            .exclude_mountpoints
            .iter()
            .any(|p| drive_trimmed.starts_with(p.as_str()));
        if !included || excluded {
            pos = end + 1;
            continue;
        }

        let mut free_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut _free_total: u64 = 0;
        let ret = unsafe {
            GetDiskFreeSpaceExW(
                wide_drive.as_ptr(),
                &mut free_available,
                &mut total_bytes,
                &mut _free_total,
            )
        };
        if ret == 0 {
            pos = end + 1;
            continue;
        }

        let used = total_bytes.saturating_sub(free_available);
        let fs_type = get_fs_type(&drive);

        let mp_b = drive_trimmed.as_bytes();
        let mp_n = mp_b.len().min(191);
        let mut mp_buf = [0u8; 192];
        mp_buf[..mp_n].copy_from_slice(&mp_b[..mp_n]);

        let fs_b = fs_type.as_bytes();
        let fs_n = fs_b.len().min(31);
        let mut fs_buf = [0u8; 32];
        fs_buf[..fs_n].copy_from_slice(&fs_b[..fs_n]);

        let _ = out.push(DiskInfo {
            mp_buf,
            mp_len: mp_n as u8,
            fs_buf,
            fs_len: fs_n as u8,
            total: total_bytes,
            used,
        });

        pos = end + 1;
    }

    out
}

pub fn aggregate(disks: &[DiskInfo]) -> (u64, u64) {
    let mut total = 0u64;
    let mut used = 0u64;
    for d in disks {
        total += d.total;
        used += d.used;
    }
    (total, used)
}
