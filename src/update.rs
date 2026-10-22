// Optional synchronous self-update through GitHub Releases; no extra runtime.
#![cfg(feature = "self-update")]

use crate::config::Config;
use crate::json::{decode_json_string, object_field, skip_value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt, fs};

use rustls::pki_types::ServerName;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECKSUMS_ASSET: &str = "checksums.txt";
const MAX_JSON: usize = 1024 * 1024;
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_BINARY: usize = 8 * 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug)]
pub enum UpdateErr {
    Http(String),
    Io(io::Error),
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
impl From<io::Error> for UpdateErr {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Verify and install a newer release, retaining the previous executable in
/// a sibling `.old` file. On Unix, re-exec with the original arguments and
/// environment; exec failure rolls the installed path back and returns Err.
/// On Windows, return true only to mean *installed for the next launch*: the
/// running process remains the old version until its supervisor restarts it.
pub fn check_and_update(current_version: &str, config: &Config) -> Result<bool, UpdateErr> {
    if config.disable_auto_update {
        return Ok(false);
    }

    // A monitoring endpoint's unsafe-cert opt-out must never authorize an
    // unverified executable download. The manifest and binary share TLS trust;
    // checksums detect corruption, not a compromised release publisher.
    let mut secure_config = config.clone();
    secure_config.ignore_unsafe_cert = false;
    let tls = Arc::new(
        crate::tls::make_tls_config(&secure_config).map_err(|e| UpdateErr::Other(e.to_string()))?,
    );
    let token = env::var("GITHUB_TOKEN").ok();
    let json = String::from_utf8(https_download(
        "https://api.github.com/repos/DeliciousBuding/komari-agent-rs/releases/latest",
        &tls,
        token.as_deref(),
        MAX_JSON,
    )?)
    .map_err(|_| UpdateErr::Http("release JSON is not UTF-8".into()))?;
    let tag = json_str(&json, "tag_name")
        .ok_or_else(|| UpdateErr::Other("missing tag_name in release JSON".into()))?;
    if !semver_gt(tag.strip_prefix('v').unwrap_or(&tag), current_version) {
        return Ok(false);
    }

    let name = platform_asset();
    let dl_url =
        json_asset_url(&json, name).ok_or_else(|| UpdateErr::Other(format!("no asset: {name}")))?;
    let sums_url = json_asset_url(&json, CHECKSUMS_ASSET)
        .ok_or_else(|| UpdateErr::Other(format!("no {CHECKSUMS_ASSET} asset")))?;
    let sums = String::from_utf8(https_download(
        &sums_url,
        &tls,
        token.as_deref(),
        MAX_MANIFEST,
    )?)
    .map_err(|_| UpdateErr::Other("checksums.txt is not UTF-8".into()))?;
    let expected = find_checksum(&sums, name)
        .ok_or_else(|| UpdateErr::Other(format!("missing or invalid checksum for {name}")))?;
    let bin = https_download(&dl_url, &tls, token.as_deref(), MAX_BINARY)?;
    let exe = env::current_exe()?;
    let old = install_verified_binary(&exe, &bin, expected)?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(&exe)
            .args(env::args_os().skip(1))
            .exec();
        Err(restore_after_failed_exec(&exe, &old, error))
    }
    #[cfg(not(unix))]
    {
        let _ = old;
        eprintln!("[komari] new executable installed; current process unchanged, restart required");
        Ok(true)
    }
}

/// Install a stable release selected by an authenticated server request.
/// Accepted selectors are `latest`, an exact semver, or a major/minor line
/// such as `1`, `1.*`, `1.2`, or `1.2.*`.
pub fn check_and_update_target(
    current_version: &str,
    config: &Config,
    target: &str,
) -> Result<bool, UpdateErr> {
    let target = target.trim().trim_start_matches(['v', 'V']);
    let current_version = current_version.trim().trim_start_matches(['v', 'V']);
    if target.is_empty() {
        return Err(UpdateErr::Other("empty requested version".into()));
    }
    if parse_semver(target).is_some() && target == current_version {
        return Ok(false);
    }
    let mut secure_config = config.clone();
    secure_config.ignore_unsafe_cert = false;
    let tls = Arc::new(
        crate::tls::make_tls_config(&secure_config).map_err(|e| UpdateErr::Other(e.to_string()))?,
    );
    let token = env::var("GITHUB_TOKEN").ok();
    let json = String::from_utf8(https_download(
        "https://api.github.com/repos/DeliciousBuding/komari-agent-rs/releases?per_page=100",
        &tls,
        token.as_deref(),
        MAX_JSON,
    )?)
    .map_err(|_| UpdateErr::Http("release JSON is not UTF-8".into()))?;
    let (version, asset_url, checksums_url) = select_release(&json, target)
        .ok_or_else(|| UpdateErr::Other("no matching stable release".into()))?;
    if version == current_version {
        return Ok(false);
    }
    install_release_urls(&tls, token.as_deref(), &asset_url, &checksums_url)
}

fn install_release_urls(
    tls: &Arc<rustls::ClientConfig>,
    token: Option<&str>,
    asset_url: &str,
    checksums_url: &str,
) -> Result<bool, UpdateErr> {
    let name = platform_asset();
    let sums = String::from_utf8(https_download(checksums_url, tls, token, MAX_MANIFEST)?)
        .map_err(|_| UpdateErr::Other("checksums.txt is not UTF-8".into()))?;
    let expected = find_checksum(&sums, name)
        .ok_or_else(|| UpdateErr::Other(format!("missing or invalid checksum for {name}")))?;
    let bin = https_download(asset_url, tls, token, MAX_BINARY)?;
    let exe = env::current_exe()?;
    let old = install_verified_binary(&exe, &bin, expected)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(&exe)
            .args(env::args_os().skip(1))
            .exec();
        Err(restore_after_failed_exec(&exe, &old, error))
    }
    #[cfg(not(unix))]
    {
        let _ = old;
        Ok(true)
    }
}

