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
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CONTROL_JSON: usize = 1024 * 1024;
const MAX_CHUNK: u64 = 128 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 500;
const MAX_SEARCH_LINE: usize = 300;
const STREAM_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const UPLOAD_CANCEL_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_UPLOAD_SESSIONS: usize = 16;
const MAX_CANCELED_UPLOADS: usize = 1024;
const MAX_WORKERS: usize = 16;
const MAX_STREAMS: usize = 8;
static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_STREAMS: AtomicUsize = AtomicUsize::new(0);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

struct UploadState {
    target: PathBuf,
    stage: PathBuf,
    total: u64,
    chunk_size: u64,
    chunk_count: u64,
    received: HashSet<u64>,
    canceled: Arc<AtomicBool>,
    updated_at: Instant,
}

fn uploads() -> &'static Mutex<HashMap<String, UploadState>> {
    static UPLOADS: OnceLock<Mutex<HashMap<String, UploadState>>> = OnceLock::new();
    UPLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn canceled_uploads() -> &'static Mutex<HashMap<String, Instant>> {
    static CANCELED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CANCELED.get_or_init(|| Mutex::new(HashMap::new()))
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
    let expanded = expand_home(value);
    let value = expanded.as_deref().unwrap_or(value);
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
            return normalize_path(&PathBuf::from(format!(
                "{}:{}",
                &normalized[1..2],
                &normalized[2..]
            )));
        }
        if normalized.len() == 2
            && normalized.starts_with('\\')
            && normalized.as_bytes()[1].is_ascii_alphabetic()
        {
            return PathBuf::from(format!(
                "{}:\\",
                normalized.as_bytes()[1].to_ascii_uppercase() as char
            ));
        }
        normalize_path(&PathBuf::from(normalized))
    }
    #[cfg(not(windows))]
    {
        if value.is_empty() {
            PathBuf::from("/")
        } else {
            normalize_path(&PathBuf::from(value))
        }
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

fn expand_home(value: &str) -> Option<String> {
    if value != "~" && !value.starts_with("~/") && !value.starts_with("~\\") {
        return None;
    }
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE")
            .ok()
            .or_else(|| std::env::var("HOME").ok())
    } else {
        std::env::var("HOME").ok()
    }?;
    if value == "~" {
        Some(home)
    } else {
        Some(
            PathBuf::from(home)
                .join(&value[2..])
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn virtual_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.len() >= 2 && text.as_bytes()[1] == b':' {
            let letter = text[..1].to_ascii_uppercase();
            return if text[2..].trim_matches('/').is_empty() {
                format!("/{letter}")
            } else {
                format!("/{letter}{}", &text[2..])
            };
        }
        text
    }
    #[cfg(not(windows))]
    path.to_string_lossy().into_owned()
}

fn list(path: String) -> Result<Vec<u8>, FileErr> {
    #[cfg(windows)]
    if path.trim() == "/" {
        let mut entries = Vec::new();
        for native in windows_drives() {
            entries.push(describe(&native)?);
        }
        return Ok(json_array(entries));
    }
    let path = resolve_path(&path);
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let Ok(entry) = entry else {
            continue;
        };
        let is_dir = fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
        let Ok(description) = describe(&entry.path()) else {
            continue;
        };
        entries.push((
            !is_dir,
            entry.file_name().to_string_lossy().to_lowercase(),
            description,
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
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| {
            #[cfg(windows)]
            if let Some(letter) = windows_drive_letter(path) {
                return letter.to_string().into();
            }
            "/".into()
        });
    crate::json::write_json_string(&mut out, &name);
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
    {
        let mode = if meta.permissions().readonly() {
            if meta.is_dir() { "0555" } else { "0444" }
        } else if meta.is_dir() {
            "0777"
        } else {
            "0666"
        };
        out.extend_from_slice(
            format!(",\"mode_octal\":\"{mode}\",\"uid\":-1,\"gid\":-1").as_bytes(),
        );
    }
    let modified = meta.modified().ok().map(rfc3339_from_system_time);
    out.extend_from_slice(b",\"mode\":");
    crate::json::write_json_string(&mut out, &mode_string(&meta));
    out.extend_from_slice(b",\"owner\":");
    crate::json::write_json_string(&mut out, &owner_name(&meta));
    out.extend_from_slice(b",\"group\":");
    crate::json::write_json_string(&mut out, &group_name(&meta));
    out.extend_from_slice(b",\"modified_at\":");
    crate::json::write_json_string(
        &mut out,
        &modified.unwrap_or_else(|| "1970-01-01T00:00:00Z".into()),
    );
    if is_symlink && let Ok(target) = fs::read_link(path) {
        out.extend_from_slice(b",\"target\":");
        crate::json::write_json_string(&mut out, &target.to_string_lossy());
    }
    out.push(b'}');
    Ok(out)
}

fn mode_string(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let mode = metadata.mode();
        let file_type = metadata.file_type();
        let mut value = String::with_capacity(10);
        if metadata.is_dir() {
            value.push('d');
        } else if file_type.is_symlink() {
            value.push('L');
        } else if file_type.is_block_device() {
            value.push('D');
        } else if file_type.is_char_device() {
            value.push_str("Dc");
        } else if file_type.is_fifo() {
            value.push('p');
        } else if file_type.is_socket() {
            value.push('S');
        }
        if mode & 0o4000 != 0 {
            value.push('u');
        }
        if mode & 0o2000 != 0 {
            value.push('g');
        }
        if mode & 0o1000 != 0 {
            value.push('t');
        }
        if value.is_empty() {
            value.push('-');
        }
        for (read, write, execute) in [(8, 7, 6), (5, 4, 3), (2, 1, 0)] {
            value.push(if mode & (1 << read) != 0 { 'r' } else { '-' });
            value.push(if mode & (1 << write) != 0 { 'w' } else { '-' });
            value.push(if mode & (1 << execute) != 0 { 'x' } else { '-' });
        }
        value
    }
    #[cfg(not(unix))]
    {
        let kind = if metadata.is_dir() {
            'd'
        } else if metadata.file_type().is_symlink() {
            'L'
        } else {
            '-'
        };
        let permissions = if metadata.permissions().readonly() {
            if metadata.is_dir() {
                "r-xr-xr-x"
            } else {
                "r--r--r--"
            }
        } else if metadata.is_dir() {
            "rwxrwxrwx"
        } else {
            "rw-rw-rw-"
        };
        format!("{kind}{permissions}")
    }
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

fn rfc3339_from_system_time(value: SystemTime) -> String {
    let duration = value.duration_since(UNIX_EPOCH).unwrap_or_default();
    rfc3339_from_unix_nanos(duration.as_secs(), duration.subsec_nanos())
}

fn rfc3339_from_unix_nanos(seconds: u64, nanos: u32) -> String {
    let days = (seconds / 86400) as i64;
    let remaining = seconds % 86400;
    let (year, month, day) = civil_from_days(days);
    let mut value = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remaining / 3600,
        (remaining % 3600) / 60,
        remaining % 60
    );
    if nanos != 0 {
        let fraction = format!("{nanos:09}").trim_end_matches('0').to_string();
        value.insert(value.len() - 1, '.');
        value.insert_str(value.len() - 1, &fraction);
    }
    value
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
    if path == Path::new(".") || path.parent().is_none() || path.as_os_str().is_empty() {
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
    let mut cleanup = RemoveOnDrop::new(stage.clone());
    let prior_permissions = match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(FileErr::Invalid("cannot replace a directory with a file"));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if prior_permissions.is_none() {
            options.mode(0o644);
        }
    }
    let file = options.open(&stage)?;
    if let Some(permissions) = prior_permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    drop(file);
    replace(&stage, path)?;
    cleanup.disarm();
    sync_parent(path)?;
    Ok(br#"{"created":true,"size":0}"#.to_vec())
}

fn mkdir(path: &Path, mode: &str) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(path)?;
    let mode = if mode.trim().is_empty() {
        0o755
    } else {
        parse_mode(mode)?
    };
    if fs::symlink_metadata(path).is_ok() {
        fs::create_dir_all(path)?;
        return Ok(br#"{"created":true}"#.to_vec());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(mode);
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::create_dir_all(path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        fs::create_dir_all(path)?;
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
    if same_path(source, destination) {
        return Ok(br#"{"moved":true}"#.to_vec());
    }
    if path_contains(source, destination) {
        return Err(FileErr::Invalid("destination is inside source"));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => {}
        Err(error) if is_cross_device(&error) => {
            copy_recursive(source, destination)?;
            let metadata = fs::symlink_metadata(source)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(source)?;
            } else {
                fs::remove_file(source)?;
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(br#"{"moved":true}"#.to_vec())
}

fn is_cross_device(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(18) // EXDEV
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

fn copy_path(source: &Path, destination: &Path) -> Result<Vec<u8>, FileErr> {
    ensure_not_root(destination)?;
    if same_path(source, destination) {
        return Err(FileErr::Invalid("source and destination are the same"));
    }
    if path_contains(source, destination) {
        return Err(FileErr::Invalid("destination is inside source"));
    }
    copy_recursive(source, destination)?;
    Ok(br#"{"copied":true}"#.to_vec())
}

fn copy_recursive(source: &Path, destination: &Path) -> Result<(), FileErr> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        prepare_symlink_destination(destination)?;
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
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, meta.permissions())?;
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn prepare_symlink_destination(destination: &Path) -> Result<(), FileErr> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let Ok(existing) = fs::symlink_metadata(destination) else {
        return Ok(());
    };
    if existing.is_dir() && !existing.file_type().is_symlink() {
        return Err(FileErr::Invalid(
            "cannot replace destination directory with a symlink",
        ));
    }
    #[cfg(windows)]
    {
        if existing.file_type().is_symlink()
            && fs::metadata(destination).is_ok_and(|metadata| metadata.is_dir())
        {
            fs::remove_dir(destination)?;
            return Ok(());
        }
    }
    fs::remove_file(destination)?;
    Ok(())
}

fn chmod(path: &Path, mode: &str) -> Result<Vec<u8>, FileErr> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = parse_mode(mode)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(format!("{{\"mode\":\"{mode:04o}\"}}").into_bytes())
    }
    #[cfg(not(unix))]
    {
        let mode = parse_mode(mode)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(mode & 0o200 == 0);
        fs::set_permissions(path, permissions)?;
        Ok(format!("{{\"mode\":\"{mode:04o}\"}}").into_bytes())
    }
}

fn parse_mode(value: &str) -> Result<u32, FileErr> {
    let mode = u32::from_str_radix(value.trim().trim_start_matches("0o"), 8)
        .map_err(|_| FileErr::Invalid("invalid mode"))?;
    if mode > 0o7777 {
        return Err(FileErr::Invalid("invalid mode"));
    }
    Ok(mode)
}

fn chown(path: &Path, args: &[u8]) -> Result<Vec<u8>, FileErr> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut uid = direct_i64(args, "uid").unwrap_or(-1);
        let mut gid = direct_i64(args, "gid").unwrap_or(-1);
        if let Some(owner) = direct_string(args, "owner").filter(|value| !value.trim().is_empty()) {
            uid = resolve_account(&owner, true).ok_or(FileErr::Invalid("unknown owner account"))?;
        }
        if let Some(group) = direct_string(args, "group").filter(|value| !value.trim().is_empty()) {
            gid =
                resolve_account(&group, false).ok_or(FileErr::Invalid("unknown group account"))?;
        }
        if uid < -1
            || gid < -1
            || uid > i64::from(u32::MAX)
            || gid > i64::from(u32::MAX)
            || (uid == -1 && gid == -1)
        {
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
    if value.trim() == "-" {
        return Some(-1);
    }
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
    let requested_root = path_arg(args, "path");
    let query = path_arg(args, "query");
    if query.is_empty() {
        return Err(FileErr::Invalid("query is required"));
    }
    let content = direct_bool(args, "content");
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();
    #[cfg(windows)]
    let roots = {
        if requested_root.trim() == "/" {
            windows_drives()
        } else {
            vec![resolve_path(&requested_root)]
        }
    };
    #[cfg(not(windows))]
    let roots = vec![resolve_path(&requested_root)];
    for root in roots {
        walk(&root, &mut |path, meta| {
            if results.len() >= MAX_SEARCH_RESULTS {
                return Ok(false);
            }
            let name_match = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&query_lower));
            if name_match && !content {
                results.push(search_result(path, 0, "", meta.is_dir()));
            } else if content
                && meta.is_file()
                && meta.len() <= 10 * 1024 * 1024
                && !meta.file_type().is_symlink()
                && let Some((line, text)) = content_match(path, &query_lower)
            {
                results.push(search_result(path, line, &text, false));
            }
            Ok(true)
        })?;
        if results.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }
    let mut out = b"{\"matches\":[".to_vec();
    out.extend_from_slice(results.join(",").as_bytes());
    out.extend_from_slice(
        format!("],\"limited\":{}}}", results.len() >= MAX_SEARCH_RESULTS).as_bytes(),
    );
    Ok(out)
}

fn content_match(path: &Path, query_lower: &str) -> Option<(usize, String)> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut raw = Vec::new();
    let mut line_number = 0;
    loop {
        raw.clear();
        let read = reader.read_until(b'\n', &mut raw).ok()?;
        if read == 0 {
            return None;
        }
        line_number += 1;
        if raw.last() == Some(&b'\n') {
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
        }
        let line = String::from_utf8_lossy(&raw);
        if line.to_lowercase().contains(query_lower) {
            return Some((
                line_number,
                line.chars().take(MAX_SEARCH_LINE).collect::<String>(),
            ));
        }
    }
}

#[cfg(windows)]
fn windows_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .map(|drive| PathBuf::from(format!("{}:\\", drive as char)))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(windows)]
fn windows_drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next()? == ':' && chars.next()? == '\\')
        .then(|| letter.to_ascii_uppercase())
}

