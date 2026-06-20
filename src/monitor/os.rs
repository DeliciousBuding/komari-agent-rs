//! OS name and kernel version detection — Linux (Android/Proxmox/Synology/fnOS),
//! Windows (registry), macOS (sw_vers), FreeBSD (sysctl / freebsd-version).

/// Collected OS identity.
#[derive(Debug, Clone)]
pub struct OsInfo {
    pub name: String,
    pub kernel_version: String,
}

/// Collect OS name and kernel version for the current platform.
pub fn collect() -> OsInfo {
    #[cfg(target_os = "linux")]
    {
        return linux::collect();
    }
    #[cfg(windows)]
    {
        windows::collect()
    }
    #[cfg(target_os = "macos")]
    {
        return macos::collect();
    }
    #[cfg(target_os = "freebsd")]
    {
        return freebsd::collect();
    }
    #[cfg(not(any(
        target_os = "linux",
        windows,
        target_os = "macos",
        target_os = "freebsd"
    )))]
    {
        OsInfo {
            name: String::from("Unknown"),
            kernel_version: String::from("Unknown"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Linux
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
mod linux {
    use super::OsInfo;
    use std::fs;
    use std::io::BufRead;
    use std::process::Command;

    pub fn collect() -> OsInfo {
        let kv = kernel_version();
        // Priority: Android → Proxmox VE → Synology → fnOS → /etc/os-release
        if let Some(n) = detect_android() {
            return OsInfo {
                name: n,
                kernel_version: kv,
            };
        }
        if let Some(n) = detect_proxmox() {
            return OsInfo {
                name: n,
                kernel_version: kv,
            };
        }
        if let Some(n) = detect_synology() {
            return OsInfo {
                name: n,
                kernel_version: kv,
            };
        }
        if let Some(n) = detect_fnos() {
            return OsInfo {
                name: n,
                kernel_version: kv,
            };
        }
        let name = parse_os_release().unwrap_or_else(|| String::from("Linux"));
        OsInfo {
            name,
            kernel_version: kv,
        }
    }

    fn kernel_version() -> String {
        Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| String::from("Unknown"))
    }

    /// Parse /etc/os-release, preferring PRETTY_NAME, falling back to NAME + VERSION_ID.
    fn parse_os_release() -> Option<String> {
        let f = fs::File::open("/etc/os-release").ok()?;
        let mut pretty = None;
        let mut name = None;
        let mut version = None;
        for line in std::io::BufReader::new(f).lines().filter_map(|l| l.ok()) {
            if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                pretty = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("NAME=") {
                name = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                version = Some(v.trim_matches('"').to_string());
            }
        }
        if let Some(p) = pretty {
            return Some(p);
        }
        match (name, version) {
            (Some(n), Some(v)) => Some(format!("{} {}", n, v)),
            (Some(n), None) => Some(n),
            _ => None,
        }
    }

    /// Detect Android via `getprop` or `/system/build.prop` or directory heuristics.
    fn detect_android() -> Option<String> {
        // 1. getprop ro.build.version.release
        if let Ok(out) = Command::new("getprop")
            .arg("ro.build.version.release")
            .output()
        {
            if let Ok(ver) = String::from_utf8(out.stdout) {
                let ver = ver.trim().to_string();
                if !ver.is_empty() {
                    let model = cmd_out("getprop", &["ro.product.model"]);
                    let brand = cmd_out("getprop", &["ro.product.brand"]);
                    let mut s = format!("Android {}", ver);
                    if !model.is_empty() {
                        if !brand.is_empty() && brand != model {
                            s.push_str(&format!(" ({} {})", brand, model));
                        } else {
                            s.push_str(&format!(" ({})", model));
                        }
                    }
                    return Some(s);
                }
            }
        }
        // 2. /system/build.prop
        if let Ok(f) = fs::File::open("/system/build.prop") {
            let mut ver = String::new();
            for line in std::io::BufReader::new(f).lines().filter_map(|l| l.ok()) {
                if let Some(v) = line.strip_prefix("ro.build.version.release=") {
                    ver = v.to_string();
                    break;
                }
            }
            if !ver.is_empty() {
                return Some(format!("Android {}", ver));
            }
        }
        // 3. Directory heuristics
        let dirs = ["/system/app", "/system/priv-app", "/data/app", "/sdcard"];
        if dirs
            .iter()
            .filter(|d| fs::metadata(d).map(|m| m.is_dir()).unwrap_or(false))
            .count()
            >= 2
        {
            return Some(String::from("Android"));
        }
        None
    }

    /// Detect Proxmox VE via `pveversion`, extracting the pve-manager version line.
    fn detect_proxmox() -> Option<String> {
        let out = Command::new("pveversion").output().ok()?;
        let stdout = String::from_utf8(out.stdout).ok()?;
        let mut version = None;
        for line in stdout.lines() {
            if let Some(rest) = line.trim().strip_prefix("pve-manager/") {
                let v = rest.split('/').next().unwrap_or(rest);
                let v = v.split('~').next().unwrap_or(v);
                version = Some(v.to_string());
                break;
            }
        }
        let v = version?;
        // Try to read VERSION_CODENAME from /etc/os-release for codename
        let codename = fs::File::open("/etc/os-release").ok().and_then(|f| {
            for line in std::io::BufReader::new(f).lines().filter_map(|l| l.ok()) {
                if let Some(c) = line.strip_prefix("VERSION_CODENAME=") {
                    return Some(c.trim_matches('"').to_string());
                }
            }
            None
        });
        if let Some(c) = codename {
            Some(format!("Proxmox VE {} ({})", v, c))
        } else {
            Some(format!("Proxmox VE {}", v))
        }
    }

    /// Detect Synology DSM via /etc/synoinfo.conf or /usr/syno directory.
    fn detect_synology() -> Option<String> {
        for path in &["/etc/synoinfo.conf", "/etc.defaults/synoinfo.conf"] {
            if let Ok(f) = fs::File::open(path) {
                let mut unique = String::new();
                let mut dsm_ver = String::new();
                for line in std::io::BufReader::new(f).lines().filter_map(|l| l.ok()) {
                    if let Some(v) = line.trim().strip_prefix("unique=") {
                        unique = v.trim_matches('"').to_string();
                    } else if let Some(v) = line.trim().strip_prefix("udc_check_state=") {
                        dsm_ver = v.trim_matches('"').to_string();
                    }
                }
                if unique.contains("synology_") {
                    let model = unique
                        .rsplit('_')
                        .next()
                        .map(|m| m.to_uppercase())
                        .unwrap_or_default();
                    let dsm = if dsm_ver.is_empty() {
                        "DSM".to_string()
                    } else {
                        format!("DSM {}", dsm_ver)
                    };
                    return Some(format!("Synology {} {}", model, dsm));
                }
            }
        }
        if fs::metadata("/usr/syno")
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Some(String::from("Synology DSM"));
        }
        None
    }

    /// Detect fnOS (Feiniu OS) via /usr/trim/BUILD_VERSION or /usr/trim directory.
    fn detect_fnos() -> Option<String> {
        if let Ok(data) = fs::read_to_string("/usr/trim/BUILD_VERSION") {
            let v = data.trim().to_string();
            if !v.is_empty() {
                return Some(format!("fnOS {}", v));
            }
        }
        if fs::metadata("/usr/trim")
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Some(String::from("fnOS"));
        }
        None
    }

    /// Run a command and return trimmed stdout, or empty string on failure.
    fn cmd_out(cmd: &str, args: &[&str]) -> String {
        Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Windows
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(windows)]
mod windows {
    use super::OsInfo;

    // ── FFI: advapi32.dll ───────────────────────────────────────────────────

    type Hkey = isize;
    #[allow(non_upper_case_globals)]
    const HKEY_LOCAL_MACHINE: Hkey = 0x80000002;
    const KEY_READ: u32 = 0x20019;
    const ERROR_SUCCESS: i32 = 0;
    const REG_SZ: u32 = 1;

    unsafe extern "system" {
        fn RegOpenKeyExW(
            hKey: Hkey,
            lpSubKey: *const u16,
            ulOptions: u32,
            samDesired: u32,
            phkResult: *mut Hkey,
        ) -> i32;
        fn RegQueryValueExW(
            hKey: Hkey,
            lpValueName: *const u16,
            lpReserved: *const u8,
            lpType: *mut u32,
            lpData: *mut u8,
            lpcbData: *mut u32,
        ) -> i32;
        fn RegCloseKey(hKey: Hkey) -> i32;
    }

    /// Read a REG_SZ value from the given registry key.
    fn reg_get_string(hkey: Hkey, subkey: &str, value: &str) -> Option<String> {
        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hk: Hkey = 0;
        // SAFETY: FFI call with valid null-terminated wide string pointers.
        if unsafe { RegOpenKeyExW(hkey, subkey_wide.as_ptr(), 0, KEY_READ, &mut hk) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut data_type: u32 = 0;
        let mut buf_size: u32 = 0;
        // First call: get required buffer size
        // SAFETY: FFI call with valid handle and pointers.
        if unsafe {
            RegQueryValueExW(
                hk,
                value_wide.as_ptr(),
                std::ptr::null(),
                &mut data_type,
                std::ptr::null_mut(),
                &mut buf_size,
            )
        } != ERROR_SUCCESS
            || data_type != REG_SZ
        {
            unsafe { RegCloseKey(hk) };
            return None;
        }
        // Allocate buffer (buf_size includes null terminator)
        let mut buf: Vec<u16> = vec![0u16; (buf_size as usize).div_ceil(2)];
        let mut buf_bytes = buf_size;
        // SAFETY: FFI call with valid handle, type, and buffer pointers.
        if unsafe {
            RegQueryValueExW(
                hk,
                value_wide.as_ptr(),
                std::ptr::null(),
                &mut data_type,
                buf.as_mut_ptr() as *mut u8,
                &mut buf_bytes,
            )
        } != ERROR_SUCCESS
        {
            unsafe { RegCloseKey(hk) };
            return None;
        }
        unsafe { RegCloseKey(hk) };
        // Convert from UTF-16 (skip null terminator)
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    const REG_PATH: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    pub fn collect() -> OsInfo {
        let name = os_name();
        let kv = kernel_version();
        OsInfo {
            name,
            kernel_version: kv,
        }
    }

    fn os_name() -> String {
        let product = reg_get_string(HKEY_LOCAL_MACHINE, REG_PATH, "ProductName")
            .unwrap_or_else(|| String::from("Microsoft Windows"));

        // Server editions: return as-is
        if product.contains("Server") {
            return product;
        }
        // Already Windows 11: return as-is
        if product.contains("Windows 11") {
            return product;
        }

        // Check build number: >= 22000 → Windows 11
        if let Some(build_str) = reg_get_string(HKEY_LOCAL_MACHINE, REG_PATH, "CurrentBuild")
            && let Ok(build) = build_str.parse::<u32>()
            && build >= 22000
        {
            if let Some(edition) = product.strip_prefix("Windows 10 ") {
                return format!("Windows 11 {}", edition);
            }
            if product == "Windows 10" {
                return String::from("Windows 11");
            }
            return product.replace("Windows 10", "Windows 11");
        }
        product
    }

    fn kernel_version() -> String {
        let build = reg_get_string(HKEY_LOCAL_MACHINE, REG_PATH, "CurrentBuild");
        let ubr = reg_get_string(HKEY_LOCAL_MACHINE, REG_PATH, "UBR");
        match (build, ubr) {
            (Some(b), Some(u)) => format!("{}.{}", b, u),
            (Some(b), None) => b,
            _ => String::from("Unknown"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// macOS
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod macos {
    use super::OsInfo;
    use std::process::Command;

    pub fn collect() -> OsInfo {
        OsInfo {
            name: os_name(),
            kernel_version: kernel_version(),
        }
    }

    fn os_name() -> String {
        Command::new("sw_vers")
            .arg("-productName")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| String::from("macOS"))
    }

    fn kernel_version() -> String {
        Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| String::from("Unknown"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// FreeBSD
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "freebsd")]
mod freebsd {
    use super::OsInfo;
    use std::process::Command;

    pub fn collect() -> OsInfo {
        OsInfo {
            name: os_name(),
            kernel_version: kernel_version(),
        }
    }

    fn os_name() -> String {
        // Prefer freebsd-version for canonical version string
        if let Ok(out) = Command::new("freebsd-version").output() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let v = s.trim().to_string();
                if !v.is_empty() {
                    return format!("FreeBSD {}", v);
                }
            }
        }
        // Fallback: uname -sr (e.g. "FreeBSD 14.1-RELEASE")
        Command::new("uname")
            .arg("-sr")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| String::from("FreeBSD"))
    }

    fn kernel_version() -> String {
        Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| String::from("Unknown"))
    }
}
