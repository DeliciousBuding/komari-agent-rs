// ws.rs — WebSocket connect + handshake + manual frame codec (RFC 6455).
//
// Design constraints (per spec.md DD3):
//   - Manual frame codec — no tungstenite.
//   - SHA-1 + Base64 via crate::crypto (ws_accept_key, ws_generate_key, ws_verify_accept).
//   - TLS via rustls::StreamOwned (ClientConfig provided by caller from tls.rs).
//   - Sync single-threaded, no async runtime.
//   - Hot-path send uses a 4096-byte stack buffer for chunked masking (zero alloc).
//
// References:
//   - RFC 6455 (WebSocket Protocol)
//   - komari-agent-rs/docs/plan/spec.md (DD3)
//
// The WS codec exposes a complete frame/error/close surface (RFC 6455 §7); a few
// error variants and the `close` handshake helper are not yet exercised by the
// live agent. Allow dead_code for the parity surface.
#![allow(dead_code)]

use crate::crypto;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;

// ── Constants ────────────────────────────────────────────────────────────

/// Maximum WebSocket frame payload accepted on read (1 MiB).
const MAX_FRAME_SIZE: u64 = 1024 * 1024;

/// Chunk size for stack-based send masking.
const MASK_CHUNK_SIZE: usize = 4096;

/// Maximum HTTP response header size during handshake.
const MAX_HTTP_RESPONSE_SIZE: usize = 65536;

// ── Error type ───────────────────────────────────────────────────────────

/// Unified WebSocket error.
///
/// Covers all failure modes of the connect→handshake→send→receive lifecycle.
#[derive(Debug)]
pub enum WsErr {
    /// I/O error from the underlying TCP/TLS stream.
    Io(String),
    /// TLS handshake or protocol error from rustls.
    Tls(String),
    /// DNS resolution failure.
    Dns(String),
    /// HTTP upgrade handshake failure (bad status, missing header, accept mismatch).
    Handshake(String),
    /// WebSocket protocol violation (bad frame, unknown opcode, oversized payload).
    Protocol(String),
    /// Internal buffer exhausted (should not occur with 4096+ byte stack buffers).
    BufferFull,
}

impl std::fmt::Display for WsErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Tls(e) => write!(f, "TLS error: {}", e),
            Self::Dns(e) => write!(f, "DNS error: {}", e),
            Self::Handshake(e) => write!(f, "handshake error: {}", e),
            Self::Protocol(e) => write!(f, "protocol error: {}", e),
            Self::BufferFull => write!(f, "buffer full"),
        }
    }
}

impl std::error::Error for WsErr {}

impl From<std::io::Error> for WsErr {
    fn from(e: std::io::Error) -> Self {
        WsErr::Io(e.to_string())
    }
}

// ── Opcode and Message types ────────────────────────────────────────────

/// WebSocket frame opcode (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WsOpcode {
    /// Continuation frame (not used in our send path; rejected on receive).
    Continuation = 0,
    /// UTF-8 text message.
    Text = 1,
    /// Binary message.
    Binary = 2,
    /// Connection close.
    Close = 8,
    /// Ping.
    Ping = 9,
    /// Pong.
    Pong = 10,
}

impl TryFrom<u8> for WsOpcode {
    type Error = WsErr;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::Continuation),
            1 => Ok(Self::Text),
            2 => Ok(Self::Binary),
            8 => Ok(Self::Close),
            9 => Ok(Self::Ping),
            10 => Ok(Self::Pong),
            _ => Err(WsErr::Protocol(format!("unknown opcode: {}", v))),
        }
    }
}

/// Parsed WebSocket message received from the server.
///
/// Server→client frames are NOT masked per RFC 6455 §5.3, but we unmask
/// if the MASK bit is set regardless (defence in depth).
/// The payload in Text/Binary/Ping/Pong is the unmasked data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// Text message (opcode=1), unmasked payload.
    Text(Vec<u8>),
    /// Binary message (opcode=2), unmasked payload.
    Binary(Vec<u8>),
    /// Ping frame (opcode=9), unmasked payload (may be empty).
    Ping(Vec<u8>),
    /// Pong frame (opcode=10), unmasked payload (may be empty).
    Pong(Vec<u8>),
    /// Close frame (opcode=8).
    Close,
}

// ── Masking ─────────────────────────────────────────────────────────────

/// Apply (or remove) a 4-byte masking key from `data` in-place.
///
/// XOR-based masking per RFC 6455 §5.3.  Since XOR is its own inverse,
/// the same function masks and unmasks.
///
/// Client→server frames MUST be masked; server→client frames MUST NOT be
/// masked (but we handle both for robustness).
#[inline]
pub fn apply_mask(data: &mut [u8], mask: [u8; 4]) {
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
}

// ── Random bytes for masking key ─────────────────────────────────────────

/// Return a 4-byte CSPRNG masking key.
///
/// Cold-path: once per outbound frame.  Opens `/dev/urandom` (Unix) or calls
/// `BCryptGenRandom` (Windows).  Acceptable overhead — WS send happens once
/// per monitoring tick (~1 Hz), not per byte.
///
/// Returns an error if the OS entropy source is unavailable.
fn random_mask_key() -> Result<[u8; 4], WsErr> {
    let mut buf = [0u8; 4];

    #[cfg(unix)]
    {
        use std::fs::File;
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut buf))
            .map_err(|e| WsErr::Io(format!("random mask key: {}", e)))?;
    }

    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                h_algorithm: *mut core::ffi::c_void,
                pb_buffer: *mut u8,
                cb_buffer: u32,
                dw_flags: u32,
            ) -> i32;
        }

        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
        let status = unsafe {
            BCryptGenRandom(
                core::ptr::null_mut(),
                buf.as_mut_ptr(),
                4,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(WsErr::Io(format!(
                "BCryptGenRandom failed with status 0x{status:08X}"
            )));
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        compile_error!("ws: unsupported target for masking key generation");
    }

    Ok(buf)
}

// ── Frame header encoding ────────────────────────────────────────────────

/// Build a masked WebSocket frame header into a fixed-size stack buffer.
///
/// Returns `(header_buf, used_len, mask_key)`.
///
/// The header buffer is 14 bytes: 2 fixed + up to 8 extended payload length + 4
/// (mask_key is returned separately and written by the caller).
///
/// ```
/// Byte 0: FIN(1) | RSV1-3(0) | opcode(4)
/// Byte 1: MASK(1) | payload_len(7)  or 126/127 marker
/// Byte 2-3 or 2-9: extended payload length (if payload_len >= 126)
/// ```
fn build_frame_header(
    opcode: u8,
    payload_len: u64,
    masked: bool,
    mask_key: [u8; 4],
    rsv1: bool,
) -> ([u8; 14], usize, [u8; 4]) {
    let mut buf = [0u8; 14];
    let mut pos = 0;

    // Byte 0: FIN=1 | RSV1(permessage-deflate) | opcode
    let fin_rsv: u8 = if rsv1 { 0xC0 } else { 0x80 };
    buf[pos] = fin_rsv | (opcode & 0x0F);
    pos += 1;

    // Byte 1: MASK + payload length (or extend marker)
    let mask_bit: u8 = if masked { 0x80 } else { 0x00 };

    if payload_len < 126 {
        buf[pos] = mask_bit | (payload_len as u8);
        pos += 1;
    } else if payload_len <= u16::MAX as u64 {
        buf[pos] = mask_bit | 126;
        pos += 1;
        buf[pos..pos + 2].copy_from_slice(&(payload_len as u16).to_be_bytes());
        pos += 2;
    } else {
        buf[pos] = mask_bit | 127;
        pos += 1;
        buf[pos..pos + 8].copy_from_slice(&payload_len.to_be_bytes());
        pos += 8;
    }

    (buf, pos, mask_key)
}

