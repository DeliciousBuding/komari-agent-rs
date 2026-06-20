// crypto.rs — SHA-1 (RFC 3174) + Base64 (RFC 4648) for WebSocket handshake.
//
// Design constraints:
//   - Stack-only: no heap allocations in SHA-1/Base64 paths. The random fill in
//     ws_generate_key opens /dev/urandom (or BCryptGenRandom on Windows) which
//     may allocate internally, but this is cold-path (once per connection).
//   - No dependencies beyond std.
//   - All arithmetic is u32, wrapping for overflow safety.

#[cfg(unix)]
use std::io::Read;

// ── Constants ────────────────────────────────────────────────────────────

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// WebSocket magic GUID per RFC 6455 §4.2.2.
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// SHA-1 initial hash values (H0..H4), per RFC 3174 §6.1.
const SHA1_INIT: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

// ── Internal helpers ─────────────────────────────────────────────────────

/// Left-rotate a 32-bit word by `n` bits.
#[inline]
const fn left_rotate(x: u32, n: u32) -> u32 {
    (x << n) | (x >> (32 - n))
}

/// Read a big-endian u32 from 4 bytes.  Panics (debug) if slice is too short.
#[inline]
fn read_u32_be(bytes: &[u8]) -> u32 {
    debug_assert!(bytes.len() >= 4);
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | (bytes[3] as u32)
}

/// Process one 64-byte (512-bit) SHA-1 block, updating the 5-word hash state.
fn process_block(h: &mut [u32; 5], block: &[u8]) {
    debug_assert!(block.len() == 64, "SHA-1 block must be exactly 64 bytes");

    // Message schedule: 16 words from block + 64 extended words = 80 total.
    let mut w = [0u32; 80];

    // 1. Copy 16 32-bit big-endian words from the block into w[0..15].
    for i in 0..16 {
        w[i] = read_u32_be(&block[i * 4..(i + 1) * 4]);
    }

    // 2. Extend to 80 words: w[t] = ROTL1(w[t-3] ^ w[t-8] ^ w[t-14] ^ w[t-16]).
    for t in 16..80 {
        w[t] = left_rotate(w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16], 1);
    }

    // 3. Initialize working variables.
    let mut a = h[0];
    let mut b = h[1];
    let mut c = h[2];
    let mut d = h[3];
    let mut e = h[4];

    // 4. 80 rounds.
    for t in 0..80 {
        let (f, k) = match t {
            0..=19 => {
                // Ch: (b & c) | ((~b) & d)
                let f = (b & c) | ((!b) & d);
                (f, 0x5A827999u32)
            }
            20..=39 => {
                // Parity: b ^ c ^ d
                (b ^ c ^ d, 0x6ED9EBA1u32)
            }
            40..=59 => {
                // Maj: (b & c) | (b & d) | (c & d)
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32)
            }
            _ => {
                // Parity: b ^ c ^ d
                (b ^ c ^ d, 0xCA62C1D6u32)
            }
        };

        let temp = left_rotate(a, 5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(w[t]);
        e = d;
        d = c;
        c = left_rotate(b, 30);
        b = a;
        a = temp;
    }

    // 5. Accumulate into hash state.
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

/// Fill `buf` with cryptographically-secure random bytes.
///
/// Cold-path helper for `ws_generate_key`.  May allocate internally
/// (OS file / syscall overhead), but called at most once per WebSocket
/// connection setup.
fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::fs::File;
        File::open("/dev/urandom")
            .expect("failed to open /dev/urandom")
            .read_exact(buf)
            .expect("failed to read random bytes from /dev/urandom");
    }

    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            // https://learn.microsoft.com/en-us/windows/win32/api/bcrypt/nf-bcrypt-bcryptgenrandom
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
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert_eq!(
            status, 0,
            "BCryptGenRandom failed with status 0x{status:08X}"
        );
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Compile-time guard: target not supported for CSPRNG.
        // Supported: Linux, macOS, FreeBSD (unix), Windows.
        compile_error!("crypto: unsupported target for random byte generation");
    }
}

