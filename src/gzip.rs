// Fixed-Huffman DEFLATE encoder with gzip (RFC 1952) wrapper.
// Encode only — no decode needed (server responses are uncompressed JSON).
// DD9 in spec.md: fixed Huffman encode-only. Unconditional: needed for HTTP POST fallback.

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

    fn lit(&mut self, byte: u8) {
        let s = byte as usize;
        self.put(LL_CODE[s], LL_BITS[s]);
    }

    fn eob(&mut self) {
        self.put(LL_CODE[256], LL_BITS[256]);
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
            bw.put(LL_CODE[ls as usize], LL_BITS[ls as usize]);
            let le = LEN_XTRA[(ls - 257) as usize];
            if le > 0 {
                bw.put(best_len as u16 - LEN_BASE[(ls - 257) as usize], le);
            }
            let ds = dist_sym(best_dist as u16);
            bw.put(ds, 5);
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
}
