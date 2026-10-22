//! Server-driven file management compatible with Komari's v2 `agent.file`.
//!
//! The entire surface is gated by `disable_web_ssh`; the default fleet posture
//! therefore exposes no file capability. Metadata remains JSON-RPC while file
//! bytes use the authenticated raw transfer endpoint. Operations are bounded
//! and uploads are committed through a same-directory temporary file.

use crate::config::Config;
use crate::json::{decode_json_string, object_field};
use crate::proxy::Dialer;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

const MAX_CONTROL_JSON: usize = 1024 * 1024;
const MAX_CHUNK: u64 = 128 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 500;
const MAX_SEARCH_LINE: usize = 300;
const STREAM_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_WORKERS: usize = 8;
static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
enum FileErr {
    Io(io::Error),
    Invalid(&'static str),
    Message(String),
}

impl fmt::Display for FileErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Invalid(error) => f.write_str(error),
            Self::Message(error) => f.write_str(error),
        }
    }
}

impl From<io::Error> for FileErr {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Default)]
struct UploadState {
    target: PathBuf,
    stage: PathBuf,
    total: u64,
    chunk_size: u64,
    chunk_count: u64,
    received: HashSet<u64>,
    canceled: Arc<AtomicBool>,
}

fn uploads() -> &'static Mutex<HashMap<String, UploadState>> {
    static UPLOADS: OnceLock<Mutex<HashMap<String, UploadState>>> = OnceLock::new();
    UPLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn canceled_uploads() -> &'static Mutex<HashSet<String>> {
    static CANCELED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CANCELED.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn spawn(config: &Config, dial: &Dialer, tls: &Arc<rustls::ClientConfig>, event: &[u8]) {
    if ACTIVE_WORKERS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < MAX_WORKERS).then_some(active + 1)
        })
        .is_err()
    {
        eprintln!("[komari] WARN: file-control worker limit reached");
        return;
    }
    let config = config.clone();
    let dial = dial.clone();
    let tls = Arc::clone(tls);
    let event = event.to_vec();
    if let Err(error) = std::thread::Builder::new()
        .name("file-control".into())
        .spawn(move || {
            struct WorkerGuard;
            impl Drop for WorkerGuard {
                fn drop(&mut self) {
                    ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _guard = WorkerGuard;
            handle(&config, &dial, &tls, &event);
        })
    {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
        eprintln!("[komari] WARN: failed to spawn file-control worker: {error}");
    }
}

fn handle(config: &Config, dial: &Dialer, tls: &Arc<rustls::ClientConfig>, event: &[u8]) {
    let request_id = field_string(event, "request_id").unwrap_or_default();
    let uuid = field_string(event, "uuid").unwrap_or_default();
    let op = field_string(event, "op").unwrap_or_default();
    let args = crate::server::task::extract_json_raw(event, "args").unwrap_or(b"{}");
    let result = if config.disable_web_ssh {
        Err(FileErr::Invalid("web control is disabled"))
    } else if request_id.is_empty() || op.is_empty() {
        Err(FileErr::Invalid("request_id and op are required"))
    } else {
        execute(config, dial, tls, &op, args)
    };
    let payload = result_payload(&uuid, &request_id, result);
    let request = crate::protocol::v2::new_notification(
        crate::protocol::v2::METHOD_AGENT_FILE_RESULT,
        &payload,
    );
    let url = format!(
        "{}/api/clients/v2/rpc?token={}",
        config.endpoint.trim_end_matches('/'),
        crate::ws::url_encode(&config.token)
    );
    for attempt in 1..=4 {
        match crate::http::http_post(&url, &request, "application/json", None, &[], tls, dial) {
            Ok(response) if (200..300).contains(&response.status_code) => break,
            Ok(response) if (400..500).contains(&response.status_code) => {
                eprintln!(
                    "[komari] WARN: file result returned HTTP {}",
                    response.status_code
                );
                break;
            }
            Ok(response) if attempt == 4 => {
                eprintln!(
                    "[komari] WARN: file result returned HTTP {}",
                    response.status_code
                );
            }
            Err(error) if attempt == 4 => {
                eprintln!("[komari] WARN: file result upload failed: {error}");
            }
            _ => std::thread::sleep(Duration::from_millis(attempt * 500)),
        }
    }
}

fn execute(
    config: &Config,
    dial: &Dialer,
    tls: &Arc<rustls::ClientConfig>,
    op: &str,
    args: &[u8],
) -> Result<Vec<u8>, FileErr> {
    match op {
        "list" => list(path_arg(args, "path")),
        "list_roots" => list_roots(),
        "stat" => describe(&resolve_path(&path_arg(args, "path"))),
        "create" => create_empty(&resolve_path(&path_arg(args, "path"))),
        "mkdir" => mkdir(
            &resolve_path(&path_arg(args, "path")),
            &path_arg(args, "mode"),
        ),
        "delete" => delete(&resolve_path(&path_arg(args, "path"))),
        "move" => move_path(
            &resolve_path(&path_arg(args, "source")),
            &resolve_path(&path_arg(args, "destination")),
        ),
        "copy" => copy_path(
            &resolve_path(&path_arg(args, "source")),
            &resolve_path(&path_arg(args, "destination")),
        ),
        "chmod" => chmod(
            &resolve_path(&path_arg(args, "path")),
            &path_arg(args, "mode"),
        ),
        "chown" => chown(&resolve_path(&path_arg(args, "path")), args),
        "search" => search(args),
        "download_stream" => download_stream(config, dial, tls, args),
        "upload_stream" => upload_stream(config, dial, tls, args),
        "upload_commit" => upload_commit(args),
        "upload_cancel" => upload_cancel(args),
        _ => Err(FileErr::Invalid("unsupported file operation")),
    }
}

fn field_string(data: &[u8], key: &str) -> Option<String> {
    crate::server::task::extract_json_string(data, key)
}

fn direct_string(data: &[u8], key: &str) -> Option<String> {
    decode_json_string(object_field(data, key)?)
}

fn direct_i64(data: &[u8], key: &str) -> Option<i64> {
    let raw = object_field(data, key)?;
    std::str::from_utf8(raw)
        .ok()?
        .trim_matches('"')
        .parse()
        .ok()
}

fn direct_bool(data: &[u8], key: &str) -> bool {
    object_field(data, key) == Some(b"true".as_slice())
}

fn path_arg(data: &[u8], key: &str) -> String {
    direct_string(data, key).unwrap_or_default()
}

fn resolve_path(value: &str) -> PathBuf {
    let value = value.trim();
    #[cfg(windows)]
    {
        let normalized = value.replace('/', "\\");
        if normalized == "\\" || normalized.is_empty() {
            return PathBuf::from(
                std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\",
            );
        }
        if normalized.len() >= 3
            && normalized.starts_with('\\')
            && normalized.as_bytes()[2] == b'\\'
        {
            return PathBuf::from(format!("{}:{}", &normalized[1..2], &normalized[2..]));
        }
        PathBuf::from(normalized)
    }
    #[cfg(not(windows))]
    {
        if value.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(value)
        }
    }
}