/// Write a complete masked frame header + masking key + chunked masked payload
/// to the TLS stream.
///
/// Uses a `[u8; 4096]` stack buffer for chunked masking — zero heap allocation
/// proportional to payload size.  The header and mask key are written first,
/// then the payload is masked and written in 4096-byte chunks.
fn write_masked_frame(
    stream: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    opcode: u8,
    payload: &[u8],
    compress: bool,
) -> Result<(), WsErr> {
    // permessage-deflate (RFC 7692): when active, compress the payload and set
    // RSV1 on the frame. The compressed bytes replace the payload; opcode is
    // unchanged. Control frames (ping/pong/close) MUST NOT be compressed, so
    // callers pass compress=false for those.
    let owned_compressed: Vec<u8>;
    let (frame_payload, rsv1) = if compress {
        owned_compressed = crate::gzip::permessage_deflate_encode(payload);
        (owned_compressed.as_slice(), true)
    } else {
        (payload, false)
    };

    let mask_key = random_mask_key()?;
    let (header, header_len, mk) =
        build_frame_header(opcode, frame_payload.len() as u64, true, mask_key, rsv1);

    stream.write_all(&header[..header_len])?;
    stream.write_all(&mk)?;

    // Mask and write in chunks — avoids allocating a full copy of the payload.
    let mut chunk_buf = [0u8; MASK_CHUNK_SIZE];
    for chunk in frame_payload.chunks(MASK_CHUNK_SIZE) {
        let len = chunk.len();
        chunk_buf[..len].copy_from_slice(chunk);
        apply_mask(&mut chunk_buf[..len], mask_key);
        stream.write_all(&chunk_buf[..len])?;
    }
    stream.flush()?;

    Ok(())
}

// ── WsConnection ─────────────────────────────────────────────────────────

/// A WebSocket connection over TLS.
///
/// Wraps a `rustls::StreamOwned<ClientConnection, TcpStream>`.
/// All reads and writes go through rustls for automatic TLS encryption.
pub struct WsConnection {
    stream: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    /// Whether permessage-deflate (RFC 7692) was negotiated with the server.
    compression_enabled: bool,
    frame_reader: FrameReader,
}

impl WsConnection {
    /// Connect to a Komari WebSocket endpoint.
    ///
    /// # Steps
    ///
    /// 1. Parse endpoint URL → host + port + path.
    /// 2. DNS resolve via `ToSocketAddrs`.
    /// 3. TCP connect with timeout (try all resolved addresses).
    /// 4. Wrap TCP stream in rustls TLS (SNI from host).
    /// 5. Perform TLS handshake via rustls automatic I/O handshake.
    /// 6. Send HTTP upgrade request (GET + headers + WebSocket key).
    /// 7. Read and verify HTTP 101 response + `Sec-WebSocket-Accept`.
    /// 8. Return ready `WsConnection`.
    #[allow(clippy::too_many_arguments)] // WS connect genuinely needs all these
    pub fn connect(
        endpoint: &str,
        ws_path: &str,
        token: &str,
        tls_cfg: Arc<rustls::ClientConfig>,
        timeout: Duration,
        extra_headers: &[(String, String)],
        dial: &crate::proxy::Dialer,
        enable_compression: bool,
    ) -> Result<Self, WsErr> {
        // ── 1. Parse URL → host + port (path supplied by caller) ──
        let (host, port, _default_path) = parse_endpoint(endpoint)?;
        if !ws_path.starts_with('/')
            || ws_path.bytes().any(|b| b <= 0x20 || b == 0x7f)
            || ws_path.contains('#')
        {
            return Err(WsErr::Protocol("invalid WebSocket request path".into()));
        }

        // ── 2. Reach (host, port) via the unified Dialer ──
        //
        // The Dialer encapsulates the full network policy: HTTPS_PROXY /
        // SOCKS5 tunneling (with NO_PROXY bypass and auth) when a proxy env
        // var applies, otherwise direct connect honoring `--custom-dns` and
        // `--prefer-ip-version`. This keeps the WS layer agnostic of the
        // surrounding network environment.
        let sock = dial.connect(&host, port, timeout).map_err(|e| {
            // Preserve the DNS-specific error variant so callers (FSM) can
            // classify connection vs. DNS failures.
            match e {
                crate::proxy::NetErr::Dns(d) => WsErr::Dns(format!("{}: {}", host, d)),
                other => WsErr::Io(format!("connect to {host}:{port}: {other}")),
            }
        })?;

        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| WsErr::Io(format!("set_read_timeout: {}", e)))?;
        sock.set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| WsErr::Io(format!("set_write_timeout: {}", e)))?;

        // ── 4. TLS client wrapping ──
        let server_name = host_to_server_name(&host)?;

        let conn = rustls::ClientConnection::new(tls_cfg, server_name)
            .map_err(|e| WsErr::Tls(format!("create TLS client: {}", e)))?;

        let mut stream = rustls::StreamOwned::new(conn, sock);

        // ── 5. TLS handshake ──
        // rustls `StreamOwned` performs the handshake implicitly on first I/O
        // via its `Read`/`Write` implementations.  The `flush()` below (after
        // writing the upgrade request) will drive the handshake to completion.
        // We do a one-byte read after flush to force any post-handshake data
        // through — this is sufficient for the upgrade-response round-trip.

        // ── 6. Build and send HTTP upgrade request ──
        let ws_key_bytes =
            crypto::ws_generate_key().map_err(|e| WsErr::Io(format!("generate WS key: {}", e)))?;
        let ws_key_str = core::str::from_utf8(&ws_key_bytes).map_err(|_| {
            WsErr::Handshake("generated WebSocket key is not valid UTF-8".to_string())
        })?;

        let upgrade_request = build_upgrade_request(
            &crate::http::host_header(&host, port, true),
            ws_path,
            token,
            ws_key_str,
            extra_headers,
            enable_compression,
        );
        stream
            .write_all(upgrade_request.as_bytes())
            .map_err(|e| WsErr::Io(format!("write upgrade request: {}", e)))?;
        stream
            .flush()
            .map_err(|e| WsErr::Io(format!("flush upgrade request: {}", e)))?;

        // ── 7. Read and verify HTTP response ──
        let response_bytes = read_http_response_headers(&mut stream)?;
        let response = std::str::from_utf8(&response_bytes)
            .map_err(|_| WsErr::Handshake("HTTP response is not valid UTF-8".to_string()))?;

        let (status_code, accept_header, permessage) = parse_http_response(response)?;

        if status_code != 101 {
            return Err(WsErr::Handshake(format!(
                "expected HTTP 101 Switching Protocols, got {}",
                status_code
            )));
        }

        let accept_value = accept_header
            .ok_or_else(|| WsErr::Handshake("missing Sec-WebSocket-Accept header".to_string()))?;

        let expected_accept = crypto::ws_accept_key(ws_key_str);
        if !crypto::ws_verify_accept(accept_value.as_bytes(), &expected_accept) {
            return Err(WsErr::Handshake(format!(
                "Sec-WebSocket-Accept mismatch: expected {}",
                core::str::from_utf8(&expected_accept).unwrap_or("<non-UTF8>")
            )));
        }

        // ── 8. Return ready connection ──
        // permessage-deflate is active only if we offered it AND the server
        // echoed it back in its handshake response.
        Ok(Self {
            stream,
            compression_enabled: enable_compression && permessage,
            frame_reader: FrameReader::default(),
        })
    }

    /// Send a masked text frame (opcode=1, FIN=1).
    ///
    /// The caller must ensure `data` is valid UTF-8.
    pub fn send_text(&mut self, data: &[u8]) -> Result<(), WsErr> {
        write_masked_frame(
            &mut self.stream,
            WsOpcode::Text as u8,
            data,
            self.compression_enabled,
        )
    }

    /// Send a masked binary frame (opcode=2, FIN=1).
    #[allow(dead_code)]
    pub fn send_binary(&mut self, data: &[u8]) -> Result<(), WsErr> {
        write_masked_frame(
            &mut self.stream,
            WsOpcode::Binary as u8,
            data,
            self.compression_enabled,
        )
    }

    /// Send a masked ping frame (opcode=9, FIN=1, empty payload).
    ///
    /// Per RFC 6455, a ping may carry up to 125 bytes of application data.
    /// This implementation sends an empty ping (payload_len=0).
    pub fn send_ping(&mut self) -> Result<(), WsErr> {
        // Control frames are never compressed (RFC 7692 §6.1).
        write_masked_frame(&mut self.stream, WsOpcode::Ping as u8, &[], false)
    }

    /// Send a masked pong frame (opcode=10, FIN=1).
    ///
    /// Called in response to a received ping.  RFC 6455 requires the pong
    /// payload to be identical to the ping payload.
    pub fn send_pong(&mut self, data: &[u8]) -> Result<(), WsErr> {
        write_masked_frame(&mut self.stream, WsOpcode::Pong as u8, data, false)
    }

    /// Send a masked close frame (opcode=8, FIN=1, empty payload).
    ///
    /// After sending, the caller should read until a close frame is received
    /// back or the connection drops (per RFC 6455 §7.1.1 close handshake).
    pub fn close(&mut self) -> Result<(), WsErr> {
        write_masked_frame(&mut self.stream, WsOpcode::Close as u8, &[], false)
    }

    /// Read one complete frame, preserving consumed bytes across socket
    /// timeouts. Only an actual zero-byte read at a frame boundary is clean
    /// EOF; resets, TLS errors and truncated frames remain errors.
    pub fn read_message(&mut self) -> Result<Option<WsMessage>, WsErr> {
        self.frame_reader
            .read_message(&mut self.stream, self.compression_enabled)
    }

    /// Return shared references to the inner TCP socket and TLS state.
    ///
    /// Useful for inspecting connection state or adjusting socket options.
    #[allow(dead_code)]
    pub fn get_ref(&self) -> &TcpStream {
        self.stream.get_ref()
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Parse an endpoint URL into `(host, port, path)`.
///
/// Accepts `https://host[:port][/path]` or `wss://host[:port][/path]`.
/// Defaults: port = 443 if omitted, path = `/api/clients/v2/rpc` if root-only.
fn parse_endpoint(endpoint: &str) -> Result<(String, u16, String), WsErr> {
    let normalized;
    let url = if let Some(rest) = endpoint.strip_prefix("wss://") {
        normalized = format!("https://{rest}");
        normalized.as_str()
    } else if endpoint.starts_with("https://") {
        endpoint
    } else {
        return Err(WsErr::Protocol(
            "TLS WebSocket requires https:// or wss://".into(),
        ));
    };
    let (host, port, path, query) = crate::http::parse_url(url)
        .map_err(|_| WsErr::Protocol("invalid WebSocket endpoint".into()))?;
    let mut path = if path == "/" {
        "/api/clients/v2/rpc".to_string()
    } else {
        path
    };
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query);
    }
    Ok((host, port, path))
}

