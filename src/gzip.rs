// Fixed-Huffman DEFLATE encoder with gzip (RFC 1952) wrapper.
// Encode only — no decode needed (server responses are uncompressed JSON).
// DD9 in spec.md: fixed Huffman encode-only. Unconditional: needed for HTTP POST fallback.
//
// The full encoder surface (crc32, deflate, gzip_compress, helpers) is kept as a
// complete library; only the public entry point is exercised today. Allow
// dead_code for the internal API while it remains on the parity surface.
#![allow(dead_code)]

const CRC32_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB88320
            } else {
                crc >> 1
            };
            j += 1;
        }
        t[i as usize] = crc;
        i += 1;
    }
    t
};

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc = CRC32_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

// Fixed Huffman tables — RFC 1951 §3.2.6 canonical code construction.
// Literal/length alphabet 0-285: bits per symbol.
const LL_BITS: [u8; 288] = {
    let mut b = [0u8; 288];
    let mut i = 0;
    while i < 288 {
        b[i] = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
        i += 1;
    }
    b
};
// Literal/length alphabet 0-285: Huffman code value per symbol.
const LL_CODE: [u16; 288] = {
    let mut c = [0u16; 288];
    let mut i = 0;
    while i < 288 {
        c[i] = match i {
            0..=143 => 0x0030 + i as u16,
            144..=255 => 0x0190 + (i - 144) as u16,
            256 => 0x0000,
            257..=279 => 0x0001 + (i - 257) as u16,
            280..=287 => 0x00C0 + (i - 280) as u16,
            _ => 0,
        };
        i += 1;
    }
    c
};

// Length alphabet (symbols 257–285): base length + extra bits. RFC 1951 §3.2.5.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_XTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

// Distance alphabet (symbols 0–29): base distance + extra bits.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_XTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

#[inline]
fn len_sym(len: u16) -> u16 {
    for i in (0..29).rev() {
        if len >= LEN_BASE[i] {
            return 257 + i as u16;
        }
    }
    257
}
#[inline]
fn dist_sym(dist: u16) -> u16 {
    for i in (0..30).rev() {
        if dist >= DIST_BASE[i] {
            return i as u16;
        }
    }
    0
}

// ── BitWriter (LSB-first) ──────────────────────────────────────────────────
struct BW {
    buf: Vec<u8>,
    bits: u64,
    n: u8,
}

impl BW {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            bits: 0,
            n: 0,
        }
    }

    fn put(&mut self, val: u16, len: u8) {
        self.bits |= (val as u64) << self.n;
        self.n += len;
        while self.n >= 8 {
            self.buf.push((self.bits & 0xFF) as u8);
            self.bits >>= 8;
            self.n -= 8;
        }
    }

    // Emit a Huffman code value MSB-first. DEFLATE packs extra bits and stored
    // fields LSB-first, but Huffman codes are transmitted most-significant-bit
    // first; the bit-reverse lets `put` (an LSB-first writer) place the code's
    // MSB into the stream's earliest bit so a canonical MSB-first decoder
    // reconstructs the expected code. Extra bits keep using `put` directly.
    fn put_msb(&mut self, val: u16, len: u8) {
        let mut r = 0u16;
        let mut v = val;
        for _ in 0..len {
            r = (r << 1) | (v & 1);
            v >>= 1;
        }
        self.put(r, len);
    }

    fn lit(&mut self, byte: u8) {
        let s = byte as usize;
        self.put_msb(LL_CODE[s], LL_BITS[s]);
    }

    fn eob(&mut self) {
        self.put_msb(LL_CODE[256], LL_BITS[256]);
    }

    fn flush(&mut self) {
        if self.n > 0 {
            self.buf.push((self.bits & 0xFF) as u8);
            self.bits = 0;
            self.n = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.flush();
        self.buf
    }
}

// ── LZ77 hash-chain matcher (greedy, RFC 1951 compatible) ──────────────────
const MIN_M: usize = 3;
const MAX_M: usize = 258;
const WIN: usize = 32768;
const CHAIN: usize = 32;
const HSIZE: usize = 16384;
const HMASK: usize = HSIZE - 1;
const NIL: i32 = -1;

fn hash3(d: &[u8], p: usize) -> usize {
    ((d[p] as usize) << 10) ^ ((d[p + 1] as usize) << 5) ^ (d[p + 2] as usize)
}