fn search_result(path: &Path, line: usize, text: &str, is_dir: bool) -> String {
    let mut out = b"{\"path\":".to_vec();
    crate::json::write_json_string(&mut out, &virtual_path(path));
    out.extend_from_slice(format!(",\"line\":{line},\"is_dir\":{is_dir}").as_bytes());
    if !text.is_empty() {
        out.extend_from_slice(b",\"text\":");
        crate::json::write_json_string(&mut out, text);
    }
    out.push(b'}');
    String::from_utf8(out).unwrap()
}

fn walk(
    path: &Path,
    visit: &mut impl FnMut(&Path, &fs::Metadata) -> Result<bool, FileErr>,
) -> Result<(), FileErr> {
    walk_inner(path, visit, false)
}

fn walk_inner(
    path: &Path,
    visit: &mut impl FnMut(&Path, &fs::Metadata) -> Result<bool, FileErr>,
    tolerate_entries: bool,
) -> Result<(), FileErr> {
    let meta = fs::symlink_metadata(path)?;
    if !visit(path, &meta)? {
        return Ok(());
    }
    if meta.is_dir() && !meta.file_type().is_symlink() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_error) if tolerate_entries => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries.flatten() {
            if walk_inner(&entry.path(), visit, true).is_err() {
                continue;
            }
        }
    }
    Ok(())
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
    let _stream = StreamGuard::acquire()?;
    let path = resolve_path(&path_arg(args, "path"));
    let offset = u64_arg(args, "offset")?;
    let length = u64_arg(args, "length")?;
    if length == 0 || length > MAX_CHUNK {
        return Err(FileErr::Invalid("invalid download range"));
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        return Err(FileErr::Invalid("cannot download a directory"));
    }
    let size = metadata.len();
    if direct_i64(args, "file_size").is_some_and(|expected| expected > 0 && expected as u64 != size)
    {
        return Err(FileErr::Invalid("file changed while opening stream"));
    }
    if let Some(expected) = direct_string(args, "modified_at") {
        let actual = metadata
            .modified()
            .ok()
            .map(rfc3339_from_system_time)
            .unwrap_or_default();
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
    let _stream = StreamGuard::acquire()?;
    let id = path_arg(args, "upload_id");
    let target = resolve_path(&path_arg(args, "path"));
    ensure_not_root(&target)?;
    let offset = u64_arg(args, "offset")?;
    let index = u64_arg(args, "chunk_index")?;
    let count = u64_arg(args, "chunk_count")?;
    let total = u64_arg(args, "total_size")?;
    let chunk = direct_i64(args, "chunk_size").unwrap_or(25 * 1024 * 1024) as u64;
    let Some((expected_offset, expected)) = validated_chunk(index, count, total, chunk) else {
        return Err(FileErr::Invalid("invalid upload metadata"));
    };
    if !valid_upload_id(&id) || offset != expected_offset {
        return Err(FileErr::Invalid("invalid upload metadata"));
    }
    if upload_is_canceled(&id)? {
        return Err(FileErr::Invalid("upload session was cancelled"));
    }
    let (stage, canceled) = {
        let mut map = uploads()
            .lock()
            .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
        prune_uploads(&mut map);
        if let Some(state) = map.get_mut(&id) {
            if !same_path(&state.target, &target)
                || state.total != total
                || state.chunk_size != chunk
                || state.chunk_count != count
            {
                return Err(FileErr::Invalid("upload session metadata changed"));
            }
            state.updated_at = Instant::now();
            (state.stage.clone(), Arc::clone(&state.canceled))
        } else {
            if index != 0 {
                return Err(FileErr::Invalid("unknown upload session"));
            }
            if map.len() >= MAX_UPLOAD_SESSIONS {
                return Err(FileErr::Invalid("too many active upload sessions"));
            }
            let stage = upload_part_path(&target, &id);
            if let Some(parent) = stage.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::remove_file(&stage) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let stage_file = options.open(&stage)?;
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
                    updated_at: Instant::now(),
                },
            );
            (stage, canceled)
        }
    };
    if upload_is_canceled(&id)? {
        canceled.store(true, Ordering::Release);
        let mut map = uploads()
            .lock()
            .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
        if let Some(state) = map.remove(&id) {
            let _ = fs::remove_file(state.stage);
        }
        return Err(FileErr::Invalid("upload session was cancelled"));
    }
    let headers = transfer_headers(args);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let chunk_stage = stage_path(&target, &format!("chunk-{id}-{index}-{sequence}"));
    if let Some(parent) = chunk_stage.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let received = options.open(&chunk_stage)?;
    let mut cleanup = RemoveOnDrop::new(chunk_stage.clone());
    let mut received = CancelWriter { received, canceled };
    let response = crate::http::http_post_stream(
        &transfer_url(config, args)?,
        &mut io::empty(),
        0,
        &mut received,
        expected,
        None,
        "application/octet-stream",
        &headers,
        tls,
        dial,
        STREAM_TIMEOUT,
    )
    .map_err(|e| FileErr::Message(e.to_string()))?;
    if response.status_code != 200 {
        return Err(FileErr::Invalid("upload transfer was rejected"));
    }
    if response.body_len != expected {
        return Err(FileErr::Invalid("upload transfer returned invalid body"));
    }
    if upload_is_canceled(&id)? {
        return Err(FileErr::Invalid("upload session was cancelled"));
    }
    received.received.sync_all()?;
    drop(received);
    let mut file = OpenOptions::new().write(true).open(&stage)?;
    let mut received = File::open(&chunk_stage)?;
    file.seek(SeekFrom::Start(offset))?;
    let written = io::copy(&mut received, &mut file)?;
    if written != expected {
        return Err(FileErr::Invalid("upload transfer was truncated"));
    }
    file.sync_data()?;
    drop(file);
    drop(received);
    fs::remove_file(&chunk_stage)?;
    cleanup.disarm();
    let mut map = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
    let state = map.get_mut(&id).ok_or_else(|| {
        let _ = fs::remove_file(&stage);
        FileErr::Invalid("upload session was cancelled")
    })?;
    state.received.insert(index);
    state.updated_at = Instant::now();
    Ok(format!(
        "{{\"received\":{},\"offset\":{}}}",
        written,
        offset + expected
    )
    .into_bytes())
}