/// Try connecting to each resolved address, return the first that succeeds.
fn connect_with_timeout(addrs: &[SocketAddr], timeout: Duration) -> Result<TcpStream, WsErr> {
    let mut last_err: Option<std::io::Error> = None;

    for addr in addrs {
        match TcpStream::connect_timeout(addr, timeout) {
            Ok(sock) => {
                let _ = sock.set_nodelay(true); // best-effort: disable Nagle's algorithm
                return Ok(sock);
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(WsErr::Io(format!(
        "failed to connect to any address: {:?}",
        last_err
    )))
}

/// Convert a hostname string to a rustls `ServerName` for SNI.
///
/// Handles both DNS names and raw IP addresses.
fn host_to_server_name(host: &str) -> Result<ServerName<'static>, WsErr> {
    // If it looks like an IP address, use IpAddress variant.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }

    ServerName::try_from(host.to_string())
        .map_err(|e| WsErr::Dns(format!("invalid hostname '{}': {}", host, e)))
}

/// Build the HTTP WebSocket upgrade request.
///
/// Format:
/// ```text
/// GET /api/clients/v2/rpc?token={token} HTTP/1.1
/// Host: {host}
/// Upgrade: websocket
/// Connection: Upgrade
/// Sec-WebSocket-Key: {base64_random_16}
/// Sec-WebSocket-Version: 13
///
/// ```
fn build_upgrade_request(
    host: &str,
    path: &str,
    token: &str,
    ws_key: &str,
    extra_headers: &[(String, String)],
    enable_compression: bool,
) -> String {
    let mut req = String::with_capacity(512);
    req.push_str("GET ");
    req.push_str(path);
    // Path may already carry query params (e.g. `/api/clients/terminal?id=…`).
    // Append token with the correct separator so multi-param dials stay valid.
    if path.contains('?') {
        req.push('&');
    } else {
        req.push('?');
    }
    req.push_str("token=");
    req.push_str(&url_encode(token));
    req.push_str(" HTTP/1.1\r\n");
    req.push_str("Host: ");
    req.push_str(host);
    req.push_str("\r\n");
    req.push_str("Upgrade: websocket\r\n");
    req.push_str("Connection: Upgrade\r\n");
    req.push_str("Sec-WebSocket-Key: ");
    req.push_str(ws_key);
    req.push_str("\r\n");
    req.push_str("Sec-WebSocket-Version: 13\r\n");
    if enable_compression {
        // RFC 7692: offer permessage-deflate. Server echoes it back if accepted.
        req.push_str("Sec-WebSocket-Extensions: permessage-deflate\r\n");
    }
    req.push_str(concat!(
        "User-Agent: komari-agent-rs/",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    ));
    req.push_str("Accept: */*\r\n");
    for (name, value) in extra_headers {
        req.push_str(name);
        req.push_str(": ");
        req.push_str(value);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req
}

/// RFC 7692 + gorilla/websocket decompression trailer.
///
/// Matches `github.com/gorilla/websocket` `decompressNoContextTakeover`:
/// sync-flush bytes required by the RFC, plus an empty final stored block so
/// DEFLATE readers that require BFINAL do not return UnexpectedEof.
const PERMESSAGE_DEFLATE_TRAILER: &[u8] = &[
    0x00, 0x00, 0xFF, 0xFF, // RFC 7692 §7.2.1
    0x01, 0x00, 0x00, 0xFF, 0xFF, // empty final stored block (gorilla)
];

/// Inflate a permessage-deflate payload (RSV1 data frame body).
fn inflate_permessage_deflate(compressed: &[u8]) -> Result<Vec<u8>, WsErr> {
    let mut with_trailer = Vec::with_capacity(compressed.len() + PERMESSAGE_DEFLATE_TRAILER.len());
    with_trailer.extend_from_slice(compressed);
    with_trailer.extend_from_slice(PERMESSAGE_DEFLATE_TRAILER);
    let mut inflated = Vec::with_capacity(compressed.len().saturating_mul(2).max(64));
    crate::inflate::inflate_raw(&with_trailer, &mut inflated)
        .map_err(|e| WsErr::Protocol(format!("permessage-deflate inflate error: {e}")))?;
    if inflated.len() > MAX_FRAME_SIZE as usize {
        return Err(WsErr::Protocol(
            "permessage-deflate output exceeded 1 MiB safety limit".to_string(),
        ));
    }
    Ok(inflated)
}

/// Minimal percent-encoding for the token query-parameter value.
///
/// Encodes bytes that are not in the RFC 3986 unreserved set (ALPHA / DIGIT / "-" / "." / "_" / "~").
/// Note: `%` itself is NOT unreserved and is encoded as `%25`.
pub(crate) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4) as char);
                out.push(hex_digit(b & 0x0F) as char);
            }
        }
    }
    out
}

