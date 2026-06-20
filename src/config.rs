// komari-agent-rs: hand-rolled CLI + env + JSON config parser.
// DD1 in spec.md: no clap, no serde. ~600 lines total.
//
// Config order matches Go flag.go struct exactly, plus user-requested fields:
//   debug_log, exclude_mountpoints
//
// References:
//   D:/Code/Projects/external/komari-agent-go/cmd/flags/flag.go  (struct + tags)
//   D:/Code/Projects/external/komari-agent-go/cmd/root.go        (cobra init + env loader)
//   D:/Code/Projects/edgehub/komari-agent-rs/docs/plan/spec.md   (DD1 constraint)

use std::env;
use std::fmt;
use std::fs;
use std::str::FromStr;

// ============================================================================
// ConfigErr — unified error type for all config operations
// ============================================================================

#[derive(Debug)]
pub enum ConfigErr {
    /// A flag that requires a value was provided without one.
    MissingValue(String),
    /// A flag value could not be parsed (e.g. "abc" for --interval).
    InvalidValue {
        flag: String,
        value: String,
        reason: String,
    },
    /// The config file path does not exist.
    FileNotFound(String),
    /// The config file could not be read.
    FileReadError {
        path: String,
        error: String,
    },
    /// The config file contains invalid JSON.
    JsonError(String),
    /// Post-load validation failed (e.g. missing required fields).
    Validation(String),
}

impl fmt::Display for ConfigErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "missing value for flag '{}'", flag),
            Self::InvalidValue {
                flag,
                value,
                reason,
            } => write!(
                f,
                "invalid value '{}' for flag '{}': {}",
                value, flag, reason
            ),
            Self::FileNotFound(path) => write!(f, "config file not found: {}", path),
            Self::FileReadError { path, error } => {
                write!(f, "failed to read config file '{}': {}", path, error)
            }
            Self::JsonError(msg) => write!(f, "JSON parse error: {}", msg),
            Self::Validation(msg) => write!(f, "config validation error: {}", msg),
        }
    }
}

impl std::error::Error for ConfigErr {}

// ============================================================================
// Config — all 32 fields matching Go agent + user-requested additions
// ============================================================================

pub struct Config {
    // -- Required (no defaults) --
    pub endpoint: String, // AGENT_ENDPOINT, --endpoint / -e
    pub token: String,    // AGENT_TOKEN,    --token / -t

    // -- Timing --
    pub interval: f64,             // AGENT_INTERVAL,             --interval / -i,          default 1.0
    pub info_report_interval: u64, // AGENT_INFO_REPORT_INTERVAL, --info-report-interval,   default 5 (min)
    pub reconnect_interval: u64,   // AGENT_RECONNECT_INTERVAL,   --reconnect-interval / -c, default 5 (sec)
    pub max_retries: u64,          // AGENT_MAX_RETRIES,          --max-retries / -r,       default 10

    // -- Feature toggles --
    pub disable_web_ssh: bool,    // AGENT_DISABLE_WEB_SSH,    --disable-web-ssh,         default true
    pub disable_auto_update: bool, // AGENT_DISABLE_AUTO_UPDATE, --disable-auto-update,     default true
    pub disable_compression: bool, // AGENT_DISABLE_COMPRESSION, --disable-compression,     default false
    pub enable_gpu: bool,          // AGENT_ENABLE_GPU,          --gpu,                     default false
    pub ignore_unsafe_cert: bool,  // AGENT_IGNORE_UNSAFE_CERT,  --ignore-unsafe-cert / -u, default false
    pub debug_log: bool,           // AGENT_DEBUG_LOG,           --debug-log,               default false
    pub show_warning: bool,        // AGENT_SHOW_WARNING,        --show-warning,            default false
    pub get_ip_addr_from_nic: bool, // AGENT_GET_IP_ADDR_FROM_NIC, --get-ip-addr-from-nic,  default false
    pub memory_include_cache: bool, // AGENT_MEMORY_INCLUDE_CACHE, --memory-include-cache,  default false
    pub memory_report_raw_used: bool, // AGENT_MEMORY_REPORT_RAW_USED, --memory-report-raw-used / --memory-exclude-bcf, default false
    #[allow(dead_code)]
    pub memory_mode_available: bool, // AGENT_MEMORY_MODE_AVAILABLE, deprecated,             default false

    // -- Network --
    pub prefer_ip_version: String, // AGENT_PREFER_IP_VERSION, --prefer-ip-version,       default ""
    pub custom_ipv4: String,       // AGENT_CUSTOM_IPV4,       --custom-ipv4,             default ""
    pub custom_ipv6: String,       // AGENT_CUSTOM_IPV6,       --custom-ipv6,             default ""
    pub custom_dns: Vec<String>,   // AGENT_CUSTOM_DNS,        --custom-dns,              default [] (comma-sep)

    // -- Cloudflare Access --
    pub cf_access_client_id: String,     // AGENT_CF_ACCESS_CLIENT_ID,     --cf-access-client-id,     default ""
    pub cf_access_client_secret: String, // AGENT_CF_ACCESS_CLIENT_SECRET, --cf-access-client-secret, default ""

    // -- Lists (Go uses comma/semicolon-separated strings; we store as Vec) --
    pub include_nics: Vec<String>,        // AGENT_INCLUDE_NICS,        --include-nics,        default [] (comma-sep)
    pub exclude_nics: Vec<String>,        // AGENT_EXCLUDE_NICS,        --exclude-nics,        default [] (comma-sep)
    pub include_mountpoints: Vec<String>, // AGENT_INCLUDE_MOUNTPOINTS, --include-mountpoints, default [] (semicolon-sep)
    pub exclude_mountpoints: Vec<String>, // AGENT_EXCLUDE_MOUNTPOINTS, --exclude-mountpoints, default [] (semicolon-sep)

    // -- Other --
    pub protocol_version: u8,     // AGENT_PROTOCOL_VERSION, --protocol-version,       default 2
    pub month_rotate: u8,         // AGENT_MONTH_ROTATE,     --month-rotate,           default 0
    pub auto_discovery_key: String, // AGENT_AUTO_DISCOVERY_KEY, --auto-discovery,     default ""
    pub host_proc: String,        // HOST_PROC,              --host-proc,              default ""
    pub config_file: String,      // AGENT_CONFIG_FILE,      --config,                 default ""
}