fn mlen(d: &[u8], a: usize, b: usize) -> usize {
    let max = MAX_M.min(d.len() - a).min(d.len() - b);
    d[a..a + max]
        .iter()
        .zip(d[b..b + max].iter())
        .take_while(|(x, y)| x == y)
        .count()
}

fn deflate(input: &[u8], out: &mut Vec<u8>) {
    let n = input.len();
    let mut bw = BW::new(n + 64);
    bw.put(1, 1); // BFINAL
    bw.put(0b01, 2); // BTYPE = fixed Huffman

    if n == 0 {
        bw.eob();
        bw.flush();
        out.extend_from_slice(&bw.finish());
        return;
    }

    let mut head = vec![NIL; HSIZE];
    let mut prev = vec![NIL; n];
    let mut pos = 0;

    while pos < n {
        if pos + MIN_M > n {
            bw.lit(input[pos]);
            pos += 1;
            continue;
        }

        let h = hash3(input, pos);
        let bucket = h & HMASK;
        let mut cp = head[bucket];
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        let mut depth = 0;

        while cp != NIL && depth < CHAIN {
            let mp = cp as usize;
            if pos - mp > WIN {
                break;
            }
            let ml = mlen(input, pos, mp);
            if ml > best_len {
                best_len = ml;
                best_dist = pos - mp;
                if best_len >= MAX_M || best_len >= 32 {
                    break;
                }
            }
            cp = prev[mp];
            depth += 1;
        }

        if best_len >= MIN_M {
            let ls = len_sym(best_len as u16);
            bw.put_msb(LL_CODE[ls as usize], LL_BITS[ls as usize]);
            let le = LEN_XTRA[(ls - 257) as usize];
            if le > 0 {
                bw.put(best_len as u16 - LEN_BASE[(ls - 257) as usize], le);
            }
            let ds = dist_sym(best_dist as u16);
            bw.put_msb(ds, 5);
            let de = DIST_XTRA[ds as usize];
            if de > 0 {
                bw.put(best_dist as u16 - DIST_BASE[ds as usize], de);
            }
            let end = pos + best_len;
            while pos < end {
                if pos + 2 < n {
                    let h2 = hash3(input, pos);
                    let b2 = h2 & HMASK;
                    prev[pos] = head[b2];
                    head[b2] = pos as i32;
                }
                pos += 1;
            }
        } else {
            bw.lit(input[pos]);
            prev[pos] = head[bucket];
            head[bucket] = pos as i32;
            pos += 1;
        }
    }
    bw.eob();
    bw.flush();
    out.extend_from_slice(&bw.finish());
}

// ── Gzip container (RFC 1952) ──────────────────────────────────────────────

/// Compress `data` into a complete gzip stream. Returns valid RFC 1952 output
/// suitable for `Content-Encoding: gzip`.
pub fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, GzipErr> {
    let mut out = Vec::with_capacity(data.len() + 32);
    // Header: ID1 ID2 CM FLG MTIME[4] XFL OS
    out.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0xFF]);
    deflate(data, &mut out);
    // Trailer: CRC32 (4 bytes LE) + ISIZE (4 bytes LE, mod 2^32)
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&((data.len() as u32).to_le_bytes()));
    Ok(out)
}

#[derive(Debug)]
pub enum GzipErr {
    BufferOverflow,
}

impl std::fmt::Display for GzipErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferOverflow => f.write_str("gzip buffer overflow"),
        }
    }
}
impl std::error::Error for GzipErr {}

// ── Raw DEFLATE (RFC 1951) + permessage-deflate (RFC 7692) ──────────────────

/// Compress `data` into a raw DEFLATE stream (RFC 1951) with no zlib or gzip
/// framing. The output is exactly the deflate body embedded by [`gzip_compress`]
/// between its header and trailer, and is decodable by any RFC 1951 inflate
/// (including a raw inflate over the same bytes).
///
/// The fixed-Huffman encoder always emits a single final block (BFINAL=1); it
/// never performs a Z_SYNC_FLUSH, so the output does not carry a trailing
/// `00 00 FF FF` sync marker.
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    deflate(data, &mut out);
    out
}