#[inline]
fn hex_digit(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'A' + n - 10 }
}

/// Read HTTP response headers until `\r\n\r\n` (end of headers marker).
fn read_http_response_headers(
    stream: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
) -> Result<Vec<u8>, WsErr> {
    let mut buf = Vec::with_capacity(4096);

    loop {
        let mut byte = [0u8; 1];
        let n = stream
            .read(&mut byte)
            .map_err(|e| WsErr::Io(format!("read HTTP response: {}", e)))?;
        if n == 0 {
            return Err(WsErr::Handshake(
                "connection closed before HTTP response completed".to_string(),
            ));
        }
        buf.push(byte[0]);

        // Check for terminating \r\n\r\n.
        let len = buf.len();
        if len >= 4 && buf[len - 4..] == *b"\r\n\r\n" {
            break;
        }

        if buf.len() > MAX_HTTP_RESPONSE_SIZE {
            return Err(WsErr::Handshake(
                "HTTP response headers too large".to_string(),
            ));
        }
    }

    Ok(buf)
}

/// Parse an HTTP/1.1 response text into `(status_code, sec_websocket_accept,
/// permessage_deflate_accepted)`.
fn parse_http_response(response: &str) -> Result<(u16, Option<String>, bool), WsErr> {
    let mut lines = response.lines();

    // Status line: "HTTP/1.1 101 Switching Protocols"
    let status_line = lines
        .next()
        .ok_or_else(|| WsErr::Handshake("empty HTTP response".to_string()))?;

    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(WsErr::Handshake(format!(
            "invalid status line: '{}'",
            status_line
        )));
    }

    let status_code: u16 = parts[1]
        .parse()
        .map_err(|_| WsErr::Handshake(format!("invalid status code: '{}'", parts[1])))?;

    // Parse headers: Sec-WebSocket-Accept + Sec-WebSocket-Extensions.
    let mut accept: Option<String> = None;
    let mut permessage = false;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(idx) = line.find(':') {
            let name = line[..idx].trim();
            let value = line[idx + 1..].trim();
            if name.eq_ignore_ascii_case("sec-websocket-accept") {
                accept = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("sec-websocket-extensions")
                && value.to_ascii_lowercase().contains("permessage-deflate")
            {
                permessage = true;
            }
        }
    }

    Ok((status_code, accept, permessage))
}

/// One in-flight frame plus an optional fragmented data message. The sum of
/// retained data payloads is at most 1 MiB; control frames are at most 125 B.
/// Timeouts preserve both frame offsets and message reassembly. Terminal
/// errors poison this reader until reconnection.
#[derive(Default)]
struct FrameReader {
    header: [u8; 14],
    header_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
    message: Vec<u8>,
    message_opcode: u8,
    message_compressed: bool,
    failed: bool,
}

struct ReceivedFrame {
    fin: bool,
    opcode: u8,
    compressed: bool,
    payload: Vec<u8>,
}

impl FrameReader {
    fn read_message<R: Read>(
        &mut self,
        stream: &mut R,
        compression_enabled: bool,
    ) -> Result<Option<WsMessage>, WsErr> {
        loop {
            let Some(ReceivedFrame {
                fin,
                opcode,
                compressed,
                payload,
            }) = self.read_frame(stream, compression_enabled)?
            else {
                if self.message_opcode != 0 {
                    self.failed = true;
                    return Err(WsErr::Io(
                        "unexpected EOF before final WebSocket continuation".to_string(),
                    ));
                }
                return Ok(None);
            };
            match opcode {
                8 => {
                    self.message = Vec::new();
                    self.message_opcode = 0;
                    self.message_compressed = false;
                    return Ok(Some(WsMessage::Close));
                }
                9 => return Ok(Some(WsMessage::Ping(payload))),
                10 => return Ok(Some(WsMessage::Pong(payload))),
                _ => {}
            }
            if opcode != 0 {
                self.message_opcode = opcode;
                self.message_compressed = compressed;
                self.message = payload;
            } else {
                // Avoid geometric capacity growth beyond the message limit.
                self.message.reserve_exact(payload.len());
                self.message.extend_from_slice(&payload);
            }
            if !fin {
                continue;
            }
            self.failed = true;
            let mut payload = std::mem::take(&mut self.message);
            if self.message_compressed {
                // RFC 7692 compresses the whole message, not individual frames.
                payload = inflate_permessage_deflate(&payload)?;
            }
            let message = if self.message_opcode == 1 {
                WsMessage::Text(payload)
            } else {
                WsMessage::Binary(payload)
            };
            self.message_opcode = 0;
            self.message_compressed = false;
            self.failed = false;
            return Ok(Some(message));
        }
    }

    /// Read one frame, retaining all offsets on timeout. Controls may be
    /// returned while message_opcode/message still track a fragmented message.
    fn read_frame<R: Read>(
        &mut self,
        stream: &mut R,
        compression_enabled: bool,
    ) -> Result<Option<ReceivedFrame>, WsErr> {
        if self.failed {
            return Err(WsErr::Protocol(
                "cannot resume a failed WebSocket frame reader".to_string(),
            ));
        }
        if !fill_frame_part(
            stream,
            &mut self.header[..2],
            &mut self.header_read,
            &mut self.failed,
        )? {
            if self.header_read == 0 {
                return Ok(None);
            }
            self.failed = true;
            return Err(truncated_frame());
        }

        let fin = self.header[0] & 0x80 != 0;
        let rsv1 = self.header[0] & 0x40 != 0;
        let opcode = self.header[0] & 0x0F;
        let masked = self.header[1] & 0x80 != 0;
        let short_len = self.header[1] & 0x7F;
        let control = matches!(opcode, 8..=10);
        let invalid = if self.header[0] & 0x30 != 0 {
            Some("unsupported WebSocket reserved bits")
        } else if !matches!(opcode, 0..=2 | 8..=10) {
            Some("unsupported WebSocket opcode")
        } else if control && (!fin || short_len > 125 || rsv1) {
            Some("invalid WebSocket control frame")
        } else if rsv1 && (!compression_enabled || opcode == 0) {
            Some("invalid WebSocket compression flag")
        } else if opcode == 0 && self.message_opcode == 0 {
            Some("unexpected WebSocket continuation frame")
        } else if matches!(opcode, 1 | 2) && self.message_opcode != 0 {
            Some("new WebSocket data frame before final continuation")
        } else {
            None
        };
        if let Some(message) = invalid {
            self.failed = true;
            return Err(WsErr::Protocol(message.to_string()));
        }
        let length_end = match short_len {
            126 => 4,
            127 => 10,
            _ => 2,
        };
        let header_len = length_end + if masked { 4 } else { 0 };
        if !fill_frame_part(
            stream,
            &mut self.header[..header_len],
            &mut self.header_read,
            &mut self.failed,
        )? {
            self.failed = true;
            return Err(truncated_frame());
        }
        let payload_len = match short_len {
            126 => u16::from_be_bytes(self.header[2..4].try_into().unwrap()) as u64,
            127 => u64::from_be_bytes(self.header[2..10].try_into().unwrap()),
            n => n as u64,
        };
        if payload_len > MAX_FRAME_SIZE
            || (!control && payload_len > MAX_FRAME_SIZE - self.message.len() as u64)
        {
            self.failed = true;
            return Err(WsErr::Protocol(
                "frame or reassembled message payload exceeds 1 MiB safety limit".to_string(),
            ));
        }

        // Resize only on the first visit to this frame's payload. A retry
        // retains bytes and payload_read rather than starting at byte zero.
        self.payload.resize(payload_len as usize, 0);
        if !fill_frame_part(
            stream,
            &mut self.payload,
            &mut self.payload_read,
            &mut self.failed,
        )? {
            self.failed = true;
            return Err(truncated_frame());
        }

        // A decoding error is terminal too: never reinterpret this frame's
        // remaining bytes as a new header on a caller's accidental retry.
        self.failed = true;
        let mut payload = std::mem::take(&mut self.payload);
        if masked {
            apply_mask(
                &mut payload,
                self.header[length_end..header_len].try_into().unwrap(),
            );
        }
        self.header_read = 0;
        self.payload_read = 0;
        self.failed = false;
        Ok(Some(ReceivedFrame {
            fin,
            opcode,
            compressed: rsv1,
            payload,
        }))
    }
}