// ============================================================================
// Default — mirrors user-specified defaults (differs from Go in a few spots)
// ============================================================================

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            token: String::new(),
            interval: 1.0,
            info_report_interval: 5,
            reconnect_interval: 5,
            max_retries: 10,
            disable_web_ssh: true,
            disable_auto_update: true,
            disable_compression: false,
            enable_gpu: false,
            ignore_unsafe_cert: false,
            debug_log: false,
            show_warning: false,
            get_ip_addr_from_nic: false,
            memory_include_cache: false,
            memory_report_raw_used: false,
            memory_mode_available: false,
            prefer_ip_version: String::new(),
            custom_ipv4: String::new(),
            custom_ipv6: String::new(),
            custom_dns: Vec::new(),
            cf_access_client_id: String::new(),
            cf_access_client_secret: String::new(),
            include_nics: Vec::new(),
            exclude_nics: Vec::new(),
            include_mountpoints: Vec::new(),
            exclude_mountpoints: Vec::new(),
            protocol_version: 2,
            month_rotate: 0,
            auto_discovery_key: String::new(),
            host_proc: String::new(),
            config_file: String::new(),
        }
    }
}

// ============================================================================
// FromStr helpers — friendly error messages for CLI/env/JSON value parsing
// ============================================================================

/// Parse a string into f64 with a friendly error message keyed by flag name.
pub fn parse_f64(flag: &str, raw: &str) -> Result<f64, ConfigErr> {
    f64::from_str(raw).map_err(|_| ConfigErr::InvalidValue {
        flag: flag.to_string(),
        value: raw.to_string(),
        reason: "expected a number (e.g. 1.0, 2.5)".to_string(),
    })
}

/// Parse a string into u64 with a friendly error message keyed by flag name.
pub fn parse_u64(flag: &str, raw: &str) -> Result<u64, ConfigErr> {
    u64::from_str(raw).map_err(|_| ConfigErr::InvalidValue {
        flag: flag.to_string(),
        value: raw.to_string(),
        reason: "expected a non-negative integer (e.g. 5, 10)".to_string(),
    })
}

/// Parse a string into u8 with a friendly error message keyed by flag name.
pub fn parse_u8(flag: &str, raw: &str) -> Result<u8, ConfigErr> {
    u8::from_str(raw).map_err(|_| ConfigErr::InvalidValue {
        flag: flag.to_string(),
        value: raw.to_string(),
        reason: "expected an integer in range 0–255".to_string(),
    })
}

/// Parse a string into bool with a friendly error message.
/// Accepts: true/false, 1/0, yes/no, on/off (case-insensitive).
pub fn parse_bool(flag: &str, raw: &str) -> Result<bool, ConfigErr> {
    match raw.to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigErr::InvalidValue {
            flag: flag.to_string(),
            value: raw.to_string(),
            reason: "expected true/false, 1/0, yes/no, or on/off".to_string(),
        }),
    }
}

/// Parse an optional bool value. `None` means the flag was present without a value → true.
pub fn parse_bool_opt(flag: &str, raw: Option<&str>) -> Result<bool, ConfigErr> {
    match raw {
        None => Ok(true),
        Some(v) => parse_bool(flag, v),
    }
}

/// Require a non-empty value for a flag, returning MissingValue if absent.
fn require_val<'a>(flag: &str, raw: Option<&'a str>) -> Result<&'a str, ConfigErr> {
    match raw {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(ConfigErr::MissingValue(flag.to_string())),
    }
}

// ============================================================================
// String → Vec<String> helpers (matching Go separators)
// ============================================================================

