// http.rs — Minimal HTTP/1.1 POST client.
// DD4 in spec.md: manual HTTP client, ~70 lines. No reqwest/hyper.
// Pure std + rustls. Manual HTTP/1.1 formatting.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Control RPC responses are small; a peer must not allocate without bound.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

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
    // Boxed: rustls::StreamOwned is large; boxing keeps the enum small on the stack.
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>>),
    Plain(std::net::TcpStream),
}

impl MaybeTls {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        match self {
            Self::Tls(stream) => stream.sock.set_read_timeout(Some(timeout)),
            Self::Plain(stream) => stream.set_read_timeout(Some(timeout)),
        }
    }
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

// ── http_post / http_get ─────────────────────────────────────────────────

pub fn http_post(
    url: &str,
    body: &[u8],
    content_type: &str,
    content_encoding: Option<&str>,
    extra_headers: &[(String, String)],
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
) -> Result<HttpResponse, HttpErr> {
    http_post_timeout(
        url,
        body,
        content_type,
        content_encoding,
        extra_headers,
        tls_cfg,
        dial,
        Duration::from_secs(30),
    )
}

/// `http_post` with a caller-chosen read/write timeout.
///
/// The v2 `agent.pull` long-poll blocks server-side for up to 25 s, so the
/// default 30 s budget leaves almost no margin for slow edges; the pull loop
/// uses 35 s (same as the Go agent's HTTP client).
#[allow(clippy::too_many_arguments)]
pub fn http_post_timeout(
    url: &str,
    body: &[u8],
    content_type: &str,
    content_encoding: Option<&str>,
    extra_headers: &[(String, String)],
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
    timeout: Duration,
) -> Result<HttpResponse, HttpErr> {
    http_request(
        "POST",
        url,
        Some(body),
        Some(content_type),
        content_encoding,
        extra_headers,
        tls_cfg,
        dial,
        timeout,
    )
}

#[allow(clippy::too_many_arguments)] // HTTP request surface genuinely needs method/url/body/headers/tls/dial
fn http_request(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    extra_headers: &[(String, String)],
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
    timeout: Duration,
) -> Result<HttpResponse, HttpErr> {
    let body = body.unwrap_or(&[]);
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
            crate::proxy::NetErr::Proxy(s) => HttpErr::Io(io::Error::other(s)),
        })?;

    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;

    // 3. Wrap stream: https:// → TLS (rustls), http:// → plain TCP (local server).
    let mut stream = if url.starts_with("https://") {
        let server_name = rustls::pki_types::ServerName::try_from(host.as_str())
            .map_err(|e| HttpErr::Tls(format!("invalid SNI: {e}")))?
            .to_owned();
        let conn = rustls::ClientConnection::new(Arc::clone(tls_cfg), server_name)
            .map_err(|e| HttpErr::Tls(format!("TLS: {e}")))?;
        MaybeTls::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        MaybeTls::Plain(tcp)
    };

    // 4. Build HTTP request (manual HTTP/1.1 formatting — no alloc for path in
    //    the common no-query-string case)
    if query.is_empty() {
        write!(stream, "{method} {path} HTTP/1.1\r\n")?;
    } else {
        write!(stream, "{method} {path}?{query} HTTP/1.1\r\n")?;
    }
    write!(
        stream,
        "Host: {}\r\n",
        host_header(&host, port, url.starts_with("https://"))
    )?;
    if let Some(content_type) = content_type {
        write!(stream, "Content-Type: {content_type}\r\n")?;
    }
    if method != "GET" {
        write!(stream, "Content-Length: {}\r\n", body.len())?;
    }
    if let Some(enc) = content_encoding {
        write!(stream, "Content-Encoding: {enc}\r\n")?;
    }
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;

    // 5. Write body + flush (flush triggers TLS handshake)
    if method != "GET" {
        stream.write_all(body)?;
    }
    stream.flush()?;

    // 6. Read response
    let deadline = Instant::now() + timeout;
    let resp_buf = read_response_with(&mut stream, |stream| {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(HttpErr::Timeout)?;
        stream.set_read_timeout(remaining)?;
        Ok(())
    })?;

    // 7. Parse status line + headers + body
    parse_http_response(&resp_buf)
}