fn virtual_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.len() >= 2 && text.as_bytes()[1] == b':' {
            return format!("/{}{}", &text[..1].to_ascii_uppercase(), &text[2..]);
        }
        text
    }
    #[cfg(not(windows))]
    path.to_string_lossy().into_owned()
}

fn list(path: String) -> Result<Vec<u8>, FileErr> {
    #[cfg(windows)]
    if path.trim().is_empty() || path.trim() == "/" {
        let mut entries = Vec::new();
        for drive in b'A'..=b'Z' {
            let native = PathBuf::from(format!("{}:\\", drive as char));
            if native.exists() {
                entries.push(describe(&native)?);
            }
        }
        return Ok(json_array(entries));
    }
    let path = resolve_path(&path);
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
        entries.push((
            !metadata.is_dir(),
            entry.file_name().to_string_lossy().to_lowercase(),
            describe(&entry.path())?,
        ));
    }
    entries.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    Ok(json_array(entries.into_iter().map(|(_, _, value)| value)))
}

fn list_roots() -> Result<Vec<u8>, FileErr> {
    #[cfg(windows)]
    {
        Ok(br#"[{"name":"/","path":"/","is_dir":true,"is_symlink":false,"size":0,"mode_octal":"0755","uid":-1,"gid":-1,"mode":"drwxrwxrwx","owner":"","group":"","modified_at":"1970-01-01T00:00:00Z"}]"#.to_vec())
    }
    #[cfg(not(windows))]
    Ok(json_array([describe(Path::new("/"))?]))
}

fn json_array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut out = vec![b'['];
    for value in values {
        if out.len() > 1 {
            out.push(b',');
        }
        out.extend_from_slice(&value);
    }
    out.push(b']');
    out
}