/// Split a comma-separated string, trimming whitespace and dropping empty segments.
fn split_comma(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split a semicolon-separated string, trimming whitespace and dropping empty segments.
fn split_semicolon(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ============================================================================
// parse_args — hand-rolled CLI flag parser (~150 lines, no clap)
// ============================================================================
//
// Supports:
//   --flag value        (long with space-separated value)
//   --flag=value        (long with =value)
//   --bool-flag         (long boolean, no value → true)
//   -f value            (short flag)
//   --                  (end of flags)
//
// Unknown flags are silently ignored (matching Go's UnknownFlags = true).

pub fn parse_args(config: &mut Config, args: &[String]) -> Result<(), ConfigErr> {
    let mut i = 1; // skip program name (args[0])

    while i < args.len() {
        let arg = &args[i];

        // -- terminates flag parsing
        if arg == "--" {
            break;
        }

        // --flag=value form
        if arg.starts_with("--") && arg.contains('=') {
            let eq = arg.find('=').unwrap();
            let flag = &arg[2..eq];
            let val = &arg[eq + 1..];
            apply_long_flag(config, flag, Some(val))?;
            i += 1;
            continue;
        }

        // --flag (boolean or expects next arg)
        if arg.starts_with("--") {
            let flag = &arg[2..];

            if is_bool_flag(flag) {
                // Boolean: --flag without value → true
                apply_long_flag(config, flag, None)?;
            } else {
                // Value flag: consume next arg
                i += 1;
                if i >= args.len() {
                    return Err(ConfigErr::MissingValue(format!("--{}", flag)));
                }
                apply_long_flag(config, flag, Some(&args[i]))?;
            }
            i += 1;
            continue;
        }

        // -f value (single-letter short flags)
        if arg.starts_with('-') && arg.len() == 2 && &arg[1..2] != "-" {
            let short = &arg[1..2];
            i += 1;
            if i >= args.len() {
                return Err(ConfigErr::MissingValue(format!("-{}", short)));
            }
            apply_short_flag(config, short, &args[i])?;
            i += 1;
            continue;
        }

        // Unrecognized: skip silently (matches Go UnknownFlags = true)
        i += 1;
    }

    Ok(())
}

/// Dispatch a long flag name (e.g. "endpoint", "disable-web-ssh") with an optional value.
fn apply_long_flag(
    config: &mut Config,
    name: &str,
    val: Option<&str>,
) -> Result<(), ConfigErr> {
    match name {
        // -- Required strings
        "endpoint" => config.endpoint = require_val("--endpoint", val)?.to_string(),
        "token" => config.token = require_val("--token", val)?.to_string(),

        // -- f64
        "interval" => {
            config.interval = parse_f64("--interval", require_val("--interval", val)?)?
        }

        // -- u64
        "info-report-interval" => {
            config.info_report_interval =
                parse_u64("--info-report-interval", require_val("--info-report-interval", val)?)?
        }
        "reconnect-interval" => {
            config.reconnect_interval =
                parse_u64("--reconnect-interval", require_val("--reconnect-interval", val)?)?
        }
        "max-retries" => {
            config.max_retries =
                parse_u64("--max-retries", require_val("--max-retries", val)?)?
        }

        // -- u8
        "protocol-version" => {
            config.protocol_version =
                parse_u8("--protocol-version", require_val("--protocol-version", val)?)?
        }
        "month-rotate" => {
            config.month_rotate =
                parse_u8("--month-rotate", require_val("--month-rotate", val)?)?
        }

        // -- Bool flags (--flag → true, --flag=value → parsed)
        "disable-web-ssh" => {
            config.disable_web_ssh = parse_bool_opt("--disable-web-ssh", val)?
        }
        "disable-auto-update" => {
            config.disable_auto_update = parse_bool_opt("--disable-auto-update", val)?
        }
        "disable-compression" => {
            config.disable_compression = parse_bool_opt("--disable-compression", val)?
        }
        "gpu" => config.enable_gpu = parse_bool_opt("--gpu", val)?,
        "ignore-unsafe-cert" => {
            config.ignore_unsafe_cert = parse_bool_opt("--ignore-unsafe-cert", val)?
        }
        "debug-log" => config.debug_log = parse_bool_opt("--debug-log", val)?,
        "show-warning" => config.show_warning = parse_bool_opt("--show-warning", val)?,
        "get-ip-addr-from-nic" => {
            config.get_ip_addr_from_nic = parse_bool_opt("--get-ip-addr-from-nic", val)?
        }
        "memory-include-cache" => {
            config.memory_include_cache = parse_bool_opt("--memory-include-cache", val)?
        }
        "memory-report-raw-used" | "memory-exclude-bcf" => {
            config.memory_report_raw_used =
                parse_bool_opt("--memory-report-raw-used", val)?
        }

        // -- Strings with defaults
        "prefer-ip-version" => {
            config.prefer_ip_version =
                require_val("--prefer-ip-version", val)?.to_string()
        }
        "custom-ipv4" => {
            config.custom_ipv4 = require_val("--custom-ipv4", val)?.to_string()
        }
        "custom-ipv6" => {
            config.custom_ipv6 = require_val("--custom-ipv6", val)?.to_string()
        }
        "cf-access-client-id" => {
            config.cf_access_client_id =
                require_val("--cf-access-client-id", val)?.to_string()
        }
        "cf-access-client-secret" => {
            config.cf_access_client_secret =
                require_val("--cf-access-client-secret", val)?.to_string()
        }
        "auto-discovery" => {
            config.auto_discovery_key =
                require_val("--auto-discovery", val)?.to_string()
        }
        "host-proc" => {
            config.host_proc = require_val("--host-proc", val)?.to_string()
        }
        "config" => {
            config.config_file = require_val("--config", val)?.to_string()
        }

        // -- Vec<String> fields (comma-separated)
        "custom-dns" => {
            config.custom_dns = split_comma(require_val("--custom-dns", val)?)
        }
        "include-nics" => {
            config.include_nics = split_comma(require_val("--include-nics", val)?)
        }
        "exclude-nics" => {
            config.exclude_nics = split_comma(require_val("--exclude-nics", val)?)
        }

        // -- Vec<String> fields (semicolon-separated — matching Go)
        "include-mountpoints" | "include-mountpoint" => {
            config.include_mountpoints =
                split_semicolon(require_val(name, val)?)
        }
        "exclude-mountpoints" | "exclude-mountpoint" => {
            config.exclude_mountpoints =
                split_semicolon(require_val(name, val)?)
        }

        // Unknown flags: silently ignored (Go UnknownFlags = true)
        _ => {}
    }
    Ok(())
}

/// Dispatch a short flag character (e.g. "e", "t", "i").
fn apply_short_flag(config: &mut Config, short: &str, val: &str) -> Result<(), ConfigErr> {
    match short {
        "e" => config.endpoint = val.to_string(),
        "t" => config.token = val.to_string(),
        "i" => config.interval = parse_f64("-i", val)?,
        "u" => config.ignore_unsafe_cert = parse_bool("-u", val)?,
        "r" => config.max_retries = parse_u64("-r", val)?,
        "c" => config.reconnect_interval = parse_u64("-c", val)?,
        _ => {} // unknown short flag: silently ignored
    }
    Ok(())
}

/// Returns true if the flag name corresponds to a boolean field (no value required).
fn is_bool_flag(name: &str) -> bool {
    matches!(
        name,
        "disable-web-ssh"
            | "disable-auto-update"
            | "disable-compression"
            | "gpu"
            | "ignore-unsafe-cert"
            | "debug-log"
            | "show-warning"
            | "get-ip-addr-from-nic"
            | "memory-include-cache"
            | "memory-report-raw-used"
            | "memory-exclude-bcf"
    )
}

// ============================================================================
// load_env — read AGENT_* environment variables into Config
// ============================================================================
//
// Env var names match the Go `env` struct tags exactly.
// Invalid numeric/bool values are silently ignored (matching Go behavior).

pub fn load_env(config: &mut Config) {
    // Required strings
    if let Ok(v) = env::var("AGENT_ENDPOINT") {
        if !v.is_empty() {
            config.endpoint = v;
        }
    }
    if let Ok(v) = env::var("AGENT_TOKEN") {
        if !v.is_empty() {
            config.token = v;
        }
    }

    // f64
    if let Ok(v) = env::var("AGENT_INTERVAL") {
        if let Ok(n) = f64::from_str(&v) {
            config.interval = n;
        }
    }

    // u64
    if let Ok(v) = env::var("AGENT_INFO_REPORT_INTERVAL") {
        if let Ok(n) = u64::from_str(&v) {
            config.info_report_interval = n;
        }
    }
    if let Ok(v) = env::var("AGENT_RECONNECT_INTERVAL") {
        if let Ok(n) = u64::from_str(&v) {
            config.reconnect_interval = n;
        }
    }
    if let Ok(v) = env::var("AGENT_MAX_RETRIES") {
        if let Ok(n) = u64::from_str(&v) {
            config.max_retries = n;
        }
    }

    // u8
    if let Ok(v) = env::var("AGENT_PROTOCOL_VERSION") {
        if let Ok(n) = u8::from_str(&v) {
            config.protocol_version = n;
        }
    }
    if let Ok(v) = env::var("AGENT_MONTH_ROTATE") {
        if let Ok(n) = u8::from_str(&v) {
            config.month_rotate = n;
        }
    }

    // Booleans (accept true/false/1/0/yes/no/on/off)
    if let Ok(v) = env::var("AGENT_DISABLE_WEB_SSH") {
        if let Ok(b) = parse_bool("AGENT_DISABLE_WEB_SSH", &v) {
            config.disable_web_ssh = b;
        }
    }
    if let Ok(v) = env::var("AGENT_DISABLE_AUTO_UPDATE") {
        if let Ok(b) = parse_bool("AGENT_DISABLE_AUTO_UPDATE", &v) {
            config.disable_auto_update = b;
        }
    }
    if let Ok(v) = env::var("AGENT_DISABLE_COMPRESSION") {
        if let Ok(b) = parse_bool("AGENT_DISABLE_COMPRESSION", &v) {
            config.disable_compression = b;
        }
    }
    if let Ok(v) = env::var("AGENT_ENABLE_GPU") {
        if let Ok(b) = parse_bool("AGENT_ENABLE_GPU", &v) {
            config.enable_gpu = b;
        }
    }
    if let Ok(v) = env::var("AGENT_IGNORE_UNSAFE_CERT") {
        if let Ok(b) = parse_bool("AGENT_IGNORE_UNSAFE_CERT", &v) {
            config.ignore_unsafe_cert = b;
        }
    }
    if let Ok(v) = env::var("AGENT_DEBUG_LOG") {
        if let Ok(b) = parse_bool("AGENT_DEBUG_LOG", &v) {
            config.debug_log = b;
        }
    }
    if let Ok(v) = env::var("AGENT_SHOW_WARNING") {
        if let Ok(b) = parse_bool("AGENT_SHOW_WARNING", &v) {
            config.show_warning = b;
        }
    }
    if let Ok(v) = env::var("AGENT_GET_IP_ADDR_FROM_NIC") {
        if let Ok(b) = parse_bool("AGENT_GET_IP_ADDR_FROM_NIC", &v) {
            config.get_ip_addr_from_nic = b;
        }
    }
    if let Ok(v) = env::var("AGENT_MEMORY_INCLUDE_CACHE") {
        if let Ok(b) = parse_bool("AGENT_MEMORY_INCLUDE_CACHE", &v) {
            config.memory_include_cache = b;
        }
    }
    if let Ok(v) = env::var("AGENT_MEMORY_REPORT_RAW_USED") {
        if let Ok(b) = parse_bool("AGENT_MEMORY_REPORT_RAW_USED", &v) {
            config.memory_report_raw_used = b;
        }
    }
    // Deprecated: AGENT_MEMORY_MODE_AVAILABLE
    if let Ok(v) = env::var("AGENT_MEMORY_MODE_AVAILABLE") {
        if let Ok(b) = parse_bool("AGENT_MEMORY_MODE_AVAILABLE", &v) {
            config.memory_mode_available = b;
        }
    }

    // Strings
    if let Ok(v) = env::var("AGENT_PREFER_IP_VERSION") {
        if !v.is_empty() {
            config.prefer_ip_version = v;
        }
    }
    if let Ok(v) = env::var("AGENT_CUSTOM_IPV4") {
        if !v.is_empty() {
            config.custom_ipv4 = v;
        }
    }
    if let Ok(v) = env::var("AGENT_CUSTOM_IPV6") {
        if !v.is_empty() {
            config.custom_ipv6 = v;
        }
    }
    if let Ok(v) = env::var("AGENT_CF_ACCESS_CLIENT_ID") {
        if !v.is_empty() {
            config.cf_access_client_id = v;
        }
    }
    if let Ok(v) = env::var("AGENT_CF_ACCESS_CLIENT_SECRET") {
        if !v.is_empty() {
            config.cf_access_client_secret = v;
        }
    }
    if let Ok(v) = env::var("AGENT_AUTO_DISCOVERY_KEY") {
        if !v.is_empty() {
            config.auto_discovery_key = v;
        }
    }
    if let Ok(v) = env::var("HOST_PROC") {
        if !v.is_empty() {
            config.host_proc = v;
        }
    }
    if let Ok(v) = env::var("AGENT_CONFIG_FILE") {
        if !v.is_empty() {
            config.config_file = v;
        }
    }

    // Vec<String> — comma-separated
    if let Ok(v) = env::var("AGENT_CUSTOM_DNS") {
        if !v.is_empty() {
            config.custom_dns = split_comma(&v);
        }
    }
    if let Ok(v) = env::var("AGENT_INCLUDE_NICS") {
        if !v.is_empty() {
            config.include_nics = split_comma(&v);
        }
    }
    if let Ok(v) = env::var("AGENT_EXCLUDE_NICS") {
        if !v.is_empty() {
            config.exclude_nics = split_comma(&v);
        }
    }

    // Vec<String> — semicolon-separated
    if let Ok(v) = env::var("AGENT_INCLUDE_MOUNTPOINTS") {
        if !v.is_empty() {
            config.include_mountpoints = split_semicolon(&v);
        }
    }
    if let Ok(v) = env::var("AGENT_EXCLUDE_MOUNTPOINTS") {
        if !v.is_empty() {
            config.exclude_mountpoints = split_semicolon(&v);
        }
    }
}

// ============================================================================
// Minimal hand-rolled JSON parser (no serde — DD2 constraint)
// ============================================================================

#[derive(Debug, Clone)]
enum JsonValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Array(Vec<JsonValue>),
    Null,
}

#[derive(Debug, Clone)]
enum Token {
    BraceOpen,
    BraceClose,
    BracketOpen,
    BracketClose,
    Colon,
    Comma,
    Str(String),
    Num(f64),
    Boolean(bool),
    Null,
}

/// Tokenize a JSON byte slice into a Vec<Token>.
fn tokenize(input: &[u8]) -> Result<Vec<Token>, ConfigErr> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut pos: usize = 0;
    let len = input.len();

    while pos < len {
        let ch = input[pos] as char;

        // Whitespace
        if ch.is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        match ch {
            '{' => {
                tokens.push(Token::BraceOpen);
                pos += 1;
            }
            '}' => {
                tokens.push(Token::BraceClose);
                pos += 1;
            }
            '[' => {
                tokens.push(Token::BracketOpen);
                pos += 1;
            }
            ']' => {
                tokens.push(Token::BracketClose);
                pos += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                pos += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                pos += 1;
            }
            '"' => {
                let (s, next) = tokenize_string(input, pos)?;
                tokens.push(Token::Str(s));
                pos = next;
            }
            '-' | '0'..='9' => {
                let (n, next) = tokenize_number(input, pos)?;
                tokens.push(Token::Num(n));
                pos = next;
            }
            't' => {
                if pos + 4 <= len && &input[pos..pos + 4] == b"true" {
                    tokens.push(Token::Boolean(true));
                    pos += 4;
                } else {
                    return Err(ConfigErr::JsonError(format!(
                        "unexpected character at position {}: expected 'true'",
                        pos
                    )));
                }
            }
            'f' => {
                if pos + 5 <= len && &input[pos..pos + 5] == b"false" {
                    tokens.push(Token::Boolean(false));
                    pos += 5;
                } else {
                    return Err(ConfigErr::JsonError(format!(
                        "unexpected character at position {}: expected 'false'",
                        pos
                    )));
                }
            }
            'n' => {
                if pos + 4 <= len && &input[pos..pos + 4] == b"null" {
                    tokens.push(Token::Null);
                    pos += 4;
                } else {
                    return Err(ConfigErr::JsonError(format!(
                        "unexpected character at position {}: expected 'null'",
                        pos
                    )));
                }
            }
            _ => {
                return Err(ConfigErr::JsonError(format!(
                    "unexpected character '{}' at position {}",
                    ch, pos
                )));
            }
        }
    }

    Ok(tokens)
}

