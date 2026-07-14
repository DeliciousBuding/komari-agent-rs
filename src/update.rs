// komari-agent-rs: self-update via GitHub Releases.
// Feature-gated behind `self-update` — zero cost when disabled.
// DD: no serde, no tokio.  ring for SHA256 (project-approved crypto dep).
#![cfg(feature = "self-update")]

use crate::config::Config;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt, fs};

use rustls::pki_types::ServerName;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Error ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum UpdateErr {
    Http(String),
    Io(std::io::Error),
    Other(String),
}

impl fmt::Display for UpdateErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(s) => write!(f, "HTTP: {s}"),
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for UpdateErr {}
impl From<std::io::Error> for UpdateErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Check GitHub Releases for a newer version.  If one exists, download the
/// platform asset, verify its SHA256 hash, and atomically replace the running
/// binary.  Returns `true` when the caller should restart.
pub fn check_and_update(current_version: &str, config: &Config) -> Result<bool, UpdateErr> {
    if config.disable_auto_update {
        return Ok(false);
    }

    let tls =
        Arc::new(crate::tls::make_tls_config(config).map_err(|e| UpdateErr::Other(e.to_string()))?);

    let github_token = env::var("GITHUB_TOKEN").ok();

    // 1. GET https://api.github.com/repos/DeliciousBuding/komari-agent-rs/releases/latest
    let json = https_get(
        "api.github.com",
        "/repos/DeliciousBuding/komari-agent-rs/releases/latest",
        &tls,
        github_token.as_deref(),
    )?;

    // 2. Parse tag_name, compare semver (strip leading 'v')
    let tag = json_str(&json, "tag_name")
        .ok_or_else(|| UpdateErr::Other("missing 'tag_name' in release JSON".into()))?;
    let latest = tag.strip_prefix('v').unwrap_or(&tag);
    if !semver_gt(latest, current_version) {
        return Ok(false);
    }

    // 3. Find download URL for this platform's asset + SHA256SUMS
    let name = platform_asset();
    let dl_url =
        json_asset_url(&json, name).ok_or_else(|| UpdateErr::Other(format!("no asset: {name}")))?;
    let sums_url = json_asset_url(&json, "SHA256SUMS")
        .ok_or_else(|| UpdateErr::Other("no SHA256SUMS asset".into()))?;

    // 4. Download binary
    let bin = https_download(&dl_url, &tls, github_token.as_deref())?;

    // 5. Download SHA256SUMS, extract expected hash for our asset
    let sums = String::from_utf8(https_download(&sums_url, &tls, github_token.as_deref())?)
        .map_err(|_| UpdateErr::Other("SHA256SUMS is not UTF-8".into()))?;
    let expected = sums
        .lines()
        .find(|l| l.contains(name))
        .and_then(|l| l.split_whitespace().next())
        .ok_or_else(|| UpdateErr::Other(format!("{name} not in SHA256SUMS")))?;

    // 6. Verify SHA256 before replacing binary (CRITICAL)
    let actual = sha256_hex(&bin);
    if actual != expected {
        return Err(UpdateErr::Other(format!(
            "SHA256 mismatch: expected {expected}, computed {actual}"
        )));
    }

    // 7. Write to agent.new
    let exe = env::current_exe()?;
    let new = exe.with_extension("new");
    fs::write(&new, &bin)?;

    // 8. chmod +x on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    }

    // 9. Atomic rename: current → old, new → current
    let old = exe.with_extension("old");
    #[cfg(windows)]
    {
        // Windows: two-step rename instead of ReplaceFileW because a running
        // executable is locked and ReplaceFileW cannot overwrite it.  NTFS
        // permits renaming an in-use file, so we rename exe→old (still usable
        // by the OS) then new→exe.  The old binary is scheduled for deletion
        // on next reboot via MoveFileExW with MOVEFILE_DELAY_UNTIL_REBOOT.
        let _ = fs::remove_file(&old);
        fs::rename(&exe, &old)?;
        fs::rename(&new, &exe)?;
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = old
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x00000001;
        const MOVEFILE_DELAY_UNTIL_REBOOT: u32 = 0x00000004;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x00000008;
        unsafe {
            MoveFileExW(
                wide.as_ptr(),
                std::ptr::null(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_DELAY_UNTIL_REBOOT | MOVEFILE_WRITE_THROUGH,
            );
        }
    }
    #[cfg(not(windows))]
    {
        // Unix: atomic rename() within same filesystem
        fs::rename(&exe, &old)?;
        fs::rename(&new, &exe)?;
    }

    Ok(true)
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

fn https_get(host: &str, path: &str, tls: &Arc<rustls::ClientConfig>, auth_token: Option<&str>) -> Result<String, UpdateErr> {
    let body = https_fetch(host, 443, path, "", tls, auth_token)?;
    String::from_utf8(body).map_err(|_| UpdateErr::Http("non-UTF-8 response".into()))
}

fn https_download(url: &str, tls: &Arc<rustls::ClientConfig>, auth_token: Option<&str>) -> Result<Vec<u8>, UpdateErr> {
    let (host, port, path, query) = parse_https_url(url)?;
    https_fetch(&host, port, &path, &query, tls, auth_token)
}

fn https_fetch(
    host: &str,
    port: u16,
    path: &str,
    query: &str,
    tls: &Arc<rustls::ClientConfig>,
    auth_token: Option<&str>,
) -> Result<Vec<u8>, UpdateErr> {
    let addr = format!("{host}:{port}");
    let sock = addr
        .to_socket_addrs()
        .map_err(|e| UpdateErr::Http(e.to_string()))?
        .next()
        .ok_or_else(|| UpdateErr::Http("DNS returned no addresses".into()))?;

    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(60))
        .map_err(|e| UpdateErr::Http(e.to_string()))?;
    tcp.set_read_timeout(Some(Duration::from_secs(60))).ok();

    let server_name = ServerName::try_from(host)
        .map_err(|e| UpdateErr::Http(format!("SNI: {e}")))?
        .to_owned();
    let conn = rustls::ClientConnection::new(Arc::clone(tls), server_name)
        .map_err(|e| UpdateErr::Http(format!("TLS: {e}")))?;
    let mut stream = rustls::StreamOwned::new(conn, tcp);

    let req_path = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    let auth_header = match auth_token {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    write!(
        stream,
        "GET {req_path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: komari-agent-rs/{}\r\nAccept: */*\r\n{auth_header}Connection: close\r\n\r\n",
        CURRENT_VERSION
    )
    .map_err(|e| UpdateErr::Http(e.to_string()))?;
    stream.flush().map_err(|e| UpdateErr::Http(e.to_string()))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| UpdateErr::Http(e.to_string()))?;

    let sep = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| UpdateErr::Http("no header/body separator".into()))?;
    let head = &buf[..sep];
    if !(head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200"))
        && !head.starts_with(b"HTTP/1.1 302")
        && !head.starts_with(b"HTTP/2 200")
    {
        return Err(UpdateErr::Http(format!(
            "status: {}",
            String::from_utf8_lossy(&head[..head.len().min(64)])
        )));
    }

    Ok(buf[sep + 4..].to_vec())
}

fn parse_https_url(url: &str) -> Result<(String, u16, String, String), UpdateErr> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| UpdateErr::Http("URL must start with https://".into()))?;
    let (host_part, path_part) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };
    let (host, port) = match host_part.find(':') {
        Some(i) => {
            let p = host_part[i + 1..]
                .parse::<u16>()
                .map_err(|_| UpdateErr::Http(format!("bad port in: {url}")))?;
            (host_part[..i].to_string(), p)
        }
        None => (host_part.to_string(), 443),
    };
    let (path, query) = match path_part.find('?') {
        Some(i) => (path_part[..i].to_string(), path_part[i + 1..].to_string()),
        None => (path_part.to_string(), String::new()),
    };
    Ok((host, port, path, query))
}

