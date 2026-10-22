//! Auto-discovery enrollment compatible with the official Go agent.
//!
//! A shared enrollment key is exchanged for a node token exactly once. The
//! resulting credentials are stored beside the executable with restrictive
//! permissions and are reused on later starts. Secret values are never logged.

use crate::config::Config;
use crate::json::{decode_json_string, object_field};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_CONFIG: usize = 1024 * 1024;

#[derive(Debug)]
pub enum DiscoveryErr {
    Io(io::Error),
    Protocol(&'static str),
    Http(String),
}

impl fmt::Display for DiscoveryErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Protocol(error) => f.write_str(error),
            Self::Http(error) => write!(f, "HTTP error: {error}"),
        }
    }
}

impl From<io::Error> for DiscoveryErr {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn resolve_token(config: &mut Config) -> Result<(), DiscoveryErr> {
    let path = credential_path()?;
    if let Some((uuid, token)) = load_credentials(&path)? {
        config.token = token;
        eprintln!("[komari] using persisted auto-discovery identity {uuid}");
        return Ok(());
    }

    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "komari-agent".to_string());
    let url = format!(
        "{}/api/clients/register?name={}",
        config.endpoint.trim_end_matches('/'),
        crate::ws::url_encode(&hostname)
    );
    let mut body = Vec::with_capacity(config.auto_discovery_key.len() + 16);
    body.extend_from_slice(b"{\"key\":");
    crate::json::write_json_string(&mut body, &config.auto_discovery_key);
    body.push(b'}');

    let tls = Arc::new(
        crate::tls::make_tls_config(config)
            .map_err(|_| DiscoveryErr::Protocol("TLS setup failed"))?,
    );
    let dial = crate::proxy::Dialer::from_config(config);
    let headers = vec![(
        "Authorization".to_string(),
        format!("Bearer {}", config.auto_discovery_key),
    )];
    let response =
        crate::http::http_post(&url, &body, "application/json", None, &headers, &tls, &dial)
            .map_err(|error| DiscoveryErr::Http(error.to_string()))?;
    if response.status_code != 200 {
        return Err(DiscoveryErr::Http(format!(
            "registration returned status {}",
            response.status_code
        )));
    }
    let root = response.body.as_slice();
    let status = object_field(root, "status")
        .and_then(decode_json_string)
        .ok_or(DiscoveryErr::Protocol(
            "registration response has no status",
        ))?;
    if status != "success" {
        return Err(DiscoveryErr::Protocol("registration was rejected"));
    }
    let data = object_field(root, "data")
        .ok_or(DiscoveryErr::Protocol("registration response has no data"))?;
    let uuid = object_field(data, "uuid")
        .and_then(decode_json_string)
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryErr::Protocol("registration response has no uuid"))?;
    let token = object_field(data, "token")
        .and_then(decode_json_string)
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryErr::Protocol("registration response has no token"))?;
    save_credentials(&path, &uuid, &token)?;
    config.token = token;
    eprintln!("[komari] auto-discovery registration completed for {uuid}");
    Ok(())
}

fn credential_path() -> Result<PathBuf, io::Error> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("auto-discovery.json"))
}

fn load_credentials(path: &Path) -> Result<Option<(String, String)>, DiscoveryErr> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > MAX_CONFIG {
        return Err(DiscoveryErr::Protocol("auto-discovery file is too large"));
    }
    let uuid = object_field(&bytes, "uuid")
        .and_then(decode_json_string)
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryErr::Protocol("auto-discovery file has no uuid"))?;
    let token = object_field(&bytes, "token")
        .and_then(decode_json_string)
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryErr::Protocol("auto-discovery file has no token"))?;
    Ok(Some((uuid, token)))
}

fn save_credentials(path: &Path, uuid: &str, token: &str) -> Result<(), DiscoveryErr> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stage = parent.join(format!(".auto-discovery-{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&stage)?;
    let result = (|| -> Result<(), DiscoveryErr> {
        let mut data = Vec::with_capacity(uuid.len() + token.len() + 32);
        data.extend_from_slice(b"{\"uuid\":");
        crate::json::write_json_string(&mut data, uuid);
        data.extend_from_slice(b",\"token\":");
        crate::json::write_json_string(&mut data, token);
        data.push(b'}');
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&stage, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_credentials_round_trip_and_reject_bad_files() {
        let root = std::env::temp_dir().join(format!(
            "komari-discovery-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("auto-discovery.json");
        save_credentials(&path, "node-世界", "secret-token").unwrap();
        assert_eq!(
            load_credentials(&path).unwrap(),
            Some(("node-世界".into(), "secret-token".into()))
        );
        fs::write(&path, br#"{"uuid":"node"}"#).unwrap();
        assert!(load_credentials(&path).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