/// Tokenize a JSON string literal starting at `pos` (pointing to the opening '"').
/// Returns the decoded string and the position after the closing '"'.
fn tokenize_string(input: &[u8], pos: usize) -> Result<(String, usize), ConfigErr> {
    let mut result = String::new();
    let mut i = pos + 1; // skip opening '"'
    let len = input.len();

    while i < len {
        let ch = input[i] as char;
        match ch {
            '"' => {
                return Ok((result, i + 1)); // skip closing '"'
            }
            '\\' => {
                i += 1;
                if i >= len {
                    return Err(ConfigErr::JsonError(
                        "unexpected end of input in string escape".to_string(),
                    ));
                }
                match input[i] as char {
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    '/' => result.push('/'),
                    'b' => result.push('\x08'),
                    'f' => result.push('\x0C'),
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    'u' => {
                        // \uXXXX — 4 hex digits
                        if i + 4 >= len {
                            return Err(ConfigErr::JsonError(
                                "unexpected end of input in \\u escape".to_string(),
                            ));
                        }
                        let hex = std::str::from_utf8(&input[i + 1..i + 5]).map_err(|_| {
                            ConfigErr::JsonError("invalid UTF-8 in \\u escape".to_string())
                        })?;
                        let codepoint =
                            u32::from_str_radix(hex, 16).map_err(|_| {
                                ConfigErr::JsonError(format!(
                                    "invalid \\u escape: \\u{}",
                                    hex
                                ))
                            })?;
                        let c = char::from_u32(codepoint).ok_or_else(|| {
                            ConfigErr::JsonError(format!(
                                "invalid unicode codepoint: \\u{}",
                                hex
                            ))
                        })?;
                        result.push(c);
                        i += 4;
                    }
                    other => {
                        return Err(ConfigErr::JsonError(format!(
                            "invalid escape sequence \\{}",
                            other
                        )));
                    }
                }
            }
            _ => {
                result.push(ch);
            }
        }
        i += 1;
    }

    Err(ConfigErr::JsonError(
        "unterminated string literal".to_string(),
    ))
}