/// Verify before creating any files. A staging collision fails closed rather
/// than following a stale file/symlink; the previous successful backup remains
/// available until another fully verified update is ready to install.
fn install_verified_binary(exe: &Path, bin: &[u8], expected: &str) -> Result<PathBuf, UpdateErr> {
    if bin.is_empty() || !sha256_hex(bin).eq_ignore_ascii_case(expected) {
        return Err(UpdateErr::Other("empty binary or SHA256 mismatch".into()));
    }
    let new = sidecar(exe, ".new");
    let old = sidecar(exe, ".old");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&new)?;
    let result = (|| -> Result<(), UpdateErr> {
        file.write_all(bin)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        // Close before rename: Windows does not allow renaming this open file.
        drop(file);
        match fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        #[cfg(unix)]
        {
            // Keep the installed path continuously present: backup the inode,
            // then atomically replace it within the same directory/filesystem.
            fs::hard_link(exe, &old)?;
            fs::rename(&new, exe)?;
        }
        #[cfg(not(unix))]
        replace_with_rollback(exe, &new, &old, |a, b| fs::rename(a, b))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&new);
    }
    result?;
    Ok(old)
}

fn sidecar(exe: &Path, suffix: &str) -> PathBuf {
    let mut path = exe.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

#[cfg(any(unix, test))]
fn restore_after_failed_exec(exe: &Path, old: &Path, error: io::Error) -> UpdateErr {
    if fs::rename(old, exe).is_err() {
        UpdateErr::Other("re-exec and rollback failed; previous executable retained in .old".into())
    } else {
        UpdateErr::Other(format!("re-exec failed; rolled back: {error}"))
    }
}

#[cfg(any(not(unix), test))]
fn replace_with_rollback(
    exe: &Path,
    new: &Path,
    old: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> Result<(), UpdateErr> {
    // Windows permits renaming a running image but not overwriting it. Never
    // discard the original if installing the staged executable fails.
    rename(exe, old)?;
    if let Err(error) = rename(new, exe) {
        if rename(old, exe).is_err() {
            return Err(UpdateErr::Other(
                "install and rollback failed; previous executable retained in .old".into(),
            ));
        }
        return Err(error.into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HttpsUrl {
    host: String,
    port: u16,
    target: String,
}

impl HttpsUrl {
    fn parse(url: &str) -> Result<Self, UpdateErr> {
        // No URL/token contents in errors: signed asset query strings are secret.
        if url.bytes().any(|b| b <= b' ' || b == 127 || b == b'\\') {
            return Err(UpdateErr::Http("invalid URL characters".into()));
        }
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| UpdateErr::Http("only HTTPS URLs are allowed".into()))?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let (host, port) = match authority.split_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| UpdateErr::Http("invalid HTTPS port".into()))?,
            ),
            None => (authority, 443),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(UpdateErr::Http("invalid HTTPS host".into()));
        }
        let suffix = rest[end..].split('#').next().unwrap_or("");
        let target = if suffix.starts_with('/') {
            suffix.to_string()
        } else {
            format!("/{suffix}")
        };
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
            target,
        })
    }

    fn same_origin(&self, other: &Self) -> bool {
        self.host == other.host && self.port == other.port
    }

    fn github_auth(&self) -> bool {
        self.port == 443 && matches!(self.host.as_str(), "api.github.com" | "github.com")
    }

    fn redirect(&self, location: &str) -> Result<Self, UpdateErr> {
        if location.starts_with("https://") {
            return Self::parse(location);
        }
        if location.starts_with("//") {
            return Self::parse(&format!("https:{location}"));
        }
        // A colon before the first slash denotes an absolute URI scheme.
        if location
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .contains(':')
        {
            return Err(UpdateErr::Http("redirect must use HTTPS".into()));
        }
        let location = location.split('#').next().unwrap_or("");
        let path = self.target.split('?').next().unwrap_or("/");
        let target = if location.is_empty() {
            self.target.clone()
        } else if location.starts_with('?') {
            format!("{path}{location}")
        } else if location.starts_with('/') {
            location.to_string()
        } else {
            let base = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            format!("{base}/{location}")
        };
        // RFC 3986 dot-segment resolution (do not decode signed/escaped paths).
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let mut segments = Vec::new();
        for part in path.split('/').skip(1) {
            match part {
                "." => {}
                ".." => {
                    segments.pop();
                }
                _ => segments.push(part),
            }
        }
        let mut path = format!("/{}", segments.join("/"));
        if (target.split('?').next().unwrap_or("").ends_with("/.")
            || target.split('?').next().unwrap_or("").ends_with("/.."))
            && !path.ends_with('/')
        {
            path.push('/');
        }
        if target.contains('?') {
            path.push('?');
            path.push_str(query);
        }
        Self::parse(&format!("https://{}:{}{path}", self.host, self.port))
    }
}