fn describe(path: &Path) -> Result<Vec<u8>, FileErr> {
    let link_meta = fs::symlink_metadata(path)?;
    let is_symlink = link_meta.file_type().is_symlink();
    let meta = if is_symlink {
        fs::metadata(path).unwrap_or(link_meta)
    } else {
        link_meta
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"{\"name\":");
    crate::json::write_json_string(
        &mut out,
        &path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| "/".into()),
    );
    out.extend_from_slice(b",\"path\":");
    crate::json::write_json_string(&mut out, &virtual_path(path));
    out.extend_from_slice(
        format!(
            ",\"is_dir\":{},\"is_symlink\":{},\"size\":{}",
            meta.is_dir(),
            is_symlink,
            meta.len()
        )
        .as_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        out.extend_from_slice(
            format!(
                ",\"mode_octal\":\"{:04o}\",\"uid\":{},\"gid\":{}",
                meta.mode() & 0o7777,
                meta.uid(),
                meta.gid()
            )
            .as_bytes(),
        );
    }
    #[cfg(not(unix))]
    out.extend_from_slice(b",\"mode_octal\":\"0000\",\"uid\":-1,\"gid\":-1");
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    out.extend_from_slice(b",\"mode\":");
    crate::json::write_json_string(&mut out, &mode_string(&meta));
    out.extend_from_slice(b",\"owner\":");
    crate::json::write_json_string(&mut out, &owner_name(&meta));
    out.extend_from_slice(b",\"group\":");
    crate::json::write_json_string(&mut out, &group_name(&meta));
    out.extend_from_slice(b",\"modified_at\":");
    crate::json::write_json_string(&mut out, &rfc3339_from_unix(modified));
    if is_symlink && let Ok(target) = fs::read_link(path) {
        out.extend_from_slice(b",\"target\":");
        crate::json::write_json_string(&mut out, &virtual_path(&target));
    }
    out.push(b'}');
    Ok(out)
}

fn mode_string(metadata: &fs::Metadata) -> String {
    let kind = if metadata.is_dir() {
        'd'
    } else if metadata.file_type().is_symlink() {
        'l'
    } else {
        '-'
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        let mut value = String::with_capacity(10);
        value.push(kind);
        for (read, write, execute) in [(8, 7, 6), (5, 4, 3), (2, 1, 0)] {
            value.push(if mode & (1 << read) != 0 { 'r' } else { '-' });
            value.push(if mode & (1 << write) != 0 { 'w' } else { '-' });
            value.push(if mode & (1 << execute) != 0 { 'x' } else { '-' });
        }
        value
    }
    #[cfg(not(unix))]
    format!("{kind}rwxrwxrwx")
}

#[cfg(unix)]
fn account_name(path: &str, id: u32) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() > 2 && fields[2].parse::<u32>().ok() == Some(id) {
            return fields[0].to_string();
        }
    }
    String::new()
}

fn owner_name(_metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        account_name("/etc/passwd", _metadata.uid())
    }
    #[cfg(not(unix))]
    String::new()
}

fn group_name(_metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        account_name("/etc/group", _metadata.gid())
    }
    #[cfg(not(unix))]
    String::new()
}

fn rfc3339_from_unix(seconds: u64) -> String {
    let days = (seconds / 86400) as i64;
    let remaining = seconds % 86400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remaining / 3600,
        (remaining % 3600) / 60,
        remaining % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let value = days + 719468;
    let era = if value >= 0 { value } else { value - 146096 } / 146097;
    let doe = (value - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month, day)
}

fn ensure_not_root(path: &Path) -> Result<(), FileErr> {
    if path.parent().is_none() || path.as_os_str().is_empty() {
        return Err(FileErr::Invalid("refusing filesystem root operation"));
    }
    Ok(())
}

fn create_empty(path: &Path) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stage = stage_path(path, "empty");
    let prior_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stage)?;
    if let Some(permissions) = prior_permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    drop(file);
    replace(&stage, path)?;
    Ok(br#"{"created":true}"#.to_vec())
}