/// Tokenize a JSON number starting at `pos`.
/// Returns the parsed f64 and the position after the last digit.
fn tokenize_number(input: &[u8], pos: usize) -> Result<(f64, usize), ConfigErr> {
    let mut i = pos;
    let len = input.len();

    // Optional leading minus
    if i < len && input[i] == b'-' {
        i += 1;
    }

    // Integer part
    if i >= len || !(input[i] as char).is_ascii_digit() {
        return Err(ConfigErr::JsonError(format!(
            "expected digit at position {}",
            i
        )));
    }
    while i < len && (input[i] as char).is_ascii_digit() {
        i += 1;
    }

    // Optional fraction
    if i < len && input[i] == b'.' {
        i += 1;
        if i >= len || !(input[i] as char).is_ascii_digit() {
            return Err(ConfigErr::JsonError(format!(
                "expected digit after decimal point at position {}",
                i
            )));
        }
        while i < len && (input[i] as char).is_ascii_digit() {
            i += 1;
        }
    }

    // Optional exponent
    if i < len && (input[i] == b'e' || input[i] == b'E') {
        i += 1;
        if i < len && (input[i] == b'+' || input[i] == b'-') {
            i += 1;
        }
        if i >= len || !(input[i] as char).is_ascii_digit() {
            return Err(ConfigErr::JsonError(format!(
                "expected digit in exponent at position {}",
                i
            )));
        }
        while i < len && (input[i] as char).is_ascii_digit() {
            i += 1;
        }
    }

    let num_str =
        std::str::from_utf8(&input[pos..i]).map_err(|_| {
            ConfigErr::JsonError("invalid UTF-8 in number".to_string())
        })?;
    let n = f64::from_str(num_str).map_err(|_| {
        ConfigErr::JsonError(format!("invalid number: {}", num_str))
    })?;

    Ok((n, i))
}