fn upload_commit(args: &[u8]) -> Result<Vec<u8>, FileErr> {
    let id = path_arg(args, "upload_id");
    if !valid_upload_id(&id) {
        return Err(FileErr::Invalid("invalid upload_id"));
    }
    let mut map = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
    prune_uploads(&mut map);
    let state = map
        .get(&id)
        .ok_or(FileErr::Invalid("unknown upload session"))?;
    let path = resolve_path(&path_arg(args, "path"));
    ensure_not_root(&path)?;
    let total = u64_arg(args, "total_size")?;
    let chunk_size = u64_arg(args, "chunk_size")?;
    let chunk_count = u64_arg(args, "chunk_count")?;
    if !same_path(&state.target, &path)
        || state.total != total
        || state.chunk_size != chunk_size
        || state.chunk_count != chunk_count
    {
        return Err(FileErr::Invalid("upload commit metadata changed"));
    }
    if state.received.len() as u64 != state.chunk_count {
        return Err(FileErr::Invalid("upload is incomplete"));
    }
    if let Ok(metadata) = fs::metadata(&state.target)
        && metadata.is_dir()
    {
        return Err(FileErr::Invalid("cannot replace a directory with a file"));
    }
    let file = OpenOptions::new().write(true).open(&state.stage)?;
    if file.metadata()?.len() < state.total {
        return Err(FileErr::Invalid("upload staging file is incomplete"));
    }
    file.set_len(state.total)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&state.target)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o644);
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    if let Ok(metadata) = fs::metadata(&state.target) {
        file.set_permissions(metadata.permissions())?;
    }
    file.sync_all()?;
    drop(file);
    replace(&state.stage, &state.target)?;
    let state = map
        .remove(&id)
        .ok_or(FileErr::Invalid("unknown upload session"))?;
    clear_upload_canceled(&id)?;
    sync_parent(&state.target)?;
    Ok(format!(
        "{{\"received\":{},\"final\":true,\"offset\":{}}}",
        state.total, state.total
    )
    .into_bytes())
}