fn mkdir(path: &Path, mode: &str) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(path)?;
    fs::create_dir_all(path)?;
    if !mode.trim().is_empty() {
        let _ = chmod(path, mode)?;
    }
    Ok(br#"{"created":true}"#.to_vec())
}

fn delete(path: &Path) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(path)?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(br#"{"deleted":true}"#.to_vec())
}

fn move_path(source: &Path, destination: &Path) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(source)?;
    ensure_not_root(destination)?;
    if destination.starts_with(source) {
        return Err(FileErr::Invalid("destination is inside source"));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(source, destination).is_err() {
        copy_recursive(source, destination)?;
        let metadata = fs::symlink_metadata(source)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(source)?;
        } else {
            fs::remove_file(source)?;
        }
    }
    Ok(br#"{"moved":true}"#.to_vec())
}

fn copy_path(source: &Path, destination: &Path) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(destination)?;
    if destination.starts_with(source) {
        return Err(FileErr::Invalid("destination is inside source"));
    }
    copy_recursive(source, destination)?;
    Ok(br#"{"copied":true}"#.to_vec())
}

fn copy_recursive(source: &Path, destination: &Path) -> Result<(), FileErr> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination)?;
        #[cfg(windows)]
        {
            if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
                std::os::windows::fs::symlink_dir(target, destination)?;
            } else {
                std::os::windows::fs::symlink_file(target, destination)?;
            }
        }
    }
    if meta.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn chmod(path: &Path, mode: &str) -> Result<Vec<u8>, FileErr> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = u32::from_str_radix(mode.trim_start_matches("0o"), 8)
            .map_err(|_| FileErr::Invalid("invalid mode"))?;
        if mode > 0o7777 {
            return Err(FileErr::Invalid("invalid mode"));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(format!("{{\"mode\":\"{mode:04o}\"}}").into_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(FileErr::Invalid("chmod is not supported on this platform"))
    }
}

fn chown(path: &Path, args: &[u8]) -> Result<Vec<u8>, FileErr> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let uid = direct_i64(args, "uid")
            .or_else(|| {
                direct_string(args, "owner").and_then(|value| resolve_account(&value, true))
            })
            .unwrap_or(-1);
        let gid = direct_i64(args, "gid")
            .or_else(|| {
                direct_string(args, "group").and_then(|value| resolve_account(&value, false))
            })
            .unwrap_or(-1);
        if uid < -1 || gid < -1 || (uid == -1 && gid == -1) {
            return Err(FileErr::Invalid("uid or gid is required"));
        }
        unsafe extern "C" {
            fn chown(path: *const core::ffi::c_char, uid: u32, gid: u32) -> i32;
        }
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        if bytes.contains(&0) {
            return Err(FileErr::Invalid("path contains NUL"));
        }
        bytes.push(0);
        let result = unsafe {
            chown(
                bytes.as_ptr().cast(),
                if uid < 0 { u32::MAX } else { uid as u32 },
                if gid < 0 { u32::MAX } else { gid as u32 },
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(format!("{{\"uid\":{uid},\"gid\":{gid}}}").into_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, args);
        Err(FileErr::Invalid("chown is not supported on this platform"))
    }
}

#[cfg(unix)]
fn resolve_account(value: &str, user: bool) -> Option<i64> {
    if let Ok(id) = value.trim().parse::<i64>() {
        return Some(id);
    }
    let file = File::open(if user { "/etc/passwd" } else { "/etc/group" }).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() > 2 && fields[0] == value.trim() {
            return fields[2].parse().ok();
        }
    }
    None
}

