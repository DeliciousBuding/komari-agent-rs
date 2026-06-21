// http.rs — Minimal HTTP/1.1 POST client.
// DD4 in spec.md: manual HTTP client, ~70 lines. No reqwest/hyper.
// Pure std + rustls. Manual HTTP/1.1 formatting.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

// ── HttpResponse ─────────────────────────────────────────────────────────

pub struct HttpResponse {
    pub status_code: u16,
    #[allow(dead_code)]
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

// ── HttpErr ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HttpErr {
    Io(io::Error),
    Tls(String),
    Parse(String),
    Timeout,
}

impl fmt::Display for HttpErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Tls(s) => write!(f, "TLS error: {s}"),
            Self::Parse(s) => write!(f, "parse error: {s}"),
            Self::Timeout => write!(f, "connection timed out"),
        }
    }
}

impl std::error::Error for HttpErr {}

impl From<io::Error> for HttpErr {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HttpErr::Timeout,
            _ => HttpErr::Io(e),
        }
    }
}

// ── MaybeTls — unified TLS (rustls) and plain TCP stream ─────────────────
//
// Supports `http://` endpoints (local server self-monitoring, no TLS needed).
// `https://` → rustls TLS stream, `http://` → plain TcpStream. Both impl
// Read+Write so http_post/ws can treat them uniformly.

enum MaybeTls {
    Tls(rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>),
    Plain(std::net::TcpStream),
}

impl Read for MaybeTls {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            MaybeTls::Tls(s) => s.read(buf),
            MaybeTls::Plain(s) => s.read(buf),
        }
    }
}

impl Write for MaybeTls {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            MaybeTls::Tls(s) => s.write(buf),
            MaybeTls::Plain(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            MaybeTls::Tls(s) => s.flush(),
            MaybeTls::Plain(s) => s.flush(),
        }
    }
}

// ── http_post ────────────────────────────────────────────────────────────

pub fn http_post(
    url: &str,
    body: &[u8],
    content_type: &str,
    content_encoding: Option<&str>,
    extra_headers: &[(String, String)],
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
) -> Result<HttpResponse, HttpErr> {
    // 1. Parse URL → host, port, path, query
    let (host, port, path, query) = parse_url(url)?;

    // 2. TCP connect via the unified Dialer.
    //
    // The Dialer honors HTTPS_PROXY / SOCKS5 (with NO_PROXY bypass and auth)
    // when a proxy env var applies, otherwise resolves directly honoring
    // `--custom-dns` and `--prefer-ip-version`.
    let tcp = dial
        .connect(&host, port, Duration::from_secs(30))
        .map_err(|e| match e {
            crate::proxy::NetErr::Io(io_err) => HttpErr::Io(io_err),
            crate::proxy::NetErr::Dns(d) => {
                HttpErr::Parse(format!("DNS resolution failed for '{host}': {d}"))
            }
            crate::proxy::NetErr::Proxy(s) => HttpErr::Io(io::Error::new(io::ErrorKind::Other, s)),
        })?;

    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))?;

    // 3. Wrap stream: https:// → TLS (rustls), http:// → plain TCP (local server).
    let mut stream = if url.starts_with("https://") {
        let server_name = rustls::pki_types::ServerName::try_from(host.as_str())
            .map_err(|e| HttpErr::Tls(format!("invalid SNI: {e}")))?
            .to_owned();
        let conn = rustls::ClientConnection::new(Arc::clone(tls_cfg), server_name)
            .map_err(|e| HttpErr::Tls(format!("TLS: {e}")))?;
        MaybeTls::Tls(rustls::StreamOwned::new(conn, tcp))
    } else {
        MaybeTls::Plain(tcp)
    };

    // 4. Build HTTP request (manual HTTP/1.1 formatting — no alloc for path in
    //    the common no-query-string case)
    if query.is_empty() {
        write!(stream, "POST {path} HTTP/1.1\r\n")?;
    } else {
        write!(stream, "POST {path}?{query} HTTP/1.1\r\n")?;
    }
    write!(stream, "Host: {host}\r\n")?;
    write!(stream, "Content-Type: {content_type}\r\n")?;
    write!(stream, "Content-Length: {}\r\n", body.len())?;
    if let Some(enc) = content_encoding {
        write!(stream, "Content-Encoding: {enc}\r\n")?;
    }
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;

    // 5. Write body + flush (flush triggers TLS handshake)
    stream.write_all(body)?;
    stream.flush()?;

    // 6. Read response
    let mut resp_buf = Vec::new();
    stream.read_to_end(&mut resp_buf)?;

    // 7. Parse status line + headers + body
    parse_http_response(&resp_buf)
}

// ── URL parsing ──────────────────────────────────────────────────────────
//
// Parses "https://host[:port][/path][?query]" (port 443) or
//         "http://host[:port][/path][?query]"  (port 80) into its components.

fn parse_url(url: &str) -> Result<(String, u16, String, String), HttpErr> {
    let (rest, default_port) = if let Some(s) = url.strip_prefix("https://") {
        (s, 443u16)
    } else if let Some(s) = url.strip_prefix("http://") {
        (s, 80u16)
    } else {
        return Err(HttpErr::Parse(
            "URL must start with http:// or https://".into(),
        ));
    };

    // Split host from path/query
    let (host_part, path_part) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };

    // Extract optional port from host_part
    let (host, port) = match host_part.find(':') {
        Some(i) => {
            let h = host_part[..i].to_string();
            let p = host_part[i + 1..]
                .parse::<u16>()
                .map_err(|_| HttpErr::Parse(format!("invalid port in URL: {url}")))?;
            (h, p)
        }
        None => (host_part.to_string(), default_port),
    };

    // Split query from path
    let (path, query) = match path_part.find('?') {
        Some(i) => (path_part[..i].to_string(), path_part[i + 1..].to_string()),
        None => (path_part.to_string(), String::new()),
    };

    Ok((host, port, path, query))
}

