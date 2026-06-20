// cf_access.rs — Cloudflare Access header injection for komari-agent-rs.
//
// Cloudflare Access is Cloudflare's Zero Trust reverse proxy.  When a Komari
// server sits behind CF Access with "service token" auth, the agent must
// present `CF-Access-Client-Id` and `CF-Access-Client-Secret` on every
// WebSocket upgrade and every HTTP POST.
//
// This module provides a single `CfAccess` value object that extracts the
// credentials from `Config` and injects them as header tuples.
//
// Integration points:
//   ws.rs  — `WsConnection::connect` injects into the HTTP upgrade request.
//   http.rs — `http_post` accepts extra headers that the caller populates via
//             `CfAccess::inject_http_headers`.

use crate::config::Config;

/// Holds a Cloudflare Access service-token credential pair.
///
/// Constructed via `CfAccess::from_config(config)`, which returns `None` when
/// either the client ID or client secret is empty (i.e. CF Access is not in
/// use).
pub struct CfAccess {
    client_id: String,
    client_secret: String,
}

impl CfAccess {
    /// Extract CF Access credentials from agent configuration.
    ///
    /// Returns `None` when either field is empty — the caller should skip
    /// header injection entirely in that case.
    pub fn from_config(config: &Config) -> Option<Self> {
        if config.cf_access_client_id.is_empty() || config.cf_access_client_secret.is_empty() {
            None
        } else {
            Some(Self {
                client_id: config.cf_access_client_id.clone(),
                client_secret: config.cf_access_client_secret.clone(),
            })
        }
    }

    /// Append `CF-Access-Client-Id` and `CF-Access-Client-Secret` to a
    /// mutable header vector.  Used for WebSocket upgrade requests.
    pub fn inject_ws_headers(&self, headers: &mut Vec<(String, String)>) {
        headers.push(("CF-Access-Client-Id".to_string(), self.client_id.clone()));
        headers.push((
            "CF-Access-Client-Secret".to_string(),
            self.client_secret.clone(),
        ));
    }

    /// Append `CF-Access-Client-Id` and `CF-Access-Client-Secret` to a
    /// mutable header vector.  Used for HTTP POST requests.
    pub fn inject_http_headers(&self, headers: &mut Vec<(String, String)>) {
        headers.push(("CF-Access-Client-Id".to_string(), self.client_id.clone()));
        headers.push((
            "CF-Access-Client-Secret".to_string(),
            self.client_secret.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn from_config_empty_returns_none() {
        let cfg = Config::default();
        assert!(CfAccess::from_config(&cfg).is_none());
    }

    #[test]
    fn from_config_partial_returns_none() {
        let mut cfg = Config::default();
        cfg.cf_access_client_id = "id".to_string();
        assert!(CfAccess::from_config(&cfg).is_none());
    }

    #[test]
    fn from_config_full_returns_some() {
        let mut cfg = Config::default();
        cfg.cf_access_client_id = "id".to_string();
        cfg.cf_access_client_secret = "secret".to_string();
        let cf = CfAccess::from_config(&cfg).unwrap();
        // fields are private; verify via injection
        let mut h: Vec<(String, String)> = Vec::new();
        cf.inject_http_headers(&mut h);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].0, "CF-Access-Client-Id");
        assert_eq!(h[0].1, "id");
        assert_eq!(h[1].0, "CF-Access-Client-Secret");
        assert_eq!(h[1].1, "secret");
    }

    #[test]
    fn inject_ws_and_http_produce_same_headers() {
        let mut cfg = Config::default();
        cfg.cf_access_client_id = "ws-id".to_string();
        cfg.cf_access_client_secret = "ws-secret".to_string();
        let cf = CfAccess::from_config(&cfg).unwrap();

        let mut wh: Vec<(String, String)> = Vec::new();
        cf.inject_ws_headers(&mut wh);

        let mut hh: Vec<(String, String)> = Vec::new();
        cf.inject_http_headers(&mut hh);

        assert_eq!(wh, hh);
    }
}