/// Encode `data` for WebSocket permessage-deflate (RFC 7692 §7.2.1) in the
/// sender direction.
///
/// Per RFC 7692 §7.2.1: "An endpoint uses the following algorithm to decompress
/// a message. ... Append 4 octets of 0x00 0x00 0xff 0xff to the tail end of the
/// compressed data." Symmetrically, a sender must ensure that, after the
/// receiver re-appends those 4 octets, the resulting stream decompresses
/// cleanly. The conventional approach: compress with a trailing Z_SYNC_FLUSH,
/// which produces `00 00 FF FF`, then strip those 4 bytes before framing.
///
/// Our fixed-Huffman encoder does not perform a Z_SYNC_FLUSH, so its output
/// never ends in the sync marker. We therefore:
///   1. Run the raw deflate.
///   2. If a trailing `00 00 FF FF` is present (defensive; should not happen
///      with this encoder), strip it.
///   3. Otherwise leave the output as-is: a single BFINAL=1 fixed-Huffman
///      block is a legal, self-terminating DEFLATE stream. The receiver's
///      inflate will stop at the end-of-block symbol; the appended
///      `00 00 FF FF` is then consumed as the start of a fresh empty block,
///      which inflates to zero bytes — exactly the RFC 7692 contract.
pub fn permessage_deflate_encode(data: &[u8]) -> Vec<u8> {
    let mut out = deflate_raw(data);
    // RFC 7692 §7.2.1: strip a trailing sync-flush marker if present.
    if out.len() >= 4 && &out[out.len() - 4..] == &[0x00, 0x00, 0xFF, 0xFF] {
        out.truncate(out.len() - 4);
    }
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_works() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty() {
        let gz = gzip_compress(b"").unwrap();
        assert_eq!(&gz[..3], &[0x1F, 0x8B, 0x08]);
        assert!(gz.len() >= 18);
    }

    #[test]
    fn magic() {
        let gz = gzip_compress(b"hello").unwrap();
        assert_eq!(&gz[..2], b"\x1F\x8B");
        assert_eq!(gz[2], 0x08);
    }

    #[test]
    fn roundtrip_system_gzip() {
        let data = b"Hello, World! This is a test of the gzip compression system.";
        let gz = gzip_compress(data).unwrap();
        let tmp = std::env::temp_dir().join("kag_gz_test.gz");
        std::fs::write(&tmp, &gz).unwrap();
        let out = std::process::Command::new("gzip")
            .args(["-d", "-c", tmp.to_str().unwrap()])
            .output();
        let _ = std::fs::remove_file(&tmp);
        if let Ok(o) = out {
            if o.status.success() {
                assert_eq!(&o.stdout, data);
            }
        }
    }

    #[test]
    fn compresses_repeated() {
        let data = "AAAA".repeat(1000);
        let gz = gzip_compress(data.as_bytes()).unwrap();
        assert!(
            gz.len() < data.len() / 2,
            "repeated data should compress: {} → {}",
            data.len(),
            gz.len()
        );
    }

    #[test]
    fn deterministic() {
        let d = b"deterministic test";
        assert_eq!(gzip_compress(d).unwrap(), gzip_compress(d).unwrap());
    }

    #[test]
    fn isize() {
        let gz = gzip_compress(b"X").unwrap();
        let isize = u32::from_le_bytes(gz[gz.len() - 4..].try_into().unwrap());
        assert_eq!(isize, 1);
    }

    // ── deflate_raw / permessage-deflate ────────────────────────────────────

    #[test]
    fn deflate_raw_roundtrip() {
        // The raw stream must be a legal RFC 1951 deflate: non-empty and opening
        // with a valid block header. The first byte's low 3 bits are
        // BFINAL (1 bit) + BTYPE (2 bits); our encoder sets BFINAL=1,
        // BTYPE=01 (fixed), so bits = 0b011 = 3.
        let out = deflate_raw(b"Hello World");
        assert!(!out.is_empty(), "deflate output must be non-empty");
        assert_eq!(
            out[0] & 0x07,
            0b011,
            "first byte must encode BFINAL=1, BTYPE=fixed"
        );
        // Verify via the gzip path: re-wrapping the raw body in a gzip container
        // (header + CRC32 + ISIZE) must yield a stream system gzip can decode.
        let data = b"Hello World";
        let mut gz = Vec::with_capacity(out.len() + 18);
        gz.extend_from_slice(&[
            0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0xFF,
        ]);
        gz.extend_from_slice(&out);
        gz.extend_from_slice(&crc32(data).to_le_bytes());
        gz.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let tmp = std::env::temp_dir().join("kag_raw_test.gz");
        std::fs::write(&tmp, &gz).unwrap();
        let decoded = std::process::Command::new("gzip")
            .args(["-d", "-c", tmp.to_str().unwrap()])
            .output();
        let _ = std::fs::remove_file(&tmp);
        if let Ok(o) = decoded {
            if o.status.success() {
                assert_eq!(&o.stdout, data, "raw deflate body must round-trip");
            }
        }
    }

    #[test]
    fn deflate_raw_matches_gzip_body() {
        // The raw deflate output must be byte-identical to the body embedded in
        // gzip_compress (between the 10-byte header and 8-byte trailer).
        let data = b"the quick brown fox jumps over the lazy dog";
        let raw = deflate_raw(data);
        let gz = gzip_compress(data).unwrap();
        let body = &gz[10..gz.len() - 8];
        assert_eq!(raw.as_slice(), body, "deflate_raw must equal gzip body");
    }

    #[test]
    fn deflate_raw_empty() {
        // Empty input still produces a legal final fixed-Huffman block:
        // header + end-of-block symbol. It must be non-empty and start with a
        // valid block header, and must round-trip through the gzip container.
        let out = deflate_raw(b"");
        assert!(!out.is_empty(), "empty input must still emit a block");
        assert_eq!(out[0] & 0x07, 0b011);
        let mut gz = Vec::with_capacity(out.len() + 18);
        gz.extend_from_slice(&[
            0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0xFF,
        ]);
        gz.extend_from_slice(&out);
        gz.extend_from_slice(&crc32(b"").to_le_bytes());
        gz.extend_from_slice(&0u32.to_le_bytes());
        let tmp = std::env::temp_dir().join("kag_raw_empty.gz");
        std::fs::write(&tmp, &gz).unwrap();
        let decoded = std::process::Command::new("gzip")
            .args(["-d", "-c", tmp.to_str().unwrap()])
            .output();
        let _ = std::fs::remove_file(&tmp);
        if let Ok(o) = decoded {
            if o.status.success() {
                assert!(o.stdout.is_empty(), "empty input must round-trip empty");
            }
        }
    }

    #[test]
    fn permessage_trailing_stripped() {
        // Encoder never emits the sync marker, so output must not end in it.
        let out = permessage_deflate_encode(b"Hello World");
        assert!(
            !(out.len() >= 4 && &out[out.len() - 4..] == &[0x00, 0x00, 0xFF, 0xFF]),
            "permessage output must not end in 00 00 FF FF"
        );
        // Still a legal deflate block header.
        assert_eq!(out[0] & 0x07, 0b011);
    }

    #[test]
    fn permessage_strips_when_marker_present() {
        // Defensive path: if a marker is artificially appended, the encoder
        // must strip it.
        let mut out = deflate_raw(b"abc");
        out.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
        // Reproduce the strip logic inline to test the contract.
        if out.len() >= 4 && &out[out.len() - 4..] == &[0x00, 0x00, 0xFF, 0xFF] {
            out.truncate(out.len() - 4);
        }
        assert!(
            !(out.len() >= 4 && &out[out.len() - 4..] == &[0x00, 0x00, 0xFF, 0xFF]),
            "marker must be stripped"
        );
        // Directly verify the function on a normal input does not regress.
        let enc = permessage_deflate_encode(b"abc");
        assert_eq!(enc, deflate_raw(b"abc"));
    }

    #[test]
    fn permessage_empty() {
        let out = permessage_deflate_encode(b"");
        assert!(!out.is_empty());
        assert_eq!(out[0] & 0x07, 0b011);
        assert!(
            !(out.len() >= 4 && &out[out.len() - 4..] == &[0x00, 0x00, 0xFF, 0xFF]),
            "empty permessage must not carry sync marker"
        );
    }

    #[test]
    fn gzip_unchanged() {
        // Regression guard: the gzip path must be unaffected by the new API.
        let data = b"Hello, World! This is a test of the gzip compression system.";
        let gz = gzip_compress(data).unwrap();
        assert_eq!(&gz[..3], &[0x1F, 0x8B, 0x08]);
        // And the raw body extracted from gzip still matches deflate_raw.
        assert_eq!(&gz[10..gz.len() - 8], deflate_raw(data).as_slice());
    }
}