// ── Response parsing ─────────────────────────────────────────────────────
//
// Parses raw HTTP response bytes into HttpResponse.
// Expects: "HTTP/1.1 <code> <reason>\r\n<headers>\r\n\r\n<body>"

fn parse_http_response(data: &[u8]) -> Result<HttpResponse, HttpErr> {
    let text = std::str::from_utf8(data)
        .map_err(|_| HttpErr::Parse("response is not valid UTF-8".into()))?;

    let sep = text
        .find("\r\n\r\n")
        .ok_or_else(|| HttpErr::Parse("no header/body separator found".into()))?;

    let header_text = &text[..sep];
    let body = data[sep + 4..].to_vec();

    let mut lines = header_text.lines();

    // Status line: "HTTP/1.1 200 OK"
    let status_line = lines
        .next()
        .ok_or_else(|| HttpErr::Parse("empty response".into()))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| HttpErr::Parse(format!("malformed status line: {status_line}")))?
        .parse::<u16>()
        .map_err(|_| HttpErr::Parse(format!("invalid status code in: {status_line}")))?;

    // Headers: "Name: value"
    let mut headers = Vec::new();
    for line in lines {
        if let Some(colon) = line.find(':') {
            headers.push((
                line[..colon].trim().to_string(),
                line[colon + 1..].trim().to_string(),
            ));
        }
    }

    Ok(HttpResponse {
        status_code,
        headers,
        body,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_standard() {
        let (host, port, path, query) =
            parse_url("https://example.com:8443/api/report?key=val").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8443);
        assert_eq!(path, "/api/report");
        assert_eq!(query, "key=val");
    }

    #[test]
    fn test_parse_url_default_port_no_path() {
        let (host, port, path, query) = parse_url("https://example.com").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/");
        assert_eq!(query, "");
    }

    #[test]
    fn test_parse_url_path_no_query() {
        let (host, port, path, query) = parse_url("https://example.com/api/v1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/api/v1");
        assert_eq!(query, "");
    }

    #[test]
    fn test_parse_url_root_path_with_query() {
        let (host, port, path, query) = parse_url("https://example.com/?token=abc").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/");
        assert_eq!(query, "token=abc");
    }

    #[test]
    fn test_parse_url_http_default_port_80() {
        // http:// is supported (local server self-monitoring, no TLS).
        let (host, port, path, query) = parse_url("http://127.0.0.1:25774").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 25774);
        assert_eq!(path, "/");
        assert_eq!(query, "");
        let (_, p80, _, _) = parse_url("http://example.com").unwrap();
        assert_eq!(p80, 80);
        let _ = (host, port, path, query);
    }

    #[test]
    fn test_parse_url_rejects_no_scheme() {
        assert!(parse_url("example.com").is_err());
        assert!(parse_url("ftp://example.com").is_err());
    }

    #[test]
    fn test_parse_response_200() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello";
        let resp = parse_http_response(raw).unwrap();
        assert_eq!(resp.status_code, 200);
        assert_eq!(resp.headers.len(), 2);
        assert_eq!(resp.headers[0].0, "Content-Type");
        assert_eq!(resp.headers[0].1, "text/plain");
        assert_eq!(resp.headers[1].0, "Content-Length");
        assert_eq!(resp.headers[1].1, "5");
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn test_parse_response_404_no_headers() {
        let raw = b"HTTP/1.1 404 Not Found\r\n\r\n";
        let resp = parse_http_response(raw).unwrap();
        assert_eq!(resp.status_code, 404);
        assert!(resp.headers.is_empty());
        assert!(resp.body.is_empty());
    }

    #[test]
    fn test_parse_response_no_separator() {
        assert!(parse_http_response(b"HTTP/1.1 200 OK").is_err());
    }

    #[test]
    fn test_parse_response_bad_status() {
        assert!(parse_http_response(b"HTTP/1.1 abc\r\n\r\n").is_err());
    }

    #[test]
    fn test_parse_response_body_with_crlf() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\nline1\r\nline2\r\n";
        let resp = parse_http_response(raw).unwrap();
        assert_eq!(resp.status_code, 200);
        assert_eq!(resp.body, b"line1\r\nline2\r\n");
    }

    #[test]
    fn test_http_err_display() {
        assert!(HttpErr::Timeout.to_string().contains("timed out"));
        assert!(
            HttpErr::Parse("bad URL".to_string())
                .to_string()
                .contains("bad URL")
        );
        assert!(
            HttpErr::Io(io::Error::new(io::ErrorKind::Other, "test"))
                .to_string()
                .contains("test")
        );
        assert!(
            HttpErr::Tls("handshake failed".to_string())
                .to_string()
                .contains("handshake failed")
        );
    }

    #[test]
    fn test_io_error_to_http_err_timeout() {
        let e = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        let he: HttpErr = e.into();
        assert!(matches!(he, HttpErr::Timeout));
    }

    #[test]
    fn test_io_error_to_http_err_would_block() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "block");
        let he: HttpErr = e.into();
        assert!(matches!(he, HttpErr::Timeout));
    }

    #[test]
    fn test_io_error_to_http_err_other() {
        let e = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let he: HttpErr = e.into();
        assert!(matches!(he, HttpErr::Io(_)));
    }
}