pub(crate) fn host_header(host: &str, port: u16, tls: bool) -> String {
    let authority = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if port == if tls { 443 } else { 80 } {
        authority
    } else {
        format!("{authority}:{port}")
    }
}

#[cfg(test)]
fn read_response(reader: &mut impl Read) -> Result<Vec<u8>, HttpErr> {
    read_response_with(reader, |_| Ok(()))
}

fn read_response_with<R: Read>(
    reader: &mut R,
    mut before_read: impl FnMut(&mut R) -> Result<(), HttpErr>,
) -> Result<Vec<u8>, HttpErr> {
    let mut data = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        before_read(reader)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(data);
        }
        if data.len() + read > MAX_RESPONSE_BYTES {
            return Err(HttpErr::Parse("HTTP response exceeds 1 MiB limit".into()));
        }
        data.extend_from_slice(&buffer[..read]);
    }
}

// ── URL parsing ──────────────────────────────────────────────────────────
//
// Parses "https://host[:port][/path][?query]" (port 443) or
//         "http://host[:port][/path][?query]"  (port 80) into its components.

pub(crate) fn parse_url(url: &str) -> Result<(String, u16, String, String), HttpErr> {
    if url.bytes().any(|b| b <= 0x20 || b == 0x7f) || url.contains('#') {
        return Err(HttpErr::Parse("control character in URL".into()));
    }
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
    let (host_part, path_part) = match rest.find(['/', '?']) {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };

    // Extract optional port from host_part
    let (host, port) = if let Some(bracketed) = host_part.strip_prefix('[') {
        let (host, tail) = bracketed
            .split_once(']')
            .ok_or_else(|| HttpErr::Parse("unclosed IPv6 literal".into()))?;
        let port = if tail.is_empty() {
            default_port
        } else {
            tail.strip_prefix(':')
                .ok_or_else(|| HttpErr::Parse("invalid IPv6 authority".into()))?
                .parse::<u16>()
                .map_err(|_| HttpErr::Parse("invalid URL port".into()))?
        };
        (host.to_string(), port)
    } else {
        match host_part.find(':') {
            Some(i) => {
                let h = host_part[..i].to_string();
                let p = host_part[i + 1..]
                    .parse::<u16>()
                    .map_err(|_| HttpErr::Parse("invalid URL port".into()))?;
                (h, p)
            }
            None => (host_part.to_string(), default_port),
        }
    };
    if host.is_empty() || host.contains(['\r', '\n', '@']) {
        return Err(HttpErr::Parse("invalid URL authority".into()));
    }

    // Split query from path
    let (path, query) = match path_part.find('?') {
        Some(i) => (
            if i == 0 {
                "/".to_string()
            } else {
                path_part[..i].to_string()
            },
            path_part[i + 1..].to_string(),
        ),
        None => (path_part.to_string(), String::new()),
    };

    Ok((host, port, path, query))
}

// ── Response parsing ─────────────────────────────────────────────────────
//
// Parses raw HTTP response bytes into HttpResponse.
// Expects: "HTTP/1.1 <code> <reason>\r\n<headers>\r\n\r\n<body>"