enum Response {
    Redirect(String),
    Body(Vec<u8>),
}

fn follow_redirects(
    url: &str,
    token: Option<&str>,
    mut fetch: impl FnMut(&HttpsUrl, Option<&str>) -> Result<Response, UpdateErr>,
) -> Result<Vec<u8>, UpdateErr> {
    let mut url = HttpsUrl::parse(url)?;
    let mut auth = token.filter(|_| url.github_auth());
    if auth.is_some_and(|s| s.bytes().any(|b| b <= b' ' || b >= 127)) {
        return Err(UpdateErr::Http("invalid authentication token".into()));
    }
    let mut visited = vec![url.clone()];
    for hop in 0..=MAX_REDIRECTS {
        match fetch(&url, auth)? {
            Response::Body(body) => return Ok(body),
            Response::Redirect(location) => {
                if hop == MAX_REDIRECTS {
                    return Err(UpdateErr::Http("redirect limit exceeded".into()));
                }
                let next = url.redirect(&location)?;
                if visited.contains(&next) {
                    return Err(UpdateErr::Http("redirect loop".into()));
                }
                // Once stripped, credentials remain stripped even on a return
                // to the original host. Never authorize an asset CDN or mirror.
                if !url.same_origin(&next) {
                    auth = None;
                }
                visited.push(next.clone());
                url = next;
            }
        }
    }
    unreachable!()
}

fn https_download(
    url: &str,
    tls: &Arc<rustls::ClientConfig>,
    token: Option<&str>,
    limit: usize,
) -> Result<Vec<u8>, UpdateErr> {
    follow_redirects(url, token, |url, auth| https_fetch(url, tls, auth, limit))
}