/// Recursive-descent parser over a token slice.
struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Result<&Token, ConfigErr> {
        let tok = self.tokens.get(self.pos).ok_or_else(|| {
            ConfigErr::JsonError("unexpected end of JSON input".to_string())
        })?;
        self.pos += 1;
        Ok(tok)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), ConfigErr> {
        let tok = self.advance()?;
        match (tok, expected) {
            (Token::BraceOpen, Token::BraceOpen) => Ok(()),
            (Token::BraceClose, Token::BraceClose) => Ok(()),
            (Token::BracketOpen, Token::BracketOpen) => Ok(()),
            (Token::BracketClose, Token::BracketClose) => Ok(()),
            (Token::Colon, Token::Colon) => Ok(()),
            (Token::Comma, Token::Comma) => Ok(()),
            (Token::Null, Token::Null) => Ok(()),
            _ => Err(ConfigErr::JsonError(format!(
                "expected {:?}, got {:?}",
                expected, tok
            ))),
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, ConfigErr> {
        match self.peek().cloned() {
            Some(Token::Str(s)) => {
                self.pos += 1;
                Ok(JsonValue::Str(s))
            }
            Some(Token::Num(n)) => {
                self.pos += 1;
                Ok(JsonValue::Num(n))
            }
            Some(Token::Boolean(b)) => {
                self.pos += 1;
                Ok(JsonValue::Bool(b))
            }
            Some(Token::Null) => {
                self.pos += 1;
                Ok(JsonValue::Null)
            }
            Some(Token::BraceOpen) => {
                let map = self.parse_object()?;
                // Flatten a flat object into its string-keyed values (we don't
                // support nested objects as config values, but parse them into
                // the map properly so we can extract fields).
                // Actually, for config purposes, return a special marker.
                // We'll handle this in the caller — nested objects become Null
                // for config extraction.
                // For simplicity, collect keys into a Vec and return as JsonValue::Str
                // with JSON representation (not ideal but configs are flat).
                let keys: Vec<String> = map.keys().cloned().collect();
                Ok(JsonValue::Str(format!("<object with keys: {:?}>", keys)))
            }
            Some(Token::BracketOpen) => {
                let arr = self.parse_array()?;
                Ok(JsonValue::Array(arr))
            }
            None => Err(ConfigErr::JsonError(
                "unexpected end of input".to_string(),
            )),
            _ => Err(ConfigErr::JsonError(format!(
                "unexpected token: {:?}",
                self.peek()
            ))),
        }
    }

    fn parse_object(&mut self) -> Result<std::collections::HashMap<String, JsonValue>, ConfigErr> {
        let mut map = std::collections::HashMap::new();

        self.expect(&Token::BraceOpen)?;

        // Empty object
        if matches!(self.peek(), Some(Token::BraceClose)) {
            self.pos += 1;
            return Ok(map);
        }

        loop {
            // Key must be a string
            let key = match self.advance()? {
                Token::Str(s) => s.clone(),
                other => {
                    return Err(ConfigErr::JsonError(format!(
                        "expected string key, got {:?}",
                        other
                    )));
                }
            };

            self.expect(&Token::Colon)?;

            let value = self.parse_value()?;
            map.insert(key, value);

            match self.peek() {
                Some(Token::Comma) => {
                    self.pos += 1;
                }
                Some(Token::BraceClose) => {
                    self.pos += 1;
                    return Ok(map);
                }
                other => {
                    return Err(ConfigErr::JsonError(format!(
                        "expected ',' or '}}', got {:?}",
                        other
                    )));
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<Vec<JsonValue>, ConfigErr> {
        let mut arr: Vec<JsonValue> = Vec::new();

        self.expect(&Token::BracketOpen)?;

        // Empty array
        if matches!(self.peek(), Some(Token::BracketClose)) {
            self.pos += 1;
            return Ok(arr);
        }

        loop {
            let value = self.parse_value()?;
            arr.push(value);

            match self.peek() {
                Some(Token::Comma) => {
                    self.pos += 1;
                }
                Some(Token::BracketClose) => {
                    self.pos += 1;
                    return Ok(arr);
                }
                other => {
                    return Err(ConfigErr::JsonError(format!(
                        "expected ',' or ']', got {:?}",
                        other
                    )));
                }
            }
        }
    }
}

// ============================================================================
// load_json_config — read and apply a JSON config file
// ============================================================================

pub fn load_json_config(config: &mut Config, path: &str) -> Result<(), ConfigErr> {
    let bytes =
        fs::read(path).map_err(|e| ConfigErr::FileReadError {
            path: path.to_string(),
            error: e.to_string(),
        })?;

    let tokens = tokenize(&bytes)?;
    let mut parser = Parser::new(tokens);
    let map = parser.parse_object()?;

    apply_json_map(config, &map);
    Ok(())
}

/// Extract fields from a parsed JSON object map into Config.
/// Missing fields keep their current value. Invalid types are silently ignored.
fn apply_json_map(config: &mut Config, map: &std::collections::HashMap<String, JsonValue>) {
    // Helper closures
    let get_str = |key: &str| -> Option<&str> {
        match map.get(key) {
            Some(JsonValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    };
    let get_f64 = |key: &str| -> Option<f64> {
        match map.get(key) {
            Some(JsonValue::Num(n)) => Some(*n),
            _ => None,
        }
    };
    let get_bool = |key: &str| -> Option<bool> {
        match map.get(key) {
            Some(JsonValue::Bool(b)) => Some(*b),
            _ => None,
        }
    };
    let get_str_vec_comma = |key: &str| -> Option<Vec<String>> {
        match map.get(key) {
            Some(JsonValue::Str(s)) => Some(split_comma(s)),
            Some(JsonValue::Array(arr)) => {
                let v: Vec<String> = arr
                    .iter()
                    .filter_map(|jv| match jv {
                        JsonValue::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                if v.is_empty() && !arr.is_empty() {
                    None
                } else {
                    Some(v)
                }
            }
            _ => None,
        }
    };
    let get_str_vec_semicolon = |key: &str| -> Option<Vec<String>> {
        match map.get(key) {
            Some(JsonValue::Str(s)) => Some(split_semicolon(s)),
            Some(JsonValue::Array(arr)) => {
                let v: Vec<String> = arr
                    .iter()
                    .filter_map(|jv| match jv {
                        JsonValue::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                if v.is_empty() && !arr.is_empty() {
                    None
                } else {
                    Some(v)
                }
            }
            _ => None,
        }
    };

    // Apply each field (JSON keys match Go `json` struct tags)

    if let Some(v) = get_str("endpoint") {
        config.endpoint = v.to_string();
    }
    if let Some(v) = get_str("token") {
        config.token = v.to_string();
    }

    if let Some(v) = get_f64("interval") {
        config.interval = v;
    }
    if let Some(v) = get_f64("info_report_interval") {
        config.info_report_interval = v as u64;
    }
    if let Some(v) = get_f64("reconnect_interval") {
        config.reconnect_interval = v as u64;
    }
    if let Some(v) = get_f64("max_retries") {
        config.max_retries = v as u64;
    }
    if let Some(v) = get_f64("protocol_version") {
        config.protocol_version = v as u8;
    }
    if let Some(v) = get_f64("month_rotate") {
        config.month_rotate = v as u8;
    }

    if let Some(v) = get_bool("disable_web_ssh") {
        config.disable_web_ssh = v;
    }
    if let Some(v) = get_bool("disable_auto_update") {
        config.disable_auto_update = v;
    }
    if let Some(v) = get_bool("disable_compression") {
        config.disable_compression = v;
    }
    if let Some(v) = get_bool("enable_gpu") {
        config.enable_gpu = v;
    }
    if let Some(v) = get_bool("ignore_unsafe_cert") {
        config.ignore_unsafe_cert = v;
    }
    if let Some(v) = get_bool("debug_log") {
        config.debug_log = v;
    }
    if let Some(v) = get_bool("show_warning") {
        config.show_warning = v;
    }
    if let Some(v) = get_bool("get_ip_addr_from_nic") {
        config.get_ip_addr_from_nic = v;
    }
    if let Some(v) = get_bool("memory_include_cache") {
        config.memory_include_cache = v;
    }
    if let Some(v) = get_bool("memory_report_raw_used") {
        config.memory_report_raw_used = v;
    }
    if let Some(v) = get_bool("memory_mode_available") {
        config.memory_mode_available = v;
    }

    if let Some(v) = get_str("prefer_ip_version") {
        config.prefer_ip_version = v.to_string();
    }
    if let Some(v) = get_str("custom_ipv4") {
        config.custom_ipv4 = v.to_string();
    }
    if let Some(v) = get_str("custom_ipv6") {
        config.custom_ipv6 = v.to_string();
    }
    if let Some(v) = get_str("cf_access_client_id") {
        config.cf_access_client_id = v.to_string();
    }
    if let Some(v) = get_str("cf_access_client_secret") {
        config.cf_access_client_secret = v.to_string();
    }
    if let Some(v) = get_str("auto_discovery_key") {
        config.auto_discovery_key = v.to_string();
    }
    if let Some(v) = get_str("host_proc") {
        config.host_proc = v.to_string();
    }
    if let Some(v) = get_str("config_file") {
        config.config_file = v.to_string();
    }

    if let Some(v) = get_str_vec_comma("custom_dns") {
        config.custom_dns = v;
    }
    if let Some(v) = get_str_vec_comma("include_nics") {
        config.include_nics = v;
    }
    if let Some(v) = get_str_vec_comma("exclude_nics") {
        config.exclude_nics = v;
    }
    if let Some(v) = get_str_vec_semicolon("include_mountpoints") {
        config.include_mountpoints = v;
    }
    if let Some(v) = get_str_vec_semicolon("exclude_mountpoints") {
        config.exclude_mountpoints = v;
    }
}

// ============================================================================
// validate — ensure required fields are non-empty
// ============================================================================
//
// Also checks prefer_ip_version is empty, "4", or "6" (matching Go).

pub fn validate(config: &Config) -> Result<(), ConfigErr> {
    if config.endpoint.is_empty() {
        return Err(ConfigErr::Validation(
            "endpoint is required (set via --endpoint, AGENT_ENDPOINT, or JSON config)".to_string(),
        ));
    }
    if config.token.is_empty() {
        return Err(ConfigErr::Validation(
            "token is required (set via --token, AGENT_TOKEN, or JSON config)".to_string(),
        ));
    }
    if !config.prefer_ip_version.is_empty()
        && config.prefer_ip_version != "4"
        && config.prefer_ip_version != "6"
    {
        return Err(ConfigErr::Validation(format!(
            "invalid prefer_ip_version '{}': expected empty, \"4\", or \"6\"",
            config.prefer_ip_version
        )));
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn args_vec(args: &[&str]) -> Vec<String> {
        std::iter::once("komari-agent-rs")
            .chain(args.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn test_defaults() {
        let c = Config::default();
        assert_eq!(c.interval, 1.0);
        assert_eq!(c.info_report_interval, 5);
        assert_eq!(c.reconnect_interval, 5);
        assert_eq!(c.max_retries, 10);
        assert!(c.disable_web_ssh);
        assert!(c.disable_auto_update);
        assert!(!c.disable_compression);
        assert!(!c.enable_gpu);
        assert!(!c.ignore_unsafe_cert);
        assert!(!c.debug_log);
        assert_eq!(c.protocol_version, 2);
        assert_eq!(c.month_rotate, 0);
        assert!(c.custom_dns.is_empty());
        assert!(c.include_nics.is_empty());
        assert!(c.exclude_nics.is_empty());
        assert!(c.include_mountpoints.is_empty());
        assert!(c.exclude_mountpoints.is_empty());
    }

    #[test]
    fn test_parse_endpoint_and_token() {
        let mut c = Config::default();
        let args = args_vec(&[
            "--endpoint",
            "https://example.com",
            "--token",
            "secret123",
        ]);
        parse_args(&mut c, &args).unwrap();
        assert_eq!(c.endpoint, "https://example.com");
        assert_eq!(c.token, "secret123");
    }

    #[test]
    fn test_parse_equals_form() {
        let mut c = Config::default();
        let args = args_vec(&["--endpoint=https://foo.com", "--token=abc"]);
        parse_args(&mut c, &args).unwrap();
        assert_eq!(c.endpoint, "https://foo.com");
        assert_eq!(c.token, "abc");
    }

    #[test]
    fn test_parse_short_flags() {
        let mut c = Config::default();
        let args = args_vec(&[
            "-e", "https://e.com",
            "-t", "tok",
            "-i", "2.5",
            "-u", "true",
            "-r", "5",
            "-c", "10",
        ]);
        parse_args(&mut c, &args).unwrap();
        assert_eq!(c.endpoint, "https://e.com");
        assert_eq!(c.token, "tok");
        assert_eq!(c.interval, 2.5);
        assert!(c.ignore_unsafe_cert);
        assert_eq!(c.max_retries, 5);
        assert_eq!(c.reconnect_interval, 10);
    }

    #[test]
    fn test_parse_bool_flags() {
        let mut c = Config::default();
        // disable-web-ssh defaults to true; --disable-web-ssh without value → true
        // gpu defaults to false; --gpu without value → true
        let args = args_vec(&[
            "--endpoint", "x",
            "--token", "x",
            "--gpu",
            "--debug-log",
            "--disable-compression=true",
            "--disable-web-ssh=false",
        ]);
        parse_args(&mut c, &args).unwrap();
        assert!(c.enable_gpu);
        assert!(c.debug_log);
        assert!(c.disable_compression);
        assert!(!c.disable_web_ssh); // explicitly set to false
    }

    #[test]
    fn test_parse_vec_fields() {
        let mut c = Config::default();
        let args = args_vec(&[
            "--endpoint", "x",
            "--token", "x",
            "--custom-dns", "8.8.8.8,1.1.1.1",
            "--include-nics", "eth0, eth1",
            "--include-mountpoints", "/mnt/data;/mnt/backup",
        ]);
        parse_args(&mut c, &args).unwrap();
        assert_eq!(c.custom_dns, vec!["8.8.8.8", "1.1.1.1"]);
        assert_eq!(c.include_nics, vec!["eth0", "eth1"]);
        assert_eq!(c.include_mountpoints, vec!["/mnt/data", "/mnt/backup"]);
    }

    #[test]
    fn test_validate_missing_endpoint() {
        let c = Config::default();
        let err = validate(&c).unwrap_err();
        assert!(matches!(err, ConfigErr::Validation(_)));
        let msg = err.to_string();
        assert!(msg.contains("endpoint"));
    }

    #[test]
    fn test_validate_missing_token() {
        let mut c = Config::default();
        c.endpoint = "https://x.com".to_string();
        let err = validate(&c).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("token"));
    }

    #[test]
    fn test_validate_ok() {
        let mut c = Config::default();
        c.endpoint = "https://x.com".to_string();
        c.token = "secret".to_string();
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn test_validate_prefer_ip_version() {
        let mut c = Config::default();
        c.endpoint = "x".to_string();
        c.token = "x".to_string();
        c.prefer_ip_version = "5".to_string();
        assert!(validate(&c).is_err());

        c.prefer_ip_version = "4".to_string();
        assert!(validate(&c).is_ok());

        c.prefer_ip_version = "6".to_string();
        assert!(validate(&c).is_ok());

        c.prefer_ip_version = "".to_string();
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn test_parse_bool_helper() {
        assert_eq!(parse_bool("test", "true").unwrap(), true);
        assert_eq!(parse_bool("test", "false").unwrap(), false);
        assert_eq!(parse_bool("test", "1").unwrap(), true);
        assert_eq!(parse_bool("test", "0").unwrap(), false);
        assert_eq!(parse_bool("test", "yes").unwrap(), true);
        assert_eq!(parse_bool("test", "no").unwrap(), false);
        assert_eq!(parse_bool("test", "on").unwrap(), true);
        assert_eq!(parse_bool("test", "off").unwrap(), false);
        assert_eq!(parse_bool("test", "TRUE").unwrap(), true);
        assert!(parse_bool("test", "maybe").is_err());
    }

    #[test]
    fn test_load_env() {
        // Safety: unset these vars before test, restore after
        let saved_endpoint = env::var("AGENT_ENDPOINT").ok();
        let saved_token = env::var("AGENT_TOKEN").ok();
        let saved_gpu = env::var("AGENT_ENABLE_GPU").ok();
        let saved_interval = env::var("AGENT_INTERVAL").ok();
        let saved_dns = env::var("AGENT_CUSTOM_DNS").ok();
        let saved_mounts = env::var("AGENT_INCLUDE_MOUNTPOINTS").ok();

        unsafe {
            env::set_var("AGENT_ENDPOINT", "https://env.example.com");
            env::set_var("AGENT_TOKEN", "env-token");
            env::set_var("AGENT_ENABLE_GPU", "true");
            env::set_var("AGENT_INTERVAL", "3.5");
            env::set_var("AGENT_CUSTOM_DNS", "8.8.8.8,1.1.1.1");
            env::set_var("AGENT_INCLUDE_MOUNTPOINTS", "/a;/b");
        }

        let mut c = Config::default();
        load_env(&mut c);

        assert_eq!(c.endpoint, "https://env.example.com");
        assert_eq!(c.token, "env-token");
        assert!(c.enable_gpu);
        assert_eq!(c.interval, 3.5);
        assert_eq!(c.custom_dns, vec!["8.8.8.8", "1.1.1.1"]);
        assert_eq!(c.include_mountpoints, vec!["/a", "/b"]);

        // Restore
        fn restore(key: &str, saved: Option<String>) {
            unsafe {
                match saved {
                    Some(v) => env::set_var(key, v),
                    None => env::remove_var(key),
                }
            }
        }
        restore("AGENT_ENDPOINT", saved_endpoint);
        restore("AGENT_TOKEN", saved_token);
        restore("AGENT_ENABLE_GPU", saved_gpu);
        restore("AGENT_INTERVAL", saved_interval);
        restore("AGENT_CUSTOM_DNS", saved_dns);
        restore("AGENT_INCLUDE_MOUNTPOINTS", saved_mounts);
    }

    #[test]
    fn test_json_parse_simple() {
        let json = br#"{"endpoint": "https://j.example.com", "token": "json-token", "interval": 2.0, "enable_gpu": true, "max_retries": 5}"#;
        let tokens = tokenize(json).unwrap();
        let mut parser = Parser::new(tokens);
        let map = parser.parse_object().unwrap();

        let mut c = Config::default();
        apply_json_map(&mut c, &map);

        assert_eq!(c.endpoint, "https://j.example.com");
        assert_eq!(c.token, "json-token");
        assert_eq!(c.interval, 2.0);
        assert!(c.enable_gpu);
        assert_eq!(c.max_retries, 5);
    }

    #[test]
    fn test_json_parse_array_fields() {
        let json = br#"{"custom_dns": ["8.8.8.8", "1.1.1.1"], "include_nics": "eth0,eth1"}"#;
        let tokens = tokenize(json).unwrap();
        let mut parser = Parser::new(tokens);
        let map = parser.parse_object().unwrap();

        let mut c = Config::default();
        apply_json_map(&mut c, &map);

        assert_eq!(c.custom_dns, vec!["8.8.8.8", "1.1.1.1"]);
        assert_eq!(c.include_nics, vec!["eth0", "eth1"]);
    }

    #[test]
    fn test_parse_number_helpers() {
        assert_eq!(parse_f64("test", "1.5").unwrap(), 1.5);
        assert!(parse_f64("test", "abc").is_err());
        assert_eq!(parse_u64("test", "42").unwrap(), 42);
        assert!(parse_u64("test", "-1").is_err());
        assert!(parse_u64("test", "3.14").is_err());
        assert_eq!(parse_u8("test", "255").unwrap(), 255);
        assert!(parse_u8("test", "256").is_err());
    }
}