fn search(args: &[u8]) -> Result<Vec<u8>, FileErr> {
    let root = resolve_path(&path_arg(args, "path"));
    let query = path_arg(args, "query");
    if query.is_empty() {
        return Err(FileErr::Invalid("query is required"));
    }
    let content = direct_bool(args, "content");
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();
    walk(&root, &mut |path, meta| {
        if results.len() >= MAX_SEARCH_RESULTS {
            return Ok(false);
        }
        let name_match = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&query_lower));
        if name_match && !content {
            results.push(search_result(path, 0, ""));
        } else if content
            && meta.is_file()
            && meta.len() <= 10 * 1024 * 1024
            && !meta.file_type().is_symlink()
            && let Ok(file) = File::open(path)
        {
            for (index, line) in BufReader::new(file).lines().enumerate() {
                let line = line?;
                if line.to_lowercase().contains(&query_lower) {
                    let text: String = line.chars().take(MAX_SEARCH_LINE).collect();
                    results.push(search_result(path, index + 1, &text));
                    break;
                }
            }
        }
        Ok(true)
    })?;
    let mut out = b"{\"matches\":[".to_vec();
    out.extend_from_slice(results.join(",").as_bytes());
    out.extend_from_slice(
        format!("],\"limited\":{}}}", results.len() >= MAX_SEARCH_RESULTS).as_bytes(),
    );
    Ok(out)
}

fn walk(
    path: &Path,
    visit: &mut impl FnMut(&Path, &fs::Metadata) -> Result<bool, FileErr>,
) -> Result<(), FileErr> {
    let meta = fs::symlink_metadata(path)?;
    if !visit(path, &meta)? {
        return Ok(());
    }
    if meta.is_dir() && !meta.file_type().is_symlink() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_error) if path.parent().is_some() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries.flatten() {
            if walk(&entry.path(), visit).is_err() {
                continue;
            }
        }
    }
    Ok(())
}

fn search_result(path: &Path, line: usize, text: &str) -> String {
    let mut out = b"{\"path\":".to_vec();
    crate::json::write_json_string(&mut out, &virtual_path(path));
    out.extend_from_slice(format!(",\"line\":{line},\"text\":").as_bytes());
    crate::json::write_json_string(&mut out, text);
    out.push(b'}');
    String::from_utf8(out).unwrap()
}

fn transfer_url(config: &Config, args: &[u8]) -> Result<String, FileErr> {
    let id = path_arg(args, "transfer_id");
    let token = path_arg(args, "transfer_token");
    if id.is_empty() || token.is_empty() {
        return Err(FileErr::Invalid("transfer credentials are required"));
    }
    Ok(format!(
        "{}/api/clients/transfer/{}?token={}&transfer_token={}",
        config.endpoint.trim_end_matches('/'),
        crate::ws::url_encode(&id),
        crate::ws::url_encode(&config.token),
        crate::ws::url_encode(&token)
    ))
}

fn transfer_headers(args: &[u8]) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for (header, key) in [
        ("X-Komari-Transfer-ID", "transfer_id"),
        ("X-Komari-Transfer-Token", "transfer_token"),
    ] {
        headers.push((header.into(), path_arg(args, key)));
    }
    headers
}