fn https_fetch(
    url: &HttpsUrl,
    tls: &Arc<rustls::ClientConfig>,
    token: Option<&str>,
    limit: usize,
) -> Result<Response, UpdateErr> {
    let sock = (url.host.as_str(), url.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| UpdateErr::Http("DNS returned no addresses".into()))?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(30))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
    let server_name = ServerName::try_from(url.host.clone())
        .map_err(|_| UpdateErr::Http("invalid SNI hostname".into()))?;
    let conn = rustls::ClientConnection::new(Arc::clone(tls), server_name)
        .map_err(|e| UpdateErr::Http(format!("TLS: {e}")))?;
    let mut stream = rustls::StreamOwned::new(conn, tcp);
    let host = if url.port == 443 {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: komari-agent-rs/{CURRENT_VERSION}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        url.target
    )?;
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.flush()?;
    read_response(&mut BufReader::new(stream), limit)
}

fn limited_line(reader: &mut impl BufRead, remaining: &mut usize) -> Result<String, UpdateErr> {
    let mut line = Vec::new();
    (&mut *reader)
        .take((*remaining + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.len() > *remaining || !line.ends_with(b"\r\n") {
        return Err(UpdateErr::Http("invalid or oversized HTTP headers".into()));
    }
    *remaining -= line.len();
    line.truncate(line.len() - 2);
    String::from_utf8(line).map_err(|_| UpdateErr::Http("invalid HTTP header encoding".into()))
}

fn read_response(reader: &mut impl BufRead, limit: usize) -> Result<Response, UpdateErr> {
    let mut remaining = MAX_HEADERS;
    let status = limited_line(reader, &mut remaining)?;
    let mut parts = status.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
        return Err(UpdateErr::Http("invalid HTTP version".into()));
    }
    let status: u16 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| UpdateErr::Http("invalid HTTP status".into()))?;
    let mut length = None;
    let mut location = None;
    let mut chunked = false;
    loop {
        let line = limited_line(reader, &mut remaining)?;
        if line.is_empty() {
            break;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| UpdateErr::Http("malformed HTTP header".into()))?;
        let value = value.trim();
        if key.eq_ignore_ascii_case("location") {
            if location.replace(value.to_string()).is_some() {
                return Err(UpdateErr::Http("duplicate redirect location".into()));
            }
        } else if key.eq_ignore_ascii_case("content-length") {
            let size = value
                .parse::<usize>()
                .map_err(|_| UpdateErr::Http("invalid content length".into()))?;
            if length.replace(size).is_some() || size > limit {
                return Err(UpdateErr::Http(
                    "duplicate or oversized content length".into(),
                ));
            }
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(UpdateErr::Http("unsupported transfer encoding".into()));
            }
            chunked = true;
        } else if key.eq_ignore_ascii_case("content-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err(UpdateErr::Http("unsupported content encoding".into()));
        }
    }
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        return location
            .map(Response::Redirect)
            .ok_or_else(|| UpdateErr::Http("redirect without location".into()));
    }
    if status != 200 {
        return Err(UpdateErr::Http(format!("status {status}")));
    }
    if chunked && length.is_some() {
        return Err(UpdateErr::Http("ambiguous HTTP body framing".into()));
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            // Bound framing bytes too, so tiny chunks with huge extensions
            // cannot evade the response size budget.
            let line = limited_line(reader, &mut remaining)?;
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| UpdateErr::Http("invalid chunk size".into()))?;
            if size == 0 {
                while !limited_line(reader, &mut remaining)?.is_empty() {}
                break;
            }
            if size > limit - body.len() {
                return Err(UpdateErr::Http("body limit exceeded".into()));
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            let mut ending = [0; 2];
            reader.read_exact(&mut ending)?;
            if ending != *b"\r\n" {
                return Err(UpdateErr::Http("invalid chunk ending".into()));
            }
            remaining = remaining
                .checked_sub(2)
                .ok_or_else(|| UpdateErr::Http("chunk framing limit exceeded".into()))?;
        }
    } else if let Some(size) = length {
        body.resize(size, 0);
        reader.read_exact(&mut body)?;
    } else {
        reader.take((limit + 1) as u64).read_to_end(&mut body)?;
        if body.len() > limit {
            return Err(UpdateErr::Http("body limit exceeded".into()));
        }
    }
    Ok(Response::Body(body))
}

fn json_str(json: &str, key: &str) -> Option<String> {
    decode_json_string(object_field(json.as_bytes(), key)?)
}