fn upload_cancel(args: &[u8]) -> Result<Vec<u8>, FileErr> {
    let id = path_arg(args, "upload_id");
    if !valid_upload_id(&id) {
        return Err(FileErr::Invalid("invalid upload_id"));
    }
    let requested_path = path_arg(args, "path");
    let mut map = uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload state unavailable"))?;
    prune_uploads(&mut map);
    let requested_path = (!requested_path.is_empty()).then(|| resolve_path(&requested_path));
    if let Some(state) = map.get(&id)
        && requested_path
            .as_ref()
            .is_some_and(|path| !same_path(path, &state.target))
    {
        return Err(FileErr::Invalid(
            "upload target path does not match session",
        ));
    }
    mark_upload_canceled(&id)?;
    if let Some(state) = map.remove(&id) {
        state.canceled.store(true, Ordering::Release);
        remove_if_exists(&state.stage)?;
    } else if let Some(path) = requested_path {
        remove_if_exists(&upload_part_path(&path, &id))?;
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

struct RemoveOnDrop {
    path: PathBuf,
    armed: bool,
}

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct StreamGuard;

impl StreamGuard {
    fn acquire() -> Result<Self, FileErr> {
        ACTIVE_STREAMS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_STREAMS).then_some(active + 1)
            })
            .map_err(|_| FileErr::Invalid("file stream limit reached"))?;
        Ok(Self)
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        ACTIVE_STREAMS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn u64_arg(args: &[u8], key: &str) -> Result<u64, FileErr> {
    direct_i64(args, key)
        .filter(|v| *v >= 0)
        .map(|v| v as u64)
        .ok_or(FileErr::Invalid("missing numeric field"))
}

fn validated_chunk(index: u64, count: u64, total: u64, chunk: u64) -> Option<(u64, u64)> {
    if total == 0 || count == 0 || chunk == 0 || chunk > MAX_CHUNK || count != total.div_ceil(chunk)
    {
        return None;
    }
    let offset = index.checked_mul(chunk)?;
    if index >= count || offset >= total {
        return None;
    }
    Some((offset, chunk.min(total - offset)))
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    let mut parent = parent.components();
    let mut child = child.components();
    loop {
        match parent.next() {
            Some(expected) => match child.next() {
                Some(actual) if path_component_eq(expected, actual) => {}
                _ => return false,
            },
            None => return child.next().is_some(),
        }
    }
}

fn path_component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn valid_upload_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn prune_uploads(map: &mut HashMap<String, UploadState>) {
    let now = Instant::now();
    let expired: Vec<_> = map
        .iter()
        .filter(|(_, state)| now.duration_since(state.updated_at) >= UPLOAD_IDLE_TIMEOUT)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        if let Some(state) = map.remove(&id) {
            state.canceled.store(true, Ordering::Release);
            let _ = fs::remove_file(state.stage);
        }
    }
}