fn download_stream(
    config: &Config,
    dial: &Dialer,
    tls: &Arc<rustls::ClientConfig>,
    args: &[u8],
) -> Result<Vec<u8>, FileErr> {
    let path = resolve_path(&path_arg(args, "path"));
    let offset = u64_arg(args, "offset")?;
    let length = u64_arg(args, "length")?;
    if length == 0 || length > MAX_CHUNK {
        return Err(FileErr::Invalid("invalid download range"));
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();
    if direct_i64(args, "file_size").is_some_and(|expected| expected < 0 || expected as u64 != size)
    {
        return Err(FileErr::Invalid("file changed while opening stream"));
    }
    if let Some(expected) = direct_string(args, "modified_at") {
        let actual = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or_else(String::new, |value| rfc3339_from_unix(value.as_secs()));
        if !actual.is_empty() && actual != expected {
            return Err(FileErr::Invalid("file changed while opening stream"));
        }
    }
    if offset.checked_add(length).is_none_or(|end| end > size) {
        return Err(FileErr::Invalid("download range exceeds file"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut headers = transfer_headers(args);
    headers.push(("X-Komari-Transfer-Offset".into(), offset.to_string()));
    headers.push(("X-Komari-Transfer-Length".into(), length.to_string()));
    let mut response_body = Vec::new();
    let response = crate::http::http_post_stream(
        &transfer_url(config, args)?,
        &mut file,
        length,
        &mut response_body,
        4096,
        None,
        "application/octet-stream",
        &headers,
        tls,
        dial,
        STREAM_TIMEOUT,
    )
    .map_err(|e| FileErr::Message(e.to_string()))?;
    if !(200..300).contains(&response.status_code) {
        return Err(FileErr::Invalid("transfer endpoint rejected download"));
    }
    Ok(format!("{{\"sent\":{length}}}").into_bytes())
}

fn upload_stream(
    config: &Config,
    dial: &Dialer,
    tls: &Arc<rustls::ClientConfig>,
    args: &[u8],
) -> Result<Vec<u8>, FileErr> {
    let id = path_arg(args, "upload_id");
    let target = resolve_path(&path_arg(args, "path"));
    let offset = u64_arg(args, "offset")?;
    let index = u64_arg(args, "chunk_index")?;
    let count = u64_arg(args, "chunk_count")?;
    let total = u64_arg(args, "total_size")?;
    let chunk = direct_i64(args, "chunk_size").unwrap_or(25 * 1024 * 1024) as u64;
    if id.is_empty()
        || total == 0
        || count == 0
        || chunk == 0
        || chunk > MAX_CHUNK
        || index >= count
        || offset != index * chunk
    {
        return Err(FileErr::Invalid("invalid upload metadata"));
    }
    let expected = chunk.min(total - offset);
    if canceled_uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?
        .contains(&id)
    {
        return Err(FileErr::Invalid("upload session was cancelled"));
    }
    let (stage, canceled) = {
        let mut map = uploads()
            .lock()
            .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
        if let Some(state) = map.get(&id) {
            if state.target != target
                || state.total != total
                || state.chunk_size != chunk
                || state.chunk_count != count
            {
                return Err(FileErr::Invalid("upload session metadata changed"));
            }
            (state.stage.clone(), Arc::clone(&state.canceled))
        } else {
            if index != 0 {
                return Err(FileErr::Invalid("unknown upload session"));
            }
            let stage = stage_path(&target, &format!("upload-{id}"));
            if let Some(parent) = stage.parent() {
                fs::create_dir_all(parent)?;
            }
            let stage_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stage)?;
            stage_file.sync_all()?;
            drop(stage_file);
            let canceled = Arc::new(AtomicBool::new(false));
            map.insert(
                id.clone(),
                UploadState {
                    target: target.clone(),
                    stage: stage.clone(),
                    total,
                    chunk_size: chunk,
                    chunk_count: count,
                    received: HashSet::new(),
                    canceled: Arc::clone(&canceled),
                },
            );
            (stage, canceled)
        }
    };
    let headers = transfer_headers(args);
    let chunk_stage = stage_path(&target, &format!("chunk-{id}-{index}"));
    if let Some(parent) = chunk_stage.parent() {
        fs::create_dir_all(parent)?;
    }
    let received = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&chunk_stage)?;
    let mut received = CancelWriter { received, canceled };
    let response = crate::http::http_post_stream(
        &transfer_url(config, args)?,
        &mut io::empty(),
        0,
        &mut received,
        expected,
        Some(expected),
        "application/octet-stream",
        &headers,
        tls,
        dial,
        STREAM_TIMEOUT,
    )
    .map_err(|e| {
        let _ = fs::remove_file(&chunk_stage);
        FileErr::Message(e.to_string())
    })?;
    if response.status_code != 200 || response.body_len != expected {
        let _ = fs::remove_file(&chunk_stage);
        return Err(FileErr::Invalid("upload transfer returned invalid body"));
    }
    if canceled_uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?
        .contains(&id)
    {
        let _ = fs::remove_file(&chunk_stage);
        return Err(FileErr::Invalid("upload session was cancelled"));
    }
    received.received.sync_all()?;
    drop(received);
    let mut file = OpenOptions::new().write(true).open(&stage)?;
    let mut received = File::open(&chunk_stage)?;
    file.seek(SeekFrom::Start(offset))?;
    let written = io::copy(&mut received, &mut file)?;
    if written != expected {
        let _ = fs::remove_file(&chunk_stage);
        return Err(FileErr::Invalid("upload transfer was truncated"));
    }
    file.sync_data()?;
    drop(file);
    drop(received);
    fs::remove_file(&chunk_stage)?;
    let mut map = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
    let state = map.get_mut(&id).ok_or_else(|| {
        let _ = fs::remove_file(&stage);
        FileErr::Invalid("upload session was cancelled")
    })?;
    state.received.insert(index);
    Ok(format!(
        "{{\"received\":{},\"offset\":{}}}",
        written,
        offset + expected
    )
    .into_bytes())
}

fn upload_commit(args: &[u8]) -> Result<Vec<u8>, FileErr> {
    let id = path_arg(args, "upload_id");
    let mut map = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
    let state = map
        .get(&id)
        .ok_or(FileErr::Invalid("unknown upload session"))?;
    let path = resolve_path(&path_arg(args, "path"));
    let total = u64_arg(args, "total_size")?;
    let chunk_size = u64_arg(args, "chunk_size")?;
    let chunk_count = u64_arg(args, "chunk_count")?;
    if state.target != path
        || state.total != total
        || state.chunk_size != chunk_size
        || state.chunk_count != chunk_count
    {
        return Err(FileErr::Invalid("upload commit metadata changed"));
    }
    if state.received.len() as u64 != state.chunk_count {
        return Err(FileErr::Invalid("upload is incomplete"));
    }
    let state = map
        .remove(&id)
        .ok_or(FileErr::Invalid("unknown upload session"))?;
    let file = OpenOptions::new().write(true).open(&state.stage)?;
    file.set_len(state.total)?;
    file.sync_all()?;
    drop(file);
    replace(&state.stage, &state.target)?;
    Ok(format!(
        "{{\"received\":{},\"final\":true,\"offset\":{}}}",
        state.total, state.total
    )
    .into_bytes())
}

fn upload_cancel(args: &[u8]) -> Result<Vec<u8>, FileErr> {
    let id = path_arg(args, "upload_id");
    {
        let mut canceled = canceled_uploads()
            .lock()
            .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?;
        if canceled.len() >= 1024 {
            canceled.clear();
        }
        canceled.insert(id.clone());
    }
    if let Some(state) = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?
        .remove(&id)
    {
        state.canceled.store(true, Ordering::Release);
        let _ = fs::remove_file(state.stage);
    }
    Ok(br#"{"cancelled":true}"#.to_vec())
}

struct CancelWriter {
    received: File,
    canceled: Arc<AtomicBool>,
}

impl Write for CancelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.canceled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "upload was cancelled",
            ));
        }
        self.received.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.received.flush()
    }
}