/// Generic fixed-size Base64 encoder (RFC 4648).
///
/// The output size `OUT` must equal `ceil(input.len() / 3) * 4`.  For a
/// 20‑byte SHA-1 digest, `OUT` is 28.  For a 16‑byte random nonce, `OUT`
/// is 24.
fn base64_encode_fixed<const OUT: usize>(input: &[u8]) -> [u8; OUT] {
    debug_assert!(
        (input.len() + 2) / 3 * 4 == OUT,
        "base64 output size mismatch: expected {}, got {OUT}",
        (input.len() + 2) / 3 * 4,
    );

    let mut out = [0u8; OUT];
    let mut pos = 0;
    let mut i = 0;

    // Process full 3-byte groups → 4 Base64 characters.
    while i + 3 <= input.len() {
        let triple =
            ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out[pos] = BASE64_TABLE[((triple >> 18) & 0x3F) as usize];
        out[pos + 1] = BASE64_TABLE[((triple >> 12) & 0x3F) as usize];
        out[pos + 2] = BASE64_TABLE[((triple >> 6) & 0x3F) as usize];
        out[pos + 3] = BASE64_TABLE[(triple & 0x3F) as usize];
        pos += 4;
        i += 3;
    }

    // Handle remaining 1 or 2 bytes with '=' padding.
    let rem = input.len() - i;
    if rem == 2 {
        let d = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out[pos] = BASE64_TABLE[((d >> 18) & 0x3F) as usize];
        out[pos + 1] = BASE64_TABLE[((d >> 12) & 0x3F) as usize];
        out[pos + 2] = BASE64_TABLE[((d >> 6) & 0x3F) as usize];
        out[pos + 3] = b'=';
    } else if rem == 1 {
        let d = (input[i] as u32) << 16;
        out[pos] = BASE64_TABLE[((d >> 18) & 0x3F) as usize];
        out[pos + 1] = BASE64_TABLE[((d >> 12) & 0x3F) as usize];
        out[pos + 2] = b'=';
        out[pos + 3] = b'=';
    }

    out
}

// ── Public API ───────────────────────────────────────────────────────────

/// Compute the SHA-1 hash (RFC 3174) of `data`.
///
/// Returns the 20-byte (160-bit) digest.  Stack-only; processes data in
/// 64-byte blocks with a 128-byte stack buffer for final padding.  No heap
/// allocations.
///
/// # Examples
///
/// ```
/// let digest = komari_agent_rs::crypto::sha1(b"abc");
/// assert_eq!(hex::encode(digest), "a9993e364706816aba3e25717850c26c9cd0d89d");
/// ```
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = SHA1_INIT;

    // Process all complete 64-byte blocks.
    let full_blocks = data.len() / 64;
    for i in 0..full_blocks {
        process_block(&mut h, &data[i * 64..(i + 1) * 64]);
    }

    let remaining = data.len() % 64;
    let bit_len = (data.len() as u64).wrapping_mul(8);

    // Final block(s) with padding: 0x80 || zeros || 64-bit big-endian length.
    // If msg_len % 64 >= 56, the length cannot fit in the current block →
    // we need a second (empty) block.
    if remaining > 55 {
        let mut pad = [0u8; 128];
        let start = full_blocks * 64;
        pad[..remaining].copy_from_slice(&data[start..]);
        pad[remaining] = 0x80;
        // pad[remaining+1 .. 120] stays zero
        pad[120..128].copy_from_slice(&bit_len.to_be_bytes());
        process_block(&mut h, &pad[..64]);
        process_block(&mut h, &pad[64..]);
    } else {
        let mut pad = [0u8; 64];
        let start = full_blocks * 64;
        pad[..remaining].copy_from_slice(&data[start..]);
        pad[remaining] = 0x80;
        // pad[remaining+1 .. 56] stays zero
        pad[56..64].copy_from_slice(&bit_len.to_be_bytes());
        process_block(&mut h, &pad);
    }

    // Emit h0..h4 as 20 big-endian bytes.
    let mut result = [0u8; 20];
    for i in 0..5 {
        let bytes = h[i].to_be_bytes();
        result[i * 4..(i + 1) * 4].copy_from_slice(&bytes);
    }
    result
}

/// Base64-encode a 20-byte SHA-1 digest into a 28-character string
/// (RFC 4648, with `=` padding).
///
/// The caller must ensure `input` is exactly 20 bytes; otherwise the
/// function will panic in debug builds or produce garbled output in
/// release builds.
pub fn base64_encode(input: &[u8]) -> [u8; 28] {
    base64_encode_fixed::<28>(input)
}