fn prune_canceled_uploads(map: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    map.retain(|_, updated_at| now.duration_since(*updated_at) < UPLOAD_CANCEL_TTL);
}

fn upload_is_canceled(id: &str) -> Result<bool, FileErr> {
    let mut canceled = canceled_uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?;
    prune_canceled_uploads(&mut canceled);
    Ok(canceled.contains_key(id))
}

fn mark_upload_canceled(id: &str) -> Result<(), FileErr> {
    let mut canceled = canceled_uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?;
    prune_canceled_uploads(&mut canceled);
    if canceled.len() >= MAX_CANCELED_UPLOADS
        && !canceled.contains_key(id)
        && let Some(oldest) = canceled
            .iter()
            .min_by_key(|(_, updated_at)| **updated_at)
            .map(|(id, _)| id.clone())
    {
        canceled.remove(&oldest);
    }
    canceled.insert(id.to_string(), Instant::now());
    Ok(())
}

fn clear_upload_canceled(id: &str) -> Result<(), FileErr> {
    canceled_uploads()
        .lock()
        .map_err(|_| FileErr::Invalid("upload cancellation state unavailable"))?
        .remove(id);
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), FileErr> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn stage_path(path: &Path, kind: &str) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    path.with_file_name(format!(".{name}.komari-{kind}-{}", std::process::id()))
}