// ── Minimal JSON extraction (no full parser — GitHub API responses are stable) ──

fn json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let end = json[start..].find('"')?;
    Some(json[start..][..end].to_string())
}

fn json_asset_url(json: &str, name: &str) -> Option<String> {
    let needle = format!("\"name\":\"{name}\"");
    let pos = json.find(&needle)?;
    let key = "\"browser_download_url\":\"";
    let start = json[pos + needle.len()..].find(key)? + key.len();
    let abs = pos + needle.len() + start;
    let end = json[abs..].find('"')?;
    Some(json[abs..][..end].to_string())
}

// ── Semver ──────────────────────────────────────────────────────────────────

fn semver_gt(a: &str, b: &str) -> bool {
    let p = |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse().ok()).collect() };
    let av = p(a);
    let bv = p(b);
    if av.is_empty() || bv.is_empty() {
        return false;
    }
    for i in 0..av.len().max(bv.len()) {
        match av.get(i).unwrap_or(&0).cmp(bv.get(i).unwrap_or(&0)) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            _ => {}
        }
    }
    false
}

// ── Platform detection ──────────────────────────────────────────────────────

fn platform_asset() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "komari-agent-rs-linux-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "komari-agent-rs-linux-arm64"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "komari-agent-rs-windows-x86_64.exe"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "komari-agent-rs-macos-x86_64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "komari-agent-rs-macos-arm64"
    } else if cfg!(all(target_os = "freebsd", target_arch = "x86_64")) {
        "komari-agent-rs-freebsd-x86_64"
    } else {
        "komari-agent-rs-unknown"
    }
}

// ── SHA256 (ring) ───────────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in digest.as_ref() {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// ── Windows FFI ─────────────────────────────────────────────────────────────

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    /// `BOOL MoveFileExW(LPCWSTR lpExisting, LPCWSTR lpNew, DWORD dwFlags)`
    /// Passing NULL for lpNew with MOVEFILE_DELAY_UNTIL_REBOOT schedules deletion.
    fn MoveFileExW(lpExisting: *const u16, lpNew: *const u16, dwFlags: u32) -> i32;
}