/// Compute the WebSocket `Sec-WebSocket-Accept` response key per RFC 6455
/// §4.2.2:
///
/// ```text
/// base64(sha1(client_key || "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"))
/// ```
///
/// Returns 28 ASCII bytes (the accept key string).  Stack-only.
pub fn ws_accept_key(client_key: &str) -> [u8; 28] {
    let key_bytes = client_key.as_bytes();
    let total = key_bytes.len() + WS_GUID.len();

    // Worst case: WebSocket key is 24 chars → 60 bytes total.  Use a
    // 64-byte stack buffer so the SHA-1 fast-path (single block) covers
    // the common case.
    let mut combined = [0u8; 64];
    debug_assert!(
        total <= 64,
        "client key + GUID exceeds 64 bytes; increase buffer size"
    );
    combined[..key_bytes.len()].copy_from_slice(key_bytes);
    combined[key_bytes.len()..total].copy_from_slice(WS_GUID);

    let hash = sha1(&combined[..total]);
    base64_encode(&hash)
}

/// Generate a random 16-byte value and Base64-encode it into a 24-character
/// `Sec-WebSocket-Key` for the opening handshake (RFC 6455 §4.1).
///
/// Uses the OS CSPRNG (`/dev/urandom` on Unix, `BCryptGenRandom` on Windows).
/// This is a cold-path function (called once per connection); it may allocate
/// internally through the OS random device / syscall layer.
pub fn ws_generate_key() -> [u8; 24] {
    let mut random = [0u8; 16];
    fill_random(&mut random);
    base64_encode_fixed::<24>(&random)
}