/// Fill a persistent part of a frame. The offset is updated after every
/// successful read, including reads followed by a timeout in this call.
fn fill_frame_part<R: Read>(
    stream: &mut R,
    buf: &mut [u8],
    filled: &mut usize,
    failed: &mut bool,
) -> Result<bool, WsErr> {
    while *filled < buf.len() {
        match stream.read(&mut buf[*filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => *filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Current sync callers recognize this static timeout text.
                // Do not depend on localized OS messages or erase progress.
                return Err(WsErr::Io("WebSocket read timed out".to_string()));
            }
            Err(e) => {
                *failed = true;
                return Err(e.into());
            }
        }
    }
    Ok(true)
}

fn truncated_frame() -> WsErr {
    WsErr::Io("unexpected EOF within WebSocket frame".to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic transport: each step supplies bytes or an OS error.
    /// Reads never bypass a scripted timeout, even in the middle of a frame.
    struct ScriptedRead {
        steps: std::collections::VecDeque<std::io::Result<Vec<u8>>>,
        pending: std::io::Cursor<Vec<u8>>,
        consumed: usize,
    }

    impl ScriptedRead {
        fn new(steps: Vec<std::io::Result<Vec<u8>>>) -> Self {
            Self {
                steps: steps.into(),
                pending: std::io::Cursor::new(Vec::new()),
                consumed: 0,
            }
        }
    }

    impl Read for ScriptedRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.position() == self.pending.get_ref().len() as u64 {
                match self.steps.pop_front() {
                    Some(Ok(bytes)) => self.pending = std::io::Cursor::new(bytes),
                    Some(Err(error)) => return Err(error),
                    None => return Ok(0),
                }
            }
            let count = self.pending.read(buf)?;
            self.consumed += count;
            Ok(count)
        }
    }

    fn read_error(kind: std::io::ErrorKind) -> std::io::Result<Vec<u8>> {
        // Deliberately misleading text: only ErrorKind controls retryability.
        Err(std::io::Error::new(kind, "closed reset eof"))
    }

    fn test_frame(opcode: u8, payload: &[u8], masked: bool) -> Vec<u8> {
        let mut wire = vec![0x80 | opcode];
        let mask_bit = if masked { 0x80 } else { 0 };
        if payload.len() < 126 {
            wire.push(mask_bit | payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            wire.push(mask_bit | 126);
            wire.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            wire.push(mask_bit | 127);
            wire.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        let mut payload = payload.to_vec();
        if masked {
            let key = [1, 2, 3, 4];
            wire.extend_from_slice(&key);
            apply_mask(&mut payload, key);
        }
        wire.extend_from_slice(&payload);
        wire
    }

    fn assert_read_timeout(result: Result<Option<WsMessage>, WsErr>) {
        assert!(matches!(result, Err(WsErr::Io(ref s)) if s == "WebSocket read timed out"));
    }

    #[test]
    fn frame_reader_idle_timeout_is_retryable_and_interrupted_is_retried() {
        let wire = test_frame(1, b"hello", false);
        let mut stream = ScriptedRead::new(vec![
            read_error(std::io::ErrorKind::WouldBlock),
            read_error(std::io::ErrorKind::TimedOut),
            read_error(std::io::ErrorKind::Interrupted),
            Ok(wire),
        ]);
        let mut reader = FrameReader::default();
        assert_read_timeout(reader.read_message(&mut stream, false));
        assert_read_timeout(reader.read_message(&mut stream, false));
        assert_eq!(reader.header_read, 0);
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Text(b"hello".to_vec()))
        );
        assert_eq!(reader.read_message(&mut stream, false).unwrap(), None);
    }

    #[test]
    fn frame_reader_preserves_every_header_mask_and_payload_split_across_timeout() {
        for masked in [false, true] {
            for payload in [b"hello".to_vec(), vec![b'x'; 130], vec![b'y'; 65536]] {
                let wire = test_frame(1, &payload, masked);
                let header_len = wire.len() - payload.len();
                let splits: Vec<usize> = if wire.len() < 200 {
                    (1..wire.len()).collect()
                } else {
                    (1..=header_len)
                        .chain([header_len + 1, header_len + 32768, wire.len() - 1])
                        .collect()
                };
                for split in splits {
                    let next_frame = test_frame(9, b"next", false);
                    let mut tail = wire[split..].to_vec();
                    tail.extend_from_slice(&next_frame);
                    let mut stream = ScriptedRead::new(vec![
                        Ok(wire[..split].to_vec()),
                        read_error(std::io::ErrorKind::TimedOut),
                        read_error(std::io::ErrorKind::WouldBlock),
                        Ok(tail),
                    ]);
                    let mut reader = FrameReader::default();
                    assert_read_timeout(reader.read_message(&mut stream, false));
                    assert_eq!(reader.header_read + reader.payload_read, split);
                    assert_read_timeout(reader.read_message(&mut stream, false));
                    assert_eq!(reader.header_read + reader.payload_read, split);
                    assert_eq!(
                        reader.read_message(&mut stream, false).unwrap(),
                        Some(WsMessage::Text(payload.clone())),
                        "split {split} of {}",
                        wire.len()
                    );
                    assert_eq!(
                        stream.consumed,
                        wire.len(),
                        "must not consume the next frame"
                    );
                    assert_eq!(
                        reader.read_message(&mut stream, false).unwrap(),
                        Some(WsMessage::Ping(b"next".to_vec()))
                    );
                }
            }
        }
    }

    #[test]
    fn frame_reader_can_resume_multiple_timeouts_within_one_frame() {
        let mut wire = test_frame(9, b"ping", false);
        wire.extend_from_slice(&test_frame(1, b"text", false));
        wire.extend_from_slice(&test_frame(8, &[], false));
        let mut steps = Vec::new();
        for byte in wire {
            steps.push(read_error(std::io::ErrorKind::TimedOut));
            steps.push(Ok(vec![byte]));
        }
        let mut stream = ScriptedRead::new(steps);
        let mut reader = FrameReader::default();
        for expected in [
            WsMessage::Ping(b"ping".to_vec()),
            WsMessage::Text(b"text".to_vec()),
            WsMessage::Close,
        ] {
            loop {
                match reader.read_message(&mut stream, false) {
                    Ok(Some(message)) => {
                        assert_eq!(message, expected);
                        break;
                    }
                    result => assert_read_timeout(result),
                }
            }
        }
        assert_eq!(reader.read_message(&mut stream, false).unwrap(), None);
    }

    #[test]
    fn frame_reader_truncated_frames_and_resets_are_not_clean_eof() {
        let wire = test_frame(1, &[b'x'; 130], true);
        for split in [1, 2, 3, 4, 5, 7, 8, 9, wire.len() - 1] {
            let mut stream = ScriptedRead::new(vec![Ok(wire[..split].to_vec())]);
            let mut reader = FrameReader::default();
            assert!(matches!(
                reader.read_message(&mut stream, false),
                Err(WsErr::Io(_))
            ));
            assert!(reader.failed);
            assert!(reader.read_message(&mut stream, false).is_err());
            assert_eq!(stream.consumed, split);
        }
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::ConnectionAborted,
        ] {
            for prefix in [Vec::new(), wire[..9].to_vec()] {
                let mut steps = Vec::new();
                if !prefix.is_empty() {
                    steps.push(Ok(prefix));
                }
                steps.push(read_error(kind));
                let mut stream = ScriptedRead::new(steps);
                let mut reader = FrameReader::default();
                assert!(matches!(
                    reader.read_message(&mut stream, false),
                    Err(WsErr::Io(_))
                ));
                assert!(reader.failed);
            }
        }
    }

    #[test]
    fn frame_reader_keeps_one_mib_cap_before_payload_allocation() {
        let mut header = vec![0x81, 127];
        header.extend_from_slice(&(MAX_FRAME_SIZE + 1).to_be_bytes());
        let mut stream = ScriptedRead::new(vec![Ok(header.clone()), Ok(vec![b'x'; 10])]);
        let mut reader = FrameReader::default();
        assert!(matches!(
            reader.read_message(&mut stream, false),
            Err(WsErr::Protocol(_))
        ));
        assert!(reader.payload.is_empty());
        assert_eq!(reader.payload.capacity(), 0);
        assert_eq!(stream.consumed, header.len());

        let payload = vec![b'x'; MAX_FRAME_SIZE as usize];
        let mut stream = ScriptedRead::new(vec![Ok(test_frame(2, &payload, false))]);
        let mut reader = FrameReader::default();
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Binary(payload))
        );
    }

    #[test]
    fn frame_reader_compressed_text_survives_payload_timeout() {
        let compressed = hex_decode(
            "04c0310a84301085e1bbfc7558c2967315b19038852251f226362177f71b9cba6b7b0ac6ff97491c3b4626d15cfd0a6ce0afd710b6ac09c5165d18eaa5b8c49c1f00",
        );
        let mut wire = test_frame(1, &compressed, false);
        wire[0] |= 0x40;
        let mut stream = ScriptedRead::new(vec![
            Ok(wire[..20].to_vec()),
            read_error(std::io::ErrorKind::TimedOut),
            Ok(wire[20..].to_vec()),
        ]);
        let mut reader = FrameReader::default();
        assert_read_timeout(reader.read_message(&mut stream, true));
        assert_eq!(
            reader.read_message(&mut stream, true).unwrap(),
            Some(WsMessage::Text(
                br#"{"jsonrpc":"2.0","id":"0","result":{"events":[],"status":"success"}}"#.to_vec()
            ))
        );
    }

    #[test]
    fn frame_reader_resumes_header_extended_length_and_payload_on_real_socket_timeout() {
        use std::sync::mpsc;
        let wire = test_frame(1, &[b'x'; 130], false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(25)))
            .unwrap();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut start = 0;
            for end in [1, 3, 10, wire.len()] {
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                socket.write_all(&wire[start..end]).unwrap();
                written_tx.send(()).unwrap();
                start = end;
            }
        });
        let mut reader = FrameReader::default();
        for consumed in [1, 3, 10] {
            resume_tx.send(()).unwrap();
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_read_timeout(reader.read_message(&mut client, false));
            assert_eq!(reader.header_read + reader.payload_read, consumed);
        }
        resume_tx.send(()).unwrap();
        written_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            reader.read_message(&mut client, false).unwrap(),
            Some(WsMessage::Text(vec![b'x'; 130]))
        );
        server.join().unwrap();
    }

    fn fragment_frame(opcode: u8, payload: &[u8], fin: bool, compressed: bool) -> Vec<u8> {
        let mut wire = test_frame(opcode, payload, false);
        if !fin {
            wire[0] &= !0x80;
        }
        if compressed {
            wire[0] |= 0x40;
        }
        wire
    }

    #[test]
    fn frame_reader_reassembles_continuations_with_interleaved_controls() {
        let mut wire = fragment_frame(1, b"hel", false, false);
        wire.extend_from_slice(&fragment_frame(0, b"lo ", false, false));
        wire.extend_from_slice(&test_frame(9, b"ping", false));
        wire.extend_from_slice(&test_frame(10, b"pong", false));
        wire.extend_from_slice(&fragment_frame(0, b"world", true, false));
        wire.extend_from_slice(&test_frame(2, b"binary", false));
        let mut reader = FrameReader::default();
        let mut stream = ScriptedRead::new(vec![Ok(wire)]);
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Ping(b"ping".to_vec()))
        );
        assert_eq!(reader.message_opcode, 1);
        assert_eq!(reader.message, b"hello ");
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Pong(b"pong".to_vec()))
        );
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Text(b"hello world".to_vec()))
        );
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Binary(b"binary".to_vec()))
        );
    }

    #[test]
    fn frame_reader_reassembles_compressed_message_across_timeouts_and_ping() {
        // Gorilla v1.5.3 NextWriter/flushFrame(false) sends opcode=1 + RSV1
        // once, then opcode=0 without RSV1. Inflate only after final FIN.
        let compressed = hex_decode(
            "04c0310a84301085e1bbfc7558c2967315b19038852251f226362177f71b9cba6b7b0ac6ff97491c3b4626d15cfd0a6ce0afd710b6ac09c5165d18eaa5b8c49c1f00",
        );
        let mut wire = fragment_frame(1, &compressed[..10], false, true);
        wire.extend_from_slice(&test_frame(9, b"alive", false));
        wire.extend_from_slice(&fragment_frame(0, &compressed[10..30], false, false));
        wire.extend_from_slice(&fragment_frame(0, &compressed[30..], true, false));
        let mut steps = Vec::new();
        for byte in wire {
            steps.push(Ok(vec![byte]));
            steps.push(read_error(std::io::ErrorKind::TimedOut));
        }
        let mut stream = ScriptedRead::new(steps);
        let mut reader = FrameReader::default();
        for expected in [
            WsMessage::Ping(b"alive".to_vec()),
            WsMessage::Text(
                br#"{"jsonrpc":"2.0","id":"0","result":{"events":[],"status":"success"}}"#.to_vec(),
            ),
        ] {
            loop {
                match reader.read_message(&mut stream, true) {
                    Ok(Some(message)) => {
                        assert_eq!(message, expected);
                        break;
                    }
                    result => assert_read_timeout(result),
                }
            }
        }
        assert_eq!(reader.message_opcode, 0);
        assert!(reader.message.is_empty());
    }

    #[test]
    fn frame_reader_validates_continuation_sequences_control_fin_and_reserved_bits() {
        let first = fragment_frame(1, b"first", false, false);
        let cases = [
            (Vec::new(), fragment_frame(0, b"orphan", true, false), true),
            (first.clone(), test_frame(1, b"new text", false), true),
            (first.clone(), test_frame(2, b"new binary", false), true),
            (
                first.clone(),
                fragment_frame(0, b"bad RSV1", true, true),
                true,
            ),
            (
                Vec::new(),
                fragment_frame(1, b"unnegotiated", true, true),
                false,
            ),
            (Vec::new(), vec![0xA1, 0], true), // RSV2
            (Vec::new(), vec![0x91, 0], true), // RSV3
            (Vec::new(), vec![0x83, 0], true), // reserved opcode
        ];
        for (prefix, frame, compression_enabled) in cases {
            let mut wire = prefix;
            wire.extend_from_slice(&frame);
            let mut stream = ScriptedRead::new(vec![Ok(wire)]);
            let mut reader = FrameReader::default();
            assert!(matches!(
                reader.read_message(&mut stream, compression_enabled),
                Err(WsErr::Protocol(_))
            ));
            assert!(reader.failed);
        }
        for opcode in [8, 9, 10] {
            for (fin, rsv1, len) in [(false, false, 0), (true, true, 0), (true, false, 126)] {
                let wire = fragment_frame(opcode, &vec![0; len], fin, rsv1);
                let mut stream = ScriptedRead::new(vec![Ok(wire)]);
                let mut reader = FrameReader::default();
                assert!(matches!(
                    reader.read_message(&mut stream, true),
                    Err(WsErr::Protocol(_))
                ));
                assert!(reader.failed);
                assert_eq!(
                    stream.consumed, 2,
                    "reject invalid control header before allocating payload"
                );
            }
        }
    }

    #[test]
    fn frame_reader_bounds_whole_fragmented_message_before_next_payload_allocation() {
        let first = fragment_frame(2, &vec![b'a'; MAX_FRAME_SIZE as usize / 2], false, false);
        let mut continuation_header = vec![0x80, 127];
        continuation_header.extend_from_slice(&(MAX_FRAME_SIZE / 2 + 1).to_be_bytes());
        let consumed = first.len() + continuation_header.len();
        let mut wire = first.clone();
        wire.extend_from_slice(&continuation_header);
        let mut reader = FrameReader::default();
        let mut stream = ScriptedRead::new(vec![Ok(wire)]);
        assert!(matches!(
            reader.read_message(&mut stream, false),
            Err(WsErr::Protocol(_))
        ));
        assert_eq!(stream.consumed, consumed);
        assert!(reader.payload.is_empty());
        assert_eq!(reader.message.len(), MAX_FRAME_SIZE as usize / 2);

        let mut wire = first;
        wire.extend_from_slice(&fragment_frame(
            0,
            &vec![b'b'; MAX_FRAME_SIZE as usize / 2],
            true,
            false,
        ));
        let mut reader = FrameReader::default();
        let mut stream = ScriptedRead::new(vec![Ok(wire)]);
        let Some(WsMessage::Binary(payload)) = reader.read_message(&mut stream, false).unwrap()
        else {
            panic!("binary message");
        };
        assert_eq!(payload.len(), MAX_FRAME_SIZE as usize);
        assert_eq!(payload[0], b'a');
        assert_eq!(payload[payload.len() - 1], b'b');
    }

    #[test]
    fn frame_reader_fragmented_eof_is_error_and_close_can_interrupt_reassembly() {
        let first = fragment_frame(1, b"unfinished", false, false);
        let mut reader = FrameReader::default();
        let mut stream = ScriptedRead::new(vec![Ok(first.clone())]);
        assert!(matches!(
            reader.read_message(&mut stream, false),
            Err(WsErr::Io(_))
        ));
        assert!(reader.failed);
        let mut wire = first;
        wire.extend_from_slice(&test_frame(8, &[], false));
        let mut reader = FrameReader::default();
        let mut stream = ScriptedRead::new(vec![Ok(wire)]);
        assert_eq!(
            reader.read_message(&mut stream, false).unwrap(),
            Some(WsMessage::Close)
        );
        assert!(reader.message.is_empty());
        assert_eq!(reader.message_opcode, 0);
    }

    // ── parse_endpoint ──

    #[test]
    fn endpoint_authority_is_shared_and_rejects_injection() {
        assert_eq!(
            parse_endpoint("https://[::1]").unwrap(),
            ("::1".into(), 443, "/api/clients/v2/rpc".into())
        );
        assert_eq!(
            parse_endpoint("wss://[::1]:8443/rpc?x=1").unwrap(),
            ("::1".into(), 8443, "/rpc?x=1".into())
        );
        for endpoint in [
            "https://host:bad",
            "https://host\r\nX:yes",
            "https://user:synthetic@host", // # leak-guard-allow:LG-F4C6A155 synthetic rejected userinfo fixture
            "https://host/#fragment",
            "https://[::1",
        ] {
            assert!(parse_endpoint(endpoint).is_err());
        }
        let request = build_upgrade_request(
            &crate::http::host_header("::1", 8443, true),
            "/rpc",
            "synthetic",
            "key",
            &[],
            false,
        );
        assert!(request.contains("Host: [::1]:8443\r\n"));
        assert!(request.contains(&format!(
            "User-Agent: komari-agent-rs/{}\r\n",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn parse_endpoint_https_defaults() {
        let (host, port, path) = parse_endpoint("https://example.com").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/api/clients/v2/rpc");
    }

    #[test]
    fn parse_endpoint_with_path() {
        let (host, port, path) = parse_endpoint("https://example.com/api/clients/v2/rpc").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/api/clients/v2/rpc");
    }

    #[test]
    fn parse_endpoint_custom_port() {
        let (host, port, path) = parse_endpoint("https://example.com:8443/api/v2/ws").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8443);
        assert_eq!(path, "/api/v2/ws");
    }

    #[test]
    fn parse_endpoint_ip_addr() {
        let (host, port, path) = parse_endpoint("https://192.168.1.1:9999/test").unwrap();
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 9999);
        assert_eq!(path, "/test");
    }

    #[test]
    fn parse_endpoint_no_path() {
        let (host, port, path) = parse_endpoint("https://127.0.0.1").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 443);
        assert_eq!(path, "/api/clients/v2/rpc");
    }

    #[test]
    fn parse_endpoint_wss_scheme() {
        let (host, port, path) = parse_endpoint("wss://example.com/ws").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/ws");
    }

    // ── apply_mask ──

    #[test]
    fn mask_empty_slice() {
        let mut data: [u8; 0] = [];
        apply_mask(&mut data, [0x12, 0x34, 0x56, 0x78]);
        // No panic = pass.
    }

    #[test]
    fn mask_twice_is_identity() {
        let original = *b"hello world!";
        let mut data = original;
        let mask = [0xAB, 0xCD, 0xEF, 0x01];
        apply_mask(&mut data, mask);
        assert_ne!(&data[..], &original[..]);
        apply_mask(&mut data, mask);
        assert_eq!(&data[..], &original[..]);
    }

    #[test]
    fn mask_zero_key_noop() {
        let mut data = *b"test";
        apply_mask(&mut data, [0, 0, 0, 0]);
        assert_eq!(&data[..], b"test");
    }

    #[test]
    fn mask_known_vector() {
        // RFC 6455 §4.7 example: "Hello" with mask [0x37, 0xfa, 0x21, 0x3d]
        // → [0x7f, 0x9f, 0x4d, 0x51, 0x58]
        let mut data = *b"Hello";
        apply_mask(&mut data, [0x37, 0xfa, 0x21, 0x3d]);
        assert_eq!(&data[..], &[0x7f, 0x9f, 0x4d, 0x51, 0x58]);
    }

    // ── build_frame_header ──

    #[test]
    fn frame_header_small_payload() {
        let (buf, len, _) = build_frame_header(WsOpcode::Text as u8, 5, true, [0; 4], false);
        assert_eq!(len, 2);
        assert_eq!(buf[0] & 0x0F, 1); // opcode=Text
        assert_eq!(buf[0] & 0x80, 0x80); // FIN=1
        assert_eq!(buf[1] & 0x7F, 5); // payload_len=5
        assert_eq!(buf[1] & 0x80, 0x80); // MASK=1
    }

    #[test]
    fn frame_header_16bit_extended() {
        let (buf, len, _) = build_frame_header(WsOpcode::Binary as u8, 256, true, [0; 4], false);
        assert_eq!(len, 4); // 2 fixed + 2 extended
        assert_eq!(buf[1] & 0x7F, 126); // marker
        let ext = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(ext, 256);
    }

    #[test]
    fn frame_header_unmasked() {
        let (buf, len, _) = build_frame_header(WsOpcode::Ping as u8, 0, false, [0; 4], false);
        assert_eq!(len, 2);
        assert_eq!(buf[0] & 0x0F, 9); // opcode=Ping
        assert_eq!(buf[1], 0x00); // no mask, len=0
    }

    #[test]
    fn frame_header_large_64bit() {
        let (buf, len, _) = build_frame_header(WsOpcode::Binary as u8, 70000, true, [0; 4], false);
        assert_eq!(len, 10); // 2 fixed + 8 extended
        assert_eq!(buf[1] & 0x7F, 127); // marker
        let ext = u64::from_be_bytes(buf[2..10].try_into().unwrap());
        assert_eq!(ext, 70000);
    }

    // ── build_upgrade_request ──

    #[test]
    fn upgrade_request_contains_all_headers() {
        let req = build_upgrade_request(
            "example.com",
            "/api/clients/v2/rpc",
            "test-token-123",
            "dGhlIHNhbXBsZSBub25jZQ==",
            &[],
            false,
        );
        assert!(req.starts_with("GET /api/clients/v2/rpc?token=test-token-123 HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com\r\n"));
        assert!(req.contains("Upgrade: websocket\r\n"));
        assert!(req.contains("Connection: Upgrade\r\n"));
        assert!(req.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(!req.contains("permessage-deflate"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn upgrade_request_url_encodes_token() {
        let req = build_upgrade_request("host", "/path", "token with spaces!", "key", &[], false);
        assert!(req.contains("token%20with%20spaces%21"));
    }

    #[test]
    fn upgrade_request_appends_token_when_path_has_query() {
        let req = build_upgrade_request(
            "host",
            "/api/clients/terminal?id=abc123",
            "tok",
            "key",
            &[],
            false,
        );
        assert!(req.starts_with("GET /api/clients/terminal?id=abc123&token=tok HTTP/1.1\r\n"));
    }

    #[test]
    fn upgrade_request_with_compression() {
        let req = build_upgrade_request("host", "/path", "tok", "key", &[], true);
        assert!(req.contains("Sec-WebSocket-Extensions: permessage-deflate\r\n"));
    }

    // ── url_encode ──

    #[test]
    fn url_encode_alphanumeric_unchanged() {
        assert_eq!(url_encode("abc123"), "abc123");
    }

    #[test]
    fn url_encode_special_chars() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a+b"), "a%2Bb");
        assert_eq!(url_encode("key=value"), "key%3Dvalue");
    }

    // ── parse_http_response ──

    #[test]
    fn parse_101_with_accept() {
        let response = "HTTP/1.1 101 Switching Protocols\r\n\
                        Upgrade: websocket\r\n\
                        Connection: Upgrade\r\n\
                        Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                        \r\n";
        let (code, accept, permessage) = parse_http_response(response).unwrap();
        assert_eq!(code, 101);
        assert_eq!(accept.as_deref(), Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        assert!(!permessage);
    }

    #[test]
    fn parse_401_no_accept() {
        let response = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
        let (code, accept, _) = parse_http_response(response).unwrap();
        assert_eq!(code, 401);
        assert!(accept.is_none());
    }

    #[test]
    fn parse_101_with_permessage_deflate() {
        let response = "HTTP/1.1 101 Switching Protocols\r\n\
                        Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                        Sec-WebSocket-Extensions: permessage-deflate; server_no_context_takeover\r\n\
                        \r\n";
        let (code, _accept, permessage) = parse_http_response(response).unwrap();
        assert_eq!(code, 101);
        assert!(permessage);
    }

    /// Gorilla strips Z_SYNC_FLUSH (`00 00 FF FF`) on the wire. RFC-only re-append
    /// is **not** enough for compress/flate streams: Go's flate reader returns
    /// UnexpectedEof without the extra empty final stored block that gorilla
    /// appends in `decompressNoContextTakeover`.
    ///
    /// Vector: `compress/flate` level=1 + gorilla `truncWriter` (Flush, hold last 4).
    #[test]
    fn inflate_permessage_deflate_gorilla_jsonrpc_ack() {
        let framed = hex_decode(
            "04c0310a84301085e1bbfc7558c2967315b19038852251f226362177f71b9cba6b7b0ac6ff97491c3b4626d15cfd0a6ce0afd710b6ac09c5165d18eaa5b8c49c1f00",
        );
        let out = inflate_permessage_deflate(&framed).expect("gorilla trailer inflate");
        assert_eq!(
            out.as_slice(),
            br#"{"jsonrpc":"2.0","id":"0","result":{"events":[],"status":"success"}}"#
        );
    }

    #[test]
    fn inflate_permessage_deflate_empty_payload() {
        // empty message after truncWriter: single 0x00 on the wire
        let out = inflate_permessage_deflate(&[0x00]).expect("empty");
        assert_eq!(out.as_slice(), b"");
    }

    #[test]
    fn inflate_permessage_rfc_only_tail_fails_on_flate_vector() {
        let framed = hex_decode(
            "04c0310a84301085e1bbfc7558c2967315b19038852251f226362177f71b9cba6b7b0ac6ff97491c3b4626d15cfd0a6ce0afd710b6ac09c5165d18eaa5b8c49c1f00",
        );
        let mut only_rfc = framed.clone();
        only_rfc.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
        let mut out = Vec::new();
        let err = crate::inflate::inflate_raw(&only_rfc, &mut out);
        assert!(
            err.is_err(),
            "RFC-only trailer must fail on gorilla/flate wire (got ok, {} bytes)",
            out.len()
        );
        // Clean path succeeds
        inflate_permessage_deflate(&framed).expect("gorilla path");
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn parse_invalid_status_line() {
        assert!(matches!(
            parse_http_response("NotHTTP\r\n\r\n").unwrap_err(),
            WsErr::Handshake(_)
        ));
    }

    #[test]
    fn parse_empty_response() {
        assert!(matches!(
            parse_http_response("").unwrap_err(),
            WsErr::Handshake(_)
        ));
    }

    // ── WsOpcode round-trip ──

    #[test]
    fn opcode_round_trip() {
        let cases = [
            (0u8, WsOpcode::Continuation),
            (1, WsOpcode::Text),
            (2, WsOpcode::Binary),
            (8, WsOpcode::Close),
            (9, WsOpcode::Ping),
            (10, WsOpcode::Pong),
        ];
        for (raw, expected) in cases {
            let parsed = WsOpcode::try_from(raw).unwrap();
            assert_eq!(parsed, expected, "opcode {raw} mismatch");
            assert_eq!(parsed as u8, raw, "repr mismatch for {raw}");
        }
    }

    #[test]
    fn opcode_invalid() {
        assert!(WsOpcode::try_from(3).is_err());
        assert!(WsOpcode::try_from(15).is_err());
        assert!(WsOpcode::try_from(7).is_err());
        assert!(WsOpcode::try_from(11).is_err());
    }

    // ── WsErr Display + From ──

    #[test]
    fn ws_err_display_handshake() {
        let e = WsErr::Handshake("test error".to_string());
        let s = e.to_string();
        assert!(s.contains("test error"));
        assert!(s.contains("handshake"));
    }

    #[test]
    fn ws_err_display_protocol() {
        let e = WsErr::Protocol("bad opcode".to_string());
        assert!(e.to_string().contains("bad opcode"));
    }

    #[test]
    fn ws_err_from_io() {
        let e: WsErr = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused").into();
        assert!(e.to_string().contains("refused"));
    }

    // ── Handshake integration (RFC 6455 §4.2.2 test vector) ──

    #[test]
    fn ws_accept_key_rfc_test_vector() {
        // Client key:  "dGhlIHNhbXBsZSBub25jZQ=="
        // Expected:    "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        let accept = crypto::ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        let expected = b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        assert_eq!(&accept, expected);
    }

    #[test]
    fn full_handshake_roundtrip() {
        let key_buf = crypto::ws_generate_key().expect("ws_generate_key failed");
        let key_str = core::str::from_utf8(&key_buf).unwrap();
        assert_eq!(key_str.len(), 24);

        let accept = crypto::ws_accept_key(key_str);
        assert_eq!(accept.len(), 28);

        // Verify correct key passes verification.
        assert!(crypto::ws_verify_accept(&accept, &accept));

        // Tampered key fails.
        let mut tampered = accept;
        tampered[5] ^= 0x01;
        assert!(!crypto::ws_verify_accept(&tampered, &accept));
    }
}