fn upload_part_path(path: &Path, id: &str) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    path.with_file_name(format!(".{name}.komari-upload-{id}.part"))
}

fn sync_parent(path: &Path) -> Result<(), FileErr> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(stage: &Path, target: &Path) -> Result<(), FileErr> {
    fs::rename(stage, target)?;
    Ok(())
}

#[cfg(windows)]
fn replace(stage: &Path, target: &Path) -> Result<(), FileErr> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let stage: Vec<u16> = stage
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replaced = unsafe {
        MoveFileExW(
            stage.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

fn result_payload(uuid: &str, request_id: &str, result: Result<Vec<u8>, FileErr>) -> Vec<u8> {
    let mut out = b"{\"uuid\":".to_vec();
    crate::json::write_json_string(&mut out, uuid);
    out.extend_from_slice(b",\"request_id\":");
    crate::json::write_json_string(&mut out, request_id);
    let prefix_len = out.len();
    match result {
        Ok(value) => {
            out.extend_from_slice(b",\"ok\":true,\"result\":");
            out.extend_from_slice(&value);
            if out.len() > MAX_CONTROL_JSON {
                out.truncate(prefix_len);
                out.extend_from_slice(
                    b",\"ok\":false,\"error\":\"file operation result exceeds 1 MiB\"",
                );
            }
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
    fn filesystem_root_and_current_directory_are_rejected_as_targets() {
        assert!(ensure_not_root(Path::new("/")).is_err());
        assert!(ensure_not_root(Path::new(".")).is_err());
    }

    #[test]
    fn file_operations_reject_roots_and_commit_atomically() {
        let root = std::env::temp_dir().join(format!("komari-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        create_empty(&file).unwrap();
        fs::write(&file, b"old").unwrap();
        create_empty(&file).unwrap();
        assert!(fs::read(&file).unwrap().is_empty());
        fs::write(&file, b"old").unwrap();
        let copy = root.join("b.txt");
        copy_path(&file, &copy).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"old");
        assert!(move_path(&copy, &copy).is_ok());
        assert!(copy_path(&copy, &copy).is_err());
        assert!(delete(Path::new("/")).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_rejects_an_existing_directory_target() {
        let root = std::env::temp_dir().join(format!(
            "komari-create-directory-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(create_empty(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn path_cleaning_prevents_destination_containment_bypasses() {
        let root = std::env::temp_dir().join(format!(
            "komari-path-clean-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let source = resolve_path(&root.join("source").to_string_lossy());
        let destination = resolve_path(
            &root
                .join("unrelated")
                .join("..")
                .join("source")
                .join("child")
                .to_string_lossy(),
        );
        assert!(path_contains(&source, &destination));
        assert!(!path_contains(&destination, &source));
    }

    #[test]
    fn upload_ids_cannot_escape_the_staging_directory() {
        assert!(valid_upload_id("8a12e3d4-abc_09"));
        assert!(!valid_upload_id(""));
        assert!(!valid_upload_id("../escape"));
        assert!(!valid_upload_id("dir/name"));
        assert!(!valid_upload_id("name.part"));
        assert!(!valid_upload_id(&"a".repeat(129)));
    }

    #[test]
    fn upload_metadata_rejects_unreachable_or_overflowing_chunks() {
        let chunk = 1024 * 1024;
        assert_eq!(validated_chunk(0, 2, chunk + 123, chunk), Some((0, chunk)));
        assert_eq!(
            validated_chunk(1, 2, chunk + 123, chunk),
            Some((chunk, 123))
        );
        assert_eq!(validated_chunk(2, 2, chunk + 123, chunk), None);
        assert_eq!(validated_chunk(1, 2, 1, 1), None);
        assert_eq!(validated_chunk(u64::MAX, 1, u64::MAX, u64::MAX), None);
    }

    #[test]
    fn modified_timestamp_preserves_subseconds_without_trailing_zeroes() {
        assert_eq!(
            rfc3339_from_unix_nanos(1_700_000_000, 123_400_000),
            "2023-11-14T22:13:20.1234Z"
        );
        assert_eq!(
            rfc3339_from_unix_nanos(1_700_000_000, 0),
            "2023-11-14T22:13:20Z"
        );
    }

    #[test]
    fn content_search_continues_after_non_utf8_lines() {
        let root = std::env::temp_dir().join(format!(
            "komari-search-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("mixed.txt");
        fs::write(&file, b"\xff\xfe\nneedle\n").unwrap();
        assert_eq!(
            content_match(&file, "needle"),
            Some((2, String::from("needle")))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_reports_a_missing_explicit_root() {
        let missing = std::env::temp_dir().join(format!(
            "komari-search-missing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut args = b"{\"query\":\"needle\",\"path\":".to_vec();
        crate::json::write_json_string(&mut args, &missing.to_string_lossy());
        args.push(b'}');
        assert!(search(&args).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_root_keeps_the_two_character_virtual_form() {
        let root = resolve_path("/c");
        assert_eq!(virtual_path(&root), "/C");
        assert!(same_path(&root, &resolve_path("/C")));
        let description = String::from_utf8(describe(&root).unwrap()).unwrap();
        assert!(description.contains("\"name\":\"C\""));
        assert!(description.contains("\"path\":\"/C\""));
    }

    #[cfg(windows)]
    #[test]
    fn windows_chmod_maps_write_bit_to_the_readonly_attribute() {
        let root = std::env::temp_dir().join(format!(
            "komari-chmod-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        fs::write(&file, b"data").unwrap();
        chmod(&file, "0444").unwrap();
        assert!(fs::metadata(&file).unwrap().permissions().readonly());
        chmod(&file, "0644").unwrap();
        assert!(!fs::metadata(&file).unwrap().permissions().readonly());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn upload_cancel_path_mismatch_keeps_the_session_active() {
        let root = std::env::temp_dir().join(format!(
            "komari-upload-cancel-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let id = format!("cancel-{}", std::process::id());
        let target = root.join("target.bin");
        let stage = root.join("stage.bin");
        fs::write(&stage, b"partial").unwrap();
        uploads().lock().unwrap().insert(
            id.clone(),
            UploadState {
                target: target.clone(),
                stage: stage.clone(),
                total: 7,
                chunk_size: 7,
                chunk_count: 1,
                received: HashSet::new(),
                canceled: Arc::new(AtomicBool::new(false)),
                updated_at: Instant::now(),
            },
        );
        canceled_uploads().lock().unwrap().remove(&id);

        let wrong_path = root.join("other.bin");
        let mut args = b"{\"upload_id\":".to_vec();
        crate::json::write_json_string(&mut args, &id);
        args.extend_from_slice(b",\"path\":");
        crate::json::write_json_string(&mut args, &wrong_path.to_string_lossy());
        args.push(b'}');

        assert!(upload_cancel(&args).is_err());
        assert!(uploads().lock().unwrap().contains_key(&id));
        assert!(!canceled_uploads().lock().unwrap().contains_key(&id));
        assert!(stage.exists());

        uploads().lock().unwrap().remove(&id);
        canceled_uploads().lock().unwrap().remove(&id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn expired_upload_is_cancelled_and_stage_is_removed() {
        let root = std::env::temp_dir().join(format!(
            "komari-upload-prune-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target.bin");
        let stage = root.join("stage.bin");
        fs::write(&stage, b"partial").unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let id = "expired-upload".to_string();
        let mut map = HashMap::from([(
            id.clone(),
            UploadState {
                target,
                stage: stage.clone(),
                total: 7,
                chunk_size: 7,
                chunk_count: 1,
                received: HashSet::new(),
                canceled: Arc::clone(&canceled),
                updated_at: Instant::now() - UPLOAD_IDLE_TIMEOUT,
            },
        )]);
        prune_uploads(&mut map);
        assert!(!map.contains_key(&id));
        assert!(canceled.load(Ordering::Acquire));
        assert!(!stage.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn upload_commit_preserves_existing_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = std::env::temp_dir().join(format!(
            "komari-upload-commit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target.bin");
        let stage = root.join("stage.bin");
        fs::write(&target, b"old").unwrap();
        fs::write(&stage, b"payload").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let id = format!("commit-{}", std::process::id());
        uploads().lock().unwrap().insert(
            id.clone(),
            UploadState {
                target: target.clone(),
                stage,
                total: 7,
                chunk_size: 7,
                chunk_count: 1,
                received: HashSet::from([0]),
                canceled: Arc::new(AtomicBool::new(false)),
                updated_at: Instant::now(),
            },
        );
        let mut args = b"{\"upload_id\":".to_vec();
        crate::json::write_json_string(&mut args, &id);
        args.extend_from_slice(
            format!(
                ",\"path\":\"{}\",\"total_size\":7,\"chunk_size\":7,\"chunk_count\":1}}",
                target.display()
            )
            .as_bytes(),
        );
        upload_commit(&args).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"payload");
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o640);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copying_a_symlink_preserves_the_link_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "komari-copy-link-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let referent = root.join("referent.txt");
        let source = root.join("source.txt");
        let copy = root.join("copy.txt");
        fs::write(&referent, b"referent").unwrap();
        fs::write(&copy, b"old").unwrap();
        symlink("referent.txt", &source).unwrap();
        copy_path(&source, &copy).unwrap();
        assert!(
            fs::symlink_metadata(&copy)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&copy).unwrap(), PathBuf::from("referent.txt"));
        let description = String::from_utf8(describe(&source).unwrap()).unwrap();
        assert!(description.contains("\"target\":\"referent.txt\""));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn mkdir_does_not_change_existing_directory_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = std::env::temp_dir().join(format!(
            "komari-mkdir-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        mkdir(&root, "0755").unwrap();
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn account_resolution_rejects_unknown_names_but_accepts_unchanged_marker() {
        assert_eq!(resolve_account("-", true), Some(-1));
        assert_eq!(
            resolve_account("komari-no-such-account-20261022", true),
            None
        );
    }

    #[test]
    fn oversized_results_are_reported_as_errors_instead_of_null_success() {
        let payload = result_payload("uuid", "request", Ok(vec![b'x'; MAX_CONTROL_JSON]));
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("\"ok\":false"));
        assert!(text.contains("file operation result exceeds 1 MiB"));
        assert!(!text.contains("\"ok\":true"));
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