/// Constant-time-ish comparison of a received `Sec-WebSocket-Accept` header
/// against the expected value.
///
/// `response_key` is the raw header bytes received from the server.
/// `expected` is the output of [`ws_accept_key`].
pub fn ws_verify_accept(response_key: &[u8], expected: &[u8; 28]) -> bool {
    response_key.len() == 28 && {
        // Use a simple bitwise-OR accumulator so the loop is not
        // short-circuited by the first differing byte.
        let mut acc = 0u8;
        for i in 0..28 {
            acc |= response_key[i] ^ expected[i];
        }
        acc == 0
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SHA-1 test vectors (RFC 3174 / NIST CAVP) ──────────────────────

    #[test]
    fn sha1_empty() {
        let digest = sha1(b"");
        let expected = hex!("da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(digest, expected, "SHA-1(\"\") mismatch");
    }

    #[test]
    fn sha1_abc() {
        let digest = sha1(b"abc");
        let expected = hex!("a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(digest, expected, "SHA-1(\"abc\") mismatch");
    }

    #[test]
    fn sha1_448_bits() {
        // 56-byte message: "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        let digest = sha1(msg);
        let expected = hex!("84983e441c3bd26ebaae4aa1f95129e5e54670f1");
        assert_eq!(digest, expected, "SHA-1(448-bit message) mismatch");
    }

    #[test]
    fn sha1_exactly_one_block() {
        // 64 bytes (exactly one 512-bit block + padding block).
        let msg = b"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz0123456789!?";
        assert_eq!(msg.len(), 64);
        let digest = sha1(msg);
        let expected = hex!("404a9d6d7620c581ec8471acf009e95d03206b4c");
        assert_eq!(digest, expected);
    }

    #[test]
    fn sha1_multiple_blocks() {
        // 200 bytes (3 full blocks + 8 remaining).
        let msg = concat!(
            "The quick brown fox jumps over the lazy dog. ",
            "The quick brown fox jumps over the lazy dog. ",
            "The quick brown fox jumps over the lazy dog. ",
            "The quick brown fox jumps over the lazy dog. ",
            "The quick brown fox "
        )
        .as_bytes();
        assert_eq!(msg.len(), 200);
        let digest = sha1(msg);
        let expected = hex!("4fa3da02d9b7509ec08a967ca0581fa7ab00d165");
        assert_eq!(digest, expected);
    }

    #[test]
    fn sha1_55_bytes_boundary() {
        // Exactly 55 bytes: no overflow into second padding block.
        let msg = b"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz012";
        assert_eq!(msg.len(), 55);
        let digest = sha1(msg);
        let expected = hex!("abe8daa7cc75181de8f29ef5670557f1d9f6e9f4");
        assert_eq!(digest, expected);
    }

    #[test]
    fn sha1_56_bytes_boundary() {
        // Exactly 56 bytes: overflow into second padding block.
        let msg = b"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz0123";
        assert_eq!(msg.len(), 56);
        let digest = sha1(msg);
        let expected = hex!("cff2e72f1868f2265680e2d03f8156328546d194");
        assert_eq!(digest, expected);
    }

    // ── Base64 test vectors ─────────────────────────────────────────────

    #[test]
    fn base64_encode_sha1_of_abc() {
        let hash = sha1(b"abc");
        let encoded = base64_encode(&hash);
        assert_eq!(&encoded, b"qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
    }

    #[test]
    fn base64_encode_all_zeros() {
        let hash = [0u8; 20];
        let encoded = base64_encode(&hash);
        assert_eq!(&encoded, b"AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }

    #[test]
    fn base64_encode_all_ones() {
        let hash = [0xFFu8; 20];
        let encoded = base64_encode(&hash);
        // 20 bytes of 0xFF: 6 full triples → 24 '/' bytes,
        // remaining 2 bytes (0xFF, 0xFF) → "//8=".
        // Total: 26 '/' + '8' + '=' = 28 bytes.
        let mut expected = [b'/'; 28];
        expected[26] = b'8';
        expected[27] = b'=';
        assert_eq!(&encoded, &expected);
    }

    // ── WebSocket handshake test vectors ───────────────────────────────

    #[test]
    fn ws_accept_key_known_vector() {
        // From RFC 6455 §4.2.2:
        //   client key:  "dGhlIHNhbXBsZSBub25jZQ=="
        //   accept key:  "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        let accept = ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        let expected = b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        assert_eq!(&accept, expected, "WebSocket accept key mismatch");
    }

    #[test]
    fn ws_verify_accept_correct() {
        let accept = ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        assert!(ws_verify_accept(&accept, &accept));
    }

    #[test]
    fn ws_verify_accept_incorrect() {
        let accept = ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        let mut wrong = accept;
        wrong[0] ^= 1;
        assert!(!ws_verify_accept(&wrong, &accept));
    }

    #[test]
    fn ws_verify_accept_wrong_length() {
        let accept = ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        assert!(!ws_verify_accept(b"too_short", &accept));
    }

    #[test]
    fn ws_generate_key_produces_24_chars() {
        let key = ws_generate_key();
        assert_eq!(key.len(), 24);
        // Must be valid Base64 (only A-Z, a-z, 0-9, +, /, =).
        for &b in &key {
            assert!(
                b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=',
                "invalid Base64 character: {b}"
            );
        }
    }

    #[test]
    fn ws_accept_key_roundtrip() {
        let key_buf = ws_generate_key();
        let client_key = core::str::from_utf8(&key_buf).unwrap();
        let accept = ws_accept_key(client_key);
        assert!(ws_verify_accept(&accept, &accept));
        // Sanity: altering any byte fails verification.
        let mut wrong = accept;
        wrong[13] ^= 0x80;
        assert!(!ws_verify_accept(&wrong, &accept));
    }

    // ── Hex helper (internal to tests) ──────────────────────────────────

    /// Minimal const-hex parser for test vectors.  Not exported.
    macro_rules! hex {
        ($s:literal) => {{
            const fn hex_digit(d: u8) -> u8 {
                match d {
                    b'0'..=b'9' => d - b'0',
                    b'a'..=b'f' => d - b'a' + 10,
                    b'A'..=b'F' => d - b'A' + 10,
                    _ => 0, // compile-time error would be nicer, but const fn is limited
                }
            }
            const BYTES: &[u8] = $s.as_bytes();
            const N: usize = BYTES.len() / 2;
            let mut arr = [0u8; N];
            let mut i = 0;
            while i < N {
                arr[i] = hex_digit(BYTES[i * 2]) << 4 | hex_digit(BYTES[i * 2 + 1]);
                i += 1;
            }
            arr
        }};
    }
    use hex;
}