fn parse_http_response(data: &[u8]) -> Result<HttpResponse, HttpErr> {
    let sep = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| HttpErr::Parse("no header/body separator found".into()))?;
    if sep > 65536 {
        return Err(HttpErr::Parse("HTTP headers exceed 64 KiB limit".into()));
    }
    let header_text = std::str::from_utf8(&data[..sep])
        .map_err(|_| HttpErr::Parse("headers are not valid UTF-8".into()))?;
    let raw_body = &data[sep + 4..];

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
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim();
            let value = line[colon + 1..].trim();
            if name.eq_ignore_ascii_case("content-length") {
                if content_length.is_some() {
                    return Err(HttpErr::Parse("duplicate content length".into()));
                }
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| HttpErr::Parse("invalid content length".into()))?,
                );
            }
            if name.eq_ignore_ascii_case("transfer-encoding") {
                if chunked || !value.eq_ignore_ascii_case("chunked") {
                    return Err(HttpErr::Parse(
                        "unsupported or duplicate transfer encoding".into(),
                    ));
                }
                chunked = true;
            }
            headers.push((
                line[..colon].trim().to_string(),
                line[colon + 1..].trim().to_string(),
            ));
        }
    }
    if chunked && content_length.is_some() {
        return Err(HttpErr::Parse("ambiguous HTTP framing".into()));
    }
    let body = if chunked {
        decode_chunked(raw_body)?
    } else {
        if content_length.is_some_and(|length| length != raw_body.len()) {
            return Err(HttpErr::Parse("HTTP content length mismatch".into()));
        }
        raw_body.to_vec()
    };

    Ok(HttpResponse {
        status_code,
        headers,
        body,
    })
}

fn decode_chunked(mut raw: &[u8]) -> Result<Vec<u8>, HttpErr> {
    let invalid = || HttpErr::Parse("invalid or truncated chunked body".into());
    let mut body = Vec::new();
    loop {
        let end = raw
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(invalid)?;
        if end > 1024 {
            return Err(invalid());
        }
        let line = std::str::from_utf8(&raw[..end]).map_err(|_| invalid())?;
        let value = line.split(';').next().ok_or_else(invalid)?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let size = usize::from_str_radix(value, 16).map_err(|_| invalid())?;
        raw = &raw[end + 2..];
        if size == 0 {
            // Trailers are ignored but must be a terminated header block.
            if raw == b"\r\n" || raw.ends_with(b"\r\n\r\n") {
                return Ok(body);
            }
            return Err(invalid());
        }
        if size > MAX_RESPONSE_BYTES - body.len()
            || size > raw.len().saturating_sub(2)
            || raw.get(size..size + 2) != Some(b"\r\n")
        {
            return Err(invalid());
        }
        body.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_preserves_ipv6_and_nondefault_ports() {
        assert_eq!(host_header("::1", 8443, true), "[::1]:8443");
        assert_eq!(host_header("example.test", 8443, true), "example.test:8443");
        assert_eq!(host_header("example.test", 443, true), "example.test");
    }

    #[test]
    fn chunked_and_content_length_are_not_confused() {
        let response = parse_http_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(response.body, b"{}");
        for data in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n{}"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{"[..],
        ] {
            assert!(parse_http_response(data).is_err());
        }
    }

    #[test]
    fn total_read_deadline_is_checked_between_progress_reads() {
        struct Dribble;
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf[0] = b'x';
                Ok(1)
            }
        }
        let mut reads = 0;
        let result = read_response_with(&mut Dribble, |_| {
            reads += 1;
            if reads > 3 {
                Err(HttpErr::Timeout)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(HttpErr::Timeout)));
        assert_eq!(reads, 4);
    }

    #[test]
    fn response_read_is_bounded() {
        assert!(read_response(&mut &vec![0; MAX_RESPONSE_BYTES][..]).is_ok());
        assert!(read_response(&mut &vec![0; MAX_RESPONSE_BYTES + 1][..]).is_err());
    }

    #[test]
    fn url_ipv6_and_query_without_path() {
        assert_eq!(
            parse_url("https://[::1]:8443/rpc").unwrap(),
            ("::1".into(), 8443, "/rpc".into(), "".into())
        );
        assert_eq!(
            parse_url("https://example.com?token=synthetic").unwrap(),
            (
                "example.com".into(),
                443,
                "/".into(),
                "token=synthetic".into()
            )
        );
        assert!(parse_url("https://user:secret@example.com/").is_err()); // # leak-guard-allow:LG-F4C6A155 synthetic invalid-authority fixture
        assert!(parse_url("https://[::1").is_err());
    }

    #[test]
    fn response_body_is_binary_not_header_text() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\n\r\n\xff\xfe")
                .unwrap()
                .body,
            vec![255, 254]
        );
    }

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