fn u64_arg(args: &[u8], key: &str) -> Result<u64, FileErr> {
    direct_i64(args, key)
        .filter(|v| *v >= 0)
        .map(|v| v as u64)
        .ok_or(FileErr::Invalid("missing numeric field"))
}
fn stage_path(path: &Path, kind: &str) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    path.with_file_name(format!(".{name}.komari-{kind}-{}", std::process::id()))
}
fn replace(stage: &Path, target: &Path) -> Result<(), FileErr> {
    #[cfg(windows)]
    {
        if target.exists() {
            fs::remove_file(target)?;
        }
    }
    fs::rename(stage, target)?;
    Ok(())
}

fn result_payload(uuid: &str, request_id: &str, result: Result<Vec<u8>, FileErr>) -> Vec<u8> {
    let mut out = b"{\"uuid\":".to_vec();
    crate::json::write_json_string(&mut out, uuid);
    out.extend_from_slice(b",\"request_id\":");
    crate::json::write_json_string(&mut out, request_id);
    match result {
        Ok(value) => {
            out.extend_from_slice(b",\"ok\":true,\"result\":");
            out.extend_from_slice(if value.len() <= MAX_CONTROL_JSON {
                &value
            } else {
                b"null"
            });
        }
        Err(error) => {
            out.extend_from_slice(b",\"ok\":false,\"error\":");
            crate::json::write_json_string(&mut out, &error.to_string());
        }
    }
    out.push(b'}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_operations_reject_roots_and_commit_atomically() {
        let root = std::env::temp_dir().join(format!("komari-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        create_empty(&file).unwrap();
        fs::write(&file, b"old").unwrap();
        let copy = root.join("b.txt");
        copy_path(&file, &copy).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"old");
        assert!(delete(Path::new("/")).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn result_payload_escapes_server_identifiers_and_errors() {
        let p = result_payload("u\"", "r\\", Err(FileErr::Message("bad\nsecret".into())));
        assert!(crate::json::object_field(&p, "error").is_some());
        assert_eq!(
            decode_json_string(crate::json::object_field(&p, "uuid").unwrap()).as_deref(),
            Some("u\"")
        );
    }
}
