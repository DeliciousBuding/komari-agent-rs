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
//   - D:/Code/Projects/edgehub/komari-agent-rs/docs/plan/spec.md (DD3)
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

/// Maximum WebSocket frame payload accepted on read (16 MB).
const MAX_FRAME_SIZE: u64 = 16 * 1024 * 1024;

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
        let ws_key_bytes = crypto::ws_generate_key()
            .map_err(|e| WsErr::Io(format!("generate WS key: {}", e)))?;
        let ws_key_str = core::str::from_utf8(&ws_key_bytes).map_err(|_| {
            WsErr::Handshake("generated WebSocket key is not valid UTF-8".to_string())
        })?;

        let upgrade_request = build_upgrade_request(
            &host,
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

    /// Read and parse the next WebSocket frame.
    ///
    /// Blocks until a complete frame arrives, subject to the socket read
    /// timeout.  Returns `Ok(None)` if the stream returns 0 bytes before a
    /// complete frame header (clean EOF).
    ///
    /// Fragmented frames (FIN=0) are rejected — we do not buffer continuation
    /// frames.  Komari protocol messages are always single-frame.
    pub fn read_message(&mut self) -> Result<Option<WsMessage>, WsErr> {
        // ── Read first 2 fixed bytes ──
        let mut buf2 = [0u8; 2];
        match read_exact(&mut self.stream, &mut buf2) {
            Ok(()) => {}
            Err(WsErr::Io(ref e)) if is_eof_string(e) => return Ok(None),
            Err(e) => return Err(e),
        }

        let fin = (buf2[0] & 0x80) != 0;
        let rsv1 = (buf2[0] & 0x40) != 0;
        let opcode_raw = buf2[0] & 0x0F;
        let masked = (buf2[1] & 0x80) != 0;
        let mut payload_len = (buf2[1] & 0x7F) as u64;

        // ── Extended payload length ──
        if payload_len == 126 {
            let mut ext = [0u8; 2];
            read_exact(&mut self.stream, &mut ext)?;
            payload_len = u16::from_be_bytes(ext) as u64;
        } else if payload_len == 127 {
            let mut ext = [0u8; 8];
            read_exact(&mut self.stream, &mut ext)?;
            payload_len = u64::from_be_bytes(ext);
        }

        // ── Size guard ──
        if payload_len > MAX_FRAME_SIZE {
            return Err(WsErr::Protocol(format!(
                "frame payload too large: {} bytes (max {})",
                payload_len, MAX_FRAME_SIZE
            )));
        }

        // ── Masking key ──
        let mut mask_key = [0u8; 4];
        if masked {
            read_exact(&mut self.stream, &mut mask_key)?;
        }

        // ── Payload ──
        let mut payload = vec![0u8; payload_len as usize];
        if payload_len > 0 {
            read_exact(&mut self.stream, &mut payload)?;
        }

        if masked {
            apply_mask(&mut payload, mask_key);
        }

        // permessage-deflate (RFC 7692): a data frame (text/binary) with RSV1
        // set carries a compressed payload. Re-append the sync-flush tail that
        // the sender stripped, then inflate. Control frames are never compressed.
        if rsv1 && self.compression_enabled && (opcode_raw == 1 || opcode_raw == 2) {
            let mut tail = payload.clone();
            tail.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
            let mut inflated = Vec::with_capacity(payload.len() * 2);
            crate::inflate::inflate_raw(&tail, &mut inflated).map_err(|e| {
                WsErr::Protocol(format!("permessage-deflate inflate error: {:?}", e))
            })?;
            if inflated.len() > 64 * 1024 * 1024 {
                return Err(WsErr::Protocol(
                    "permessage-deflate output exceeded 64 MiB safety limit".to_string(),
                ));
            }
            payload = inflated;
        }

        // ── Fragmentation guard ──
        if !fin {
            return Err(WsErr::Protocol(
                "fragmented frames not supported".to_string(),
            ));
        }

        // ── Opcode dispatch ──
        match opcode_raw {
            1 => Ok(Some(WsMessage::Text(payload))),
            2 => Ok(Some(WsMessage::Binary(payload))),
            8 => Ok(Some(WsMessage::Close)),
            9 => Ok(Some(WsMessage::Ping(payload))),
            10 => Ok(Some(WsMessage::Pong(payload))),
            _ => Err(WsErr::Protocol(format!(
                "unknown or unsupported opcode: {}",
                opcode_raw
            ))),
        }
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
    // Strip scheme.
    let rest = if let Some(idx) = endpoint.find("://") {
        &endpoint[idx + 3..]
    } else {
        endpoint
    };

    // Split host[:port] from path.
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/api/clients/v2/rpc"),
    };

    // Split host from port.
    let (host, port) = match host_port.rfind(':') {
        Some(idx) => {
            let cand_host = &host_port[..idx];
            let cand_port = &host_port[idx + 1..];

            // Skip if the colon is for an IPv6 address (e.g. "[::1]:port").
            let is_ipv6_bracket = cand_host.starts_with('[') && !cand_host.contains(']');

            if is_ipv6_bracket {
                // The colon is part of the IPv6 address; keep looking for a port.
                // For simplicity: if no closing bracket, treat the whole thing as
                // host with default port (IPv6 without port is rare in configs).
                (host_port.to_string(), 443)
            } else {
                match cand_port.parse::<u16>() {
                    Ok(p) => {
                        let h = cand_host.trim_start_matches('[').trim_end_matches(']');
                        (h.to_string(), p)
                    }
                    Err(_) => (host_port.to_string(), 443),
                }
            }
        }
        None => (host_port.to_string(), 443),
    };

    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

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
    req.push_str("User-Agent: komari-agent-rs/0.1.0\r\n");
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
        if len >= 4 && buf[len - 4..] == [b'\r', b'\n', b'\r', b'\n'] {
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

/// Read exactly `buf.len()` bytes into `buf`, retrying on partial reads.
fn read_exact(
    stream: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    buf: &mut [u8],
) -> Result<(), WsErr> {
    let mut total = 0;
    while total < buf.len() {
        let n = stream.read(&mut buf[total..])?;
        if n == 0 {
            return Err(WsErr::Io("connection closed unexpectedly".to_string()));
        }
        total += n;
    }
    Ok(())
}

/// Heuristic to detect EOF/connection-closed errors from their message text.
fn is_eof_string(s: &str) -> bool {
    s.contains("closed") || s.contains("eof") || s.contains("end of file") || s.contains("reset")
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_endpoint ──

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
        assert!(req.starts_with(
            "GET /api/clients/terminal?id=abc123&token=tok HTTP/1.1\r\n"
        ));
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