fn json_asset_url(json: &str, name: &str) -> Option<String> {
    let assets = object_field(json.as_bytes(), "assets")?;
    let assets = assets.strip_prefix(b"[")?.strip_suffix(b"]")?;
    let mut pos = 0;
    let mut found = None;
    loop {
        while assets.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if pos == assets.len() {
            return found;
        }
        let end = skip_value(assets, pos);
        if end <= pos {
            return None;
        }
        let asset = assets.get(pos..end)?;
        if !asset.starts_with(b"{") || !asset.ends_with(b"}") {
            return None;
        }
        let asset_name = decode_json_string(object_field(asset, "name")?)?;
        if asset_name == name {
            if found.is_some() {
                return None;
            }
            found = Some(decode_json_string(object_field(
                asset,
                "browser_download_url",
            )?)?);
        }
        pos = end;
        while assets.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if pos == assets.len() {
            return found;
        }
        match assets.get(pos)? {
            b',' => {
                pos += 1;
                if assets[pos..].iter().all(u8::is_ascii_whitespace) {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

fn select_release(json: &str, target: &str) -> Option<(String, String, String)> {
    let array = json.as_bytes().trim_ascii();
    let members = array.strip_prefix(b"[")?.strip_suffix(b"]")?;
    let mut pos = 0;
    let mut selected: Option<([u64; 3], String, String, String)> = None;
    while pos < members.len() {
        while members
            .get(pos)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b',')
        {
            pos += 1;
        }
        if pos == members.len() {
            break;
        }
        let end = skip_value(members, pos);
        if end <= pos {
            return None;
        }
        let release = std::str::from_utf8(members.get(pos..end)?).ok()?;
        let prerelease = object_field(release.as_bytes(), "prerelease") == Some(b"true".as_slice());
        let draft = object_field(release.as_bytes(), "draft") == Some(b"true".as_slice());
        if !prerelease && !draft {
            let tag = json_str(release, "tag_name")?;
            let version = tag.strip_prefix(['v', 'V']).unwrap_or(&tag);
            if let Some(key) = parse_semver(version)
                && version_matches_target(version, key, target)
            {
                let name = platform_asset();
                if let (Some(asset), Some(sums)) = (
                    json_asset_url(release, name),
                    json_asset_url(release, CHECKSUMS_ASSET),
                ) && selected.as_ref().is_none_or(|current| key > current.0)
                {
                    selected = Some((key, version.to_string(), asset, sums));
                }
            }
        }
        pos = end;
    }
    selected.map(|(_, version, asset, sums)| (version, asset, sums))
}

fn parse_semver(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.split('.');
    let mut parsed = [0; 3];
    for out in &mut parsed {
        let part = parts.next()?;
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        *out = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(parsed)
}

fn version_matches_target(version: &str, parsed: [u64; 3], target: &str) -> bool {
    if target.eq_ignore_ascii_case("latest") {
        return true;
    }
    let prefix = target.strip_suffix(".*").unwrap_or(target);
    let fields: Vec<_> = prefix.split('.').collect();
    match fields.as_slice() {
        [major] => major.parse::<u64>().ok() == Some(parsed[0]),
        [major, minor] => {
            major.parse::<u64>().ok() == Some(parsed[0])
                && minor.parse::<u64>().ok() == Some(parsed[1])
        }
        [_, _, _] => version == target,
        _ => false,
    }
}

fn semver_gt(a: &str, b: &str) -> bool {
    matches!((parse_semver(a), parse_semver(b)), (Some(a), Some(b)) if a > b)
}

// Release naming and exact checksum matching adapted from community PR #106
// by Hongquan Zou. self-update binaries on Linux are the full variant.
fn platform_asset() -> &'static str {
    platform_asset_for(env::consts::OS, env::consts::ARCH)
}

fn platform_asset_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("linux", "x86_64") => "komari-agent-rs-linux-x86_64-full",
        ("linux", "aarch64") => "komari-agent-rs-linux-arm64-full",
        ("windows", "x86_64") => "komari-agent-rs-windows-x86_64.exe",
        ("macos", "x86_64") => "komari-agent-rs-macos-x86_64",
        ("macos", "aarch64") => "komari-agent-rs-macos-arm64",
        ("freebsd", "x86_64") => "komari-agent-rs-freebsd-x86_64",
        _ => "komari-agent-rs-unknown",
    }
}

fn find_checksum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    let mut found = None;
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            continue;
        };
        let Some(file) = fields.next() else {
            continue;
        };
        if file.strip_prefix('*').unwrap_or(file) != name {
            continue;
        }
        if found.is_some()
            || fields.next().is_some()
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        found = Some(hash);
    }
    found
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_json_whitespace_escaping_and_asset_boundaries() {
        let json = r#"{
            "tag_name" : "v1.2.3", "body": "not a field: \"name\":\"missing\"",
            "assets" : [
                {"name":"a", "browser_download_url": "https:\/\/github.com\/a"},
                {"browser_download_url":"https://github.com/\u0063", "name":"checksums.txt"},
                {"name":"no-url"}, {"name":"other", "browser_download_url":"https://github.com/other"}
            ]
        }"#;
        assert_eq!(json_str(json, "tag_name").as_deref(), Some("v1.2.3"));
        assert_eq!(
            json_asset_url(json, "a").as_deref(),
            Some("https://github.com/a")
        );
        assert_eq!(
            json_asset_url(json, CHECKSUMS_ASSET).as_deref(),
            Some("https://github.com/c")
        );
        assert_eq!(json_asset_url(json, "no-url"), None);
        assert_eq!(json_asset_url(json, "missing"), None);
        assert_eq!(
            json_str(r#"{"s":"quote\" slash\\ café \uD83D\uDE80"}"#, "s").as_deref(),
            Some("quote\" slash\\ café 🚀")
        );
        for bad in [r#"{"s":"\uD800"}"#, r#"{"s":"\x"}"#, r#"{"s":"x","s":"y"}"#] {
            assert_eq!(json_str(bad, "s"), None);
        }
        for bad in [
            r#"{"assets":[{"name":"a","browser_download_url":"https://x/a"},]}"#,
            r#"{"assets":[{"name":"a","browser_download_url":"https://x/a"]}"#,
            r#"{"assets":[{"name":"a","browser_download_url":"https://x/a"}"#,
            r#"{"assets":[true]}"#,
        ] {
            assert_eq!(json_asset_url(bad, "a"), None);
        }
    }

    #[test]
    fn requested_release_selector_is_stable_and_exact() {
        let hash = "a".repeat(64);
        let json = format!(
            r#"[
              {{"tag_name":"v0.7.1","draft":false,"prerelease":false,"assets":[
                {{"name":"{}","browser_download_url":"https://example.test/071"}},
                {{"name":"checksums.txt","browser_download_url":"https://example.test/s071"}}]}},
              {{"tag_name":"v0.7.2-rc1","draft":false,"prerelease":true,"assets":[]}},
              {{"tag_name":"v0.6.9","draft":false,"prerelease":false,"assets":[
                {{"name":"{}","browser_download_url":"https://example.test/069"}},
                {{"name":"checksums.txt","browser_download_url":"https://example.test/s069"}}]}}
            ]"#,
            platform_asset(),
            platform_asset()
        );
        let selected = select_release(&json, "0.7").unwrap();
        assert_eq!(selected.0, "0.7.1");
        assert_eq!(selected.1, "https://example.test/071");
        assert!(select_release(&json, "0.7.2-rc1").is_none());
        assert!(version_matches_target("0.7.1", [0, 7, 1], "0.*"));
        assert!(!version_matches_target("0.7.1", [0, 7, 1], "0.6.*"));
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn current_exact_target_needs_no_network() {
        let mut config = Config::default();
        config.disable_auto_update = false;
        assert!(!check_and_update_target("v0.7.0", &config, "v0.7.0").unwrap());
    }

    #[test]
    fn release_asset_and_checksum_exact_match() {
        let hash = "A".repeat(64);
        let other = "b".repeat(64);
        let sums = format!("{other}  agent-full\n{hash} *agent\n");
        assert_eq!(find_checksum(&sums, "agent"), Some(hash.as_str()));
        assert_eq!(find_checksum(&sums, "agen"), None);
        for invalid in [
            format!("short agent"),
            format!("{} agent", "z".repeat(64)),
            format!("{hash} agent extra"),
            format!("{hash} agent\n{hash} agent"),
        ] {
            assert_eq!(find_checksum(&invalid, "agent"), None);
        }
        let json = r#"{"assets":[{"name":"agent-full","browser_download_url":"https://x/full"}]}"#;
        assert_eq!(json_asset_url(json, "agent"), None);
        let json = r#"{"assets":[{"name":"a","browser_download_url":"https://x/1"},{"name":"a","browser_download_url":"https://x/2"}]}"#;
        assert_eq!(json_asset_url(json, "a"), None);
        if cfg!(target_os = "linux") {
            assert!(platform_asset().ends_with("-full"));
        }
        assert!(semver_gt("0.5.1", "0.5.0"));
        assert!(!semver_gt("0.6.0-rc1", "0.5.0"));
        for (os, arch, expected) in [
            ("linux", "x86_64", "komari-agent-rs-linux-x86_64-full"),
            ("linux", "aarch64", "komari-agent-rs-linux-arm64-full"),
            ("windows", "x86_64", "komari-agent-rs-windows-x86_64.exe"),
            ("macos", "x86_64", "komari-agent-rs-macos-x86_64"),
            ("macos", "aarch64", "komari-agent-rs-macos-arm64"),
            ("freebsd", "x86_64", "komari-agent-rs-freebsd-x86_64"),
            ("other", "other", "komari-agent-rs-unknown"),
        ] {
            assert_eq!(platform_asset_for(os, arch), expected);
        }
    }

    #[test]
    fn redirect_resolution_and_url_rejection() {
        let base = HttpsUrl::parse("https://GITHUB.com/a/b?old=1").unwrap();
        for (location, expected) in [
            ("../c?q=1", "/c?q=1"),
            ("/d", "/d"),
            ("?new=1", "/a/b?new=1"),
            ("?url=https://example.com", "/a/b?url=https://example.com"),
            ("./c", "/a/c"),
            ("..", "/"),
            ("#fragment", "/a/b?old=1"),
        ] {
            assert_eq!(base.redirect(location).unwrap().target, expected);
        }
        assert_eq!(
            base.redirect("//cdn.example/x").unwrap().host,
            "cdn.example"
        );
        assert_eq!(base.redirect("https://cdn.example/x").unwrap().target, "/x");
        for bad in [
            "http://github.com/a",
            "https://user:pass@github.com/", // # leak-guard-allow:LG-F4C6A155 synthetic invalid-authority fixture
            "https://github.com:0/a",
            "https://github.com/\r\nX: evil",
            "https://github.com\\@evil/a",
        ] {
            assert!(HttpsUrl::parse(bad).is_err());
        }
        assert!(base.redirect("http://github.com/a").is_err());
    }

    #[test]
    fn redirects_strip_cross_origin_auth_permanently() {
        let mut calls = 0;
        let body = follow_redirects(
            "https://github.com/a",
            Some("synthetic-token"),
            |url, auth| {
                calls += 1;
                match calls {
                    1 => {
                        assert_eq!(auth, Some("synthetic-token"));
                        Ok(Response::Redirect("/b".into()))
                    }
                    2 => {
                        assert_eq!(auth, Some("synthetic-token"));
                        Ok(Response::Redirect(
                            "https://cdn.example/file?signature=synthetic".into(),
                        ))
                    }
                    3 => {
                        assert_eq!(url.host, "cdn.example");
                        assert_eq!(auth, None);
                        Ok(Response::Redirect("https://github.com/c".into()))
                    }
                    _ => {
                        assert_eq!(auth, None);
                        Ok(Response::Body(b"binary".to_vec()))
                    }
                }
            },
        )
        .unwrap();
        assert_eq!(body, b"binary");
        follow_redirects(
            "https://cdn.example/file",
            Some("synthetic-token"),
            |_, auth| {
                assert_eq!(auth, None);
                Ok(Response::Body(Vec::new()))
            },
        )
        .unwrap();
        follow_redirects(
            "https://github.com/a",
            Some("synthetic-token"),
            |url, auth| {
                if url.port == 443 {
                    Ok(Response::Redirect("https://github.com:444/b".into()))
                } else {
                    assert_eq!(auth, None);
                    Ok(Response::Body(Vec::new()))
                }
            },
        )
        .unwrap();
    }

    #[test]
    fn redirects_reject_loops_downgrades_and_excessive_hops() {
        assert!(
            follow_redirects(
                "https://github.com/a",
                Some("synthetic\r\nX: evil"),
                |_, _| {
                    panic!("invalid authentication must fail before network I/O");
                }
            )
            .is_err()
        );
        assert!(
            follow_redirects("https://github.com/a", None, |_, _| Ok(Response::Redirect(
                "/a".into()
            )))
            .is_err()
        );
        assert!(
            follow_redirects("https://github.com/a", None, |_, _| Ok(Response::Redirect(
                "http://github.com/b".into()
            )))
            .is_err()
        );
        let mut calls = 0;
        assert!(
            follow_redirects("https://github.com/a", None, |_, _| {
                calls += 1;
                Ok(Response::Redirect(format!("/{calls}")))
            })
            .is_err()
        );
        assert_eq!(calls, MAX_REDIRECTS + 1);
    }

    #[test]
    fn http_framing_and_body_caps() {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc",
            "HTTP/1.1 200 OK\r\n\r\nabc",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n2\r\nbc\r\n0\r\n\r\n",
        ] {
            match read_response(&mut io::Cursor::new(response), 3).unwrap() {
                Response::Body(b) => assert_eq!(b, b"abc"),
                _ => panic!("expected body"),
            }
            assert!(read_response(&mut io::Cursor::new(response), 2).is_err());
        }
        for bad in [
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabc",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 302 Found\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n",
        ] {
            assert!(read_response(&mut io::Cursor::new(bad), 10).is_err());
        }
        let huge = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "x".repeat(MAX_HEADERS));
        assert!(read_response(&mut io::Cursor::new(huge), 10).is_err());
        let redirect =
            "HTTP/1.1 302 Found\r\nLocation: https://cdn.example/a\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(
            read_response(&mut io::Cursor::new(redirect), 10).unwrap(),
            Response::Redirect(_)
        ));
    }

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path =
                env::temp_dir().join(format!("komari-updater-test-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn install_verifies_before_touching_files_and_retains_backup() {
        let dir = TestDir::new();
        let exe = dir.0.join("agent.exe");
        fs::write(&exe, b"old").unwrap();
        assert!(install_verified_binary(&exe, b"new", &"0".repeat(64)).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert!(!sidecar(&exe, ".new").exists());
        assert!(!sidecar(&exe, ".old").exists());
        let old = install_verified_binary(&exe, b"new", &sha256_hex(b"new")).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(fs::read(old).unwrap(), b"old");
        fs::write(sidecar(&exe, ".new"), b"do not overwrite").unwrap();
        assert!(install_verified_binary(&exe, b"next", &sha256_hex(b"next")).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(
            fs::read(sidecar(&exe, ".new")).unwrap(),
            b"do not overwrite"
        );
    }

    #[test]
    fn failed_replacement_restores_original_and_failed_rollback_keeps_backup() {
        for fail_rollback in [false, true] {
            let dir = TestDir::new();
            let exe = dir.0.join("agent.exe");
            let new = sidecar(&exe, ".new");
            let old = sidecar(&exe, ".old");
            fs::write(&exe, b"old").unwrap();
            fs::write(&new, b"new").unwrap();
            let mut calls = 0;
            let result = replace_with_rollback(&exe, &new, &old, |a, b| {
                calls += 1;
                if calls == 2 || (calls == 3 && fail_rollback) {
                    return Err(io::Error::other("synthetic rename failure"));
                }
                fs::rename(a, b)
            });
            assert!(result.is_err());
            assert_eq!(
                fs::read(if fail_rollback { &old } else { &exe }).unwrap(),
                b"old"
            );
            assert_eq!(fs::read(new).unwrap(), b"new");
        }
    }

    #[test]
    fn failed_exec_restores_installed_path_or_preserves_backup() {
        for fail_rollback in [false, true] {
            let dir = TestDir::new();
            let exe = dir.0.join("agent");
            let old = sidecar(&exe, ".old");
            fs::write(&old, b"old").unwrap();
            if fail_rollback {
                fs::create_dir(&exe).unwrap();
            }
            let error =
                restore_after_failed_exec(&exe, &old, io::Error::other("synthetic exec failure"));
            assert!(error.to_string().contains(if fail_rollback {
                "rollback failed"
            } else {
                "rolled back"
            }));
            assert_eq!(
                fs::read(if fail_rollback { &old } else { &exe }).unwrap(),
                b"old"
            );
        }
    }
}
