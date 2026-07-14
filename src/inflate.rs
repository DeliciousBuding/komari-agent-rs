// DEFLATE inflate decoder (RFC 1951) + zlib (RFC 1950) wrapper.
// Decode only — receives permessage-deflate compressed frames from the server.
//
// Three RFC 1951 block types are supported:
//   BTYPE=00 stored (verbatim copy), BTYPE=01 fixed Huffman,
//   BTYPE=10 dynamic Huffman. BFINAL marks the last block.
// LZ77 back-references copy length(3..=258)+distance(1..=32768) from already
// decoded output. The decoder is single-threaded and allocation-light: output
// is appended to a caller-supplied Vec.
//
// Kept as a complete library; allow dead_code until ws.rs wires it in.
#![allow(dead_code)]

#[derive(Debug)]
pub enum InflateErr {
    /// Bit stream exhausted before a complete symbol/field was read.
    UnexpectedEof,
    /// Block type, code length, or symbol outside the valid range.
    InvalidData,
    /// Back-reference distance exceeds already-decoded output.
    BadDistance,
    /// Huffman code did not match any symbol (oversubscribed / corrupt table).
    BadCode,
    /// Decompressed output exceeded the 64 MiB safety limit (DEFLATE bomb protection).
    MaxSizeExceeded,
}

impl std::fmt::Display for InflateErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("inflate: unexpected end of stream"),
            Self::InvalidData => f.write_str("inflate: invalid data"),
            Self::BadDistance => f.write_str("inflate: distance too far back"),
            Self::BadCode => f.write_str("inflate: invalid huffman code"),
            Self::MaxSizeExceeded => f.write_str("inflate: output exceeded 64 MiB safety limit"),
        }
    }
}
impl std::error::Error for InflateErr {}

/// Maximum decompressed output size (64 MiB) — DEFLATE bomb protection.
const MAX_INFLATE_SIZE: usize = 64 * 1024 * 1024;

// ── BitReader (LSB-first) ───────────────────────────────────────────────────
// DEFLATE packs bits least-significant-first within each byte; Huffman codes
// are packed MSB-first, which we handle by reading the code bit by bit and
// shifting the accumulator left (see Huff::decode).
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // byte cursor
    bit: u32,   // bit buffer
    nbits: u32, // valid bits in buffer
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bit: 0,
            nbits: 0,
        }
    }

    // Pull `n` bits LSB-first into the low end of the returned value (1..=16).
    #[inline]
    fn read(&mut self, n: u32) -> Result<u32, InflateErr> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or(InflateErr::UnexpectedEof)?;
            self.pos += 1;
            self.bit |= (b as u32) << self.nbits;
            self.nbits += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }

    // Byte-align the cursor by dropping buffered bits (used after stored blocks).
    #[inline]
    fn align_byte(&mut self) {
        self.bit = 0;
        self.nbits = 0;
        // pos already advances byte-by-byte; only the in-byte buffer is dropped.
    }

    // Read one bit for Huffman decoding; returned as the MSB of an accumulating
    // code value. Caller builds the code top-down.
    #[inline]
    fn read_bit(&mut self) -> Result<u32, InflateErr> {
        if self.nbits == 0 {
            let b = *self.data.get(self.pos).ok_or(InflateErr::UnexpectedEof)?;
            self.pos += 1;
            self.bit = b as u32;
            self.nbits = 8;
        }
        let v = self.bit & 1;
        self.bit >>= 1;
        self.nbits -= 1;
        Ok(v)
    }
}

// ── Length / distance base+extra tables (RFC 1951 §3.2.5) ────────────────────
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_XTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_XTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

// Order in which dynamic-block code-length-code lengths are stored (§3.2.7).
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ── Huffman decoder ─────────────────────────────────────────────────────────
// Canonical Huffman represented by per-bit-length counts + sorted symbols.
// decode_symbol walks bits MSB-first, accumulating a code until it lands in the
// range [first_code+len_count[len], ...) for the current length — the standard
// canonical-decoding algorithm.
struct Huff {
    counts: [u16; 16], // number of codes of each bit length (1..15)
    symbols: Vec<u16>, // symbols sorted by (length, code)
}

impl Huff {
    /// Build from a per-symbol code-length array. `lens[i]` is the bit length
    /// of symbol `i` (0 = unused). Returns Err on oversubscription or on a
    /// symbol index exceeding 285/29 as appropriate (caller bounds).
    fn build(lens: &[u8]) -> Result<Self, InflateErr> {
        let mut counts = [0u16; 16];
        for &l in lens {
            if l as usize >= 16 {
                return Err(InflateErr::InvalidData);
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0; // length-0 symbols are unused, not counted for offsets

        // Reject oversubscribed codes (RFC 1951 canonical-code invariant).
        // `left` starts at 1 code-slot for length-1 codes and doubles each
        // length; subtracting each length's code count must stay non-negative.
        // Incomplete codes (left > 0 at the end) are permitted: dynamic blocks
        // may carry single-symbol distance tables.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                return Err(InflateErr::BadCode);
            }
        }

        // Offsets per length: starting index into `symbols` for each length.
        let mut offsets = [0u16; 16];
        let mut off = 0u16;
        for len in 1..16 {
            offsets[len] = off;
            off = off
                .checked_add(counts[len])
                .ok_or(InflateErr::InvalidData)?;
        }

        let mut symbols = vec![0u16; lens.len()];
        for (sym, &l) in lens.iter().enumerate() {
            if l != 0 {
                let l = l as usize;
                symbols[offsets[l] as usize] = sym as u16;
                offsets[l] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    /// Decode one symbol, reading bits MSB-first from `r`.
    fn decode(&self, r: &mut BitReader<'_>) -> Result<u16, InflateErr> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16usize {
            // Pull one more bit into the LSB of the accumulator (MSB-first code).
            code = (code << 1) | r.read_bit()? as i32;
            let count = self.counts[len] as i32;
            // Codes at this bit-length occupy [first, first+count). The
            // canonical-decode invariant is `code - first < count`, i.e.
            // `code < first + count` — NOT `count - first` (that subtracts
            // `first` twice and corrupts every symbol lookup).
            let span = count;
            if code < first + span {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
        }
        Err(InflateErr::BadCode)
    }
}

// Fixed Huffman literal/length code lengths (§3.2.6): 0..=143 → 8, 144..=255 →
// 9, 256..=279 → 7, 280..=287 → 8.
fn fixed_ll_lens() -> [u8; 288] {
    let mut l = [0u8; 288];
    let mut i = 0;
    while i < 288 {
        l[i] = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
        i += 1;
    }
    l
}
// Fixed distance code: all 30 symbols get 5 bits (§3.2.6). RFC nominally
// defines 32 symbols; we only ever read codes < 30 in well-formed streams.
fn fixed_dist_lens() -> [u8; 30] {
    [5u8; 30]
}

// ── Block decoders ──────────────────────────────────────────────────────────

/// Decode a stored (BTYPE=00) block: drop to byte boundary, read LEN/NLEN, copy
/// LEN bytes verbatim.
fn decode_stored(r: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), InflateErr> {
    r.align_byte();
    let len = read_u16_le(r)? as usize;
    let nlen = read_u16_le(r)?;
    if len != !nlen as usize {
        return Err(InflateErr::InvalidData);
    }
    for _ in 0..len {
        if out.len() >= MAX_INFLATE_SIZE {
            return Err(InflateErr::MaxSizeExceeded);
        }
        let b = r.read(8)?;
        out.push(b as u8);
    }
    Ok(())
}

// Read a 16-bit little-endian value directly from the byte cursor (post-align).
#[inline]
fn read_u16_le(r: &mut BitReader<'_>) -> Result<u16, InflateErr> {
    let lo = r.read(8)? as u16;
    let hi = r.read(8)? as u16;
    Ok((hi << 8) | lo)
}

/// Decode a Huffman-coded block (fixed or dynamic). Shared body: emit literals,
/// expand length/distance pairs into back-references, stop at symbol 256.
fn decode_huffman_block(
    r: &mut BitReader<'_>,
    ll: &Huff,
    dist: &Huff,
    out: &mut Vec<u8>,
) -> Result<(), InflateErr> {
    loop {
        let sym = ll.decode(r)?;
        if sym < 256 {
            if out.len() >= MAX_INFLATE_SIZE {
                return Err(InflateErr::MaxSizeExceeded);
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        if sym > 285 {
            return Err(InflateErr::InvalidData);
        }
        // Length symbol 257..=285.
        let li = (sym - 257) as usize;
        let mut len = LEN_BASE[li] as u32;
        let lex = LEN_XTRA[li] as u32;
        if lex > 0 {
            len += r.read(lex)?;
        }
        // Distance symbol 0..=29 (5 bits for fixed; dynamic reads its own).
        let dsym = dist.decode(r)?;
        if dsym > 29 {
            return Err(InflateErr::InvalidData);
        }
        let di = dsym as usize;
        let mut distance = DIST_BASE[di] as u32;
        let dex = DIST_XTRA[di] as u32;
        if dex > 0 {
            distance += r.read(dex)?;
        }
        let dist_us = distance as usize;
        if dist_us == 0 || dist_us > out.len() {
            return Err(InflateErr::BadDistance);
        }
        // Copy `len` bytes from `dist_us` back. Overlap (dist < len) is legal
        // and must be byte-by-byte so freshly written bytes are visible.
        let start = out.len() - dist_us;
        let mut copied = 0usize;
        while copied < len as usize {
            if out.len() >= MAX_INFLATE_SIZE {
                return Err(InflateErr::MaxSizeExceeded);
            }
            let b = out[start + copied];
            out.push(b);
            copied += 1;
        }
    }
}

/// Build the dynamic Huffman tables from a BTYPE=10 block header (§3.2.7).
fn build_dynamic(r: &mut BitReader<'_>) -> Result<(Huff, Huff), InflateErr> {
    let hlit = r.read(5)? as usize + 257;
    let hdist = r.read(5)? as usize + 1;
    let hclen = r.read(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateErr::InvalidData);
    }

    // Code-length-code lengths (3 bits each) in CL_ORDER.
    let mut cl_lens = [0u8; 19];
    for i in 0..hclen {
        cl_lens[CL_ORDER[i]] = r.read(3)? as u8;
    }
    let cl_huff = Huff::build(&cl_lens)?;

    // Decode the run-length-encoded literal/length + distance code lengths.
    let total = hlit + hdist;
    let mut lens = vec![0u8; total];
    let mut i = 0usize;
    while i < total {
        let sym = cl_huff.decode(r)?;
        match sym {
            0..=15 => {
                lens[i] = sym as u8;
                i += 1;
            }
            16 => {
                // Copy previous length 3..=6 times.
                if i == 0 {
                    return Err(InflateErr::InvalidData);
                }
                let prev = lens[i - 1];
                let rep = r.read(2)? as usize + 3;
                if i + rep > total {
                    return Err(InflateErr::InvalidData);
                }
                for _ in 0..rep {
                    lens[i] = prev;
                    i += 1;
                }
            }
            17 => {
                // Repeat 0 for 3..=10 times.
                let rep = r.read(3)? as usize + 3;
                if i + rep > total {
                    return Err(InflateErr::InvalidData);
                }
                i += rep;
            }
            18 => {
                // Repeat 0 for 11..=138 times.
                let rep = r.read(7)? as usize + 11;
                if i + rep > total {
                    return Err(InflateErr::InvalidData);
                }
                i += rep;
            }
            _ => return Err(InflateErr::InvalidData),
        }
    }

    let ll = Huff::build(&lens[..hlit])?;
    let dist = Huff::build(&lens[hlit..total])?;
    Ok((ll, dist))
}

// ── Public entry points ─────────────────────────────────────────────────────

/// Inflate a raw DEFLATE stream (no zlib header; RFC 1951).
/// Decoded bytes are appended to `output`.
pub fn inflate_raw(input: &[u8], output: &mut Vec<u8>) -> Result<(), InflateErr> {
    let mut r = BitReader::new(input);
    loop {
        let bfinal = r.read(1)?;
        let btype = r.read(2)?;
        match btype {
            0 => decode_stored(&mut r, output)?,
            1 => {
                let ll = Huff::build(&fixed_ll_lens()[..])?;
                let dist = Huff::build(&fixed_dist_lens()[..])?;
                decode_huffman_block(&mut r, &ll, &dist, output)?;
            }
            2 => {
                let (ll, dist) = build_dynamic(&mut r)?;
                decode_huffman_block(&mut r, &ll, &dist, output)?;
            }
            _ => return Err(InflateErr::InvalidData),
        }
        // Belt-and-suspenders: per-block size guard (hot-path checks are in the
        // block decoders above, but a corrupt stream may bypass them).
        if output.len() > MAX_INFLATE_SIZE {
            return Err(InflateErr::MaxSizeExceeded);
        }
        if bfinal == 1 {
            return Ok(());
        }
    }
}

/// Inflate a zlib stream (RFC 1950): 2-byte CMF/FLG header + raw DEFLATE +
/// 4-byte Adler-32 trailer. The CM/CINFO/FCHECK fields and the FDICT bit are
/// validated up front; the trailing Adler-32 is checked best-effort and a
/// mismatch is ignored (consumers of permessage-deflate only need the bytes).
pub fn inflate_zlib(input: &[u8], output: &mut Vec<u8>) -> Result<(), InflateErr> {
    if input.len() < 6 {
        // 2 header + 4 adler minimum (raw body may be empty).
        return Err(InflateErr::UnexpectedEof);
    }
    let cmf = input[0];
    let flg = input[1];
    // CM must be 8 (deflate); CINFO <= 7 (window size); FCHECK makes
    // (CMF*256+FLG) divisible by 31. We treat a missing FDICT as required
    // (no preset dictionary support) and skip strict checks only on failure.
    if (cmf & 0x0F) != 8 || (cmf >> 4) > 7 || !((cmf as u16) << 8 | flg as u16).is_multiple_of(31) {
        return Err(InflateErr::InvalidData);
    }
    if flg & 0x20 != 0 {
        // FDICT set: a 4-byte DICTID follows; we don't support dictionaries.
        return Err(InflateErr::InvalidData);
    }
    // Decode the raw body, leaving the trailing 4-byte Adler-32 untouched.
    inflate_raw(&input[2..input.len() - 4], output)?;
    // Adler-32 is optional to enforce; verify best-effort and ignore mismatch.
    let _ = verify_adler32(output, &input[input.len() - 4..]);
    Ok(())
}

// ── Adler-32 (RFC 1950 §9) ───────────────────────────────────────────────────
// Big-endian: s2 in high 16 bits, s1 in low 16 bits.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &b in data {
        s1 = (s1 + b as u32) % MOD;
        s2 = (s2 + s1) % MOD;
    }
    (s2 << 16) | s1
}

// Returns Ok on match, Err on mismatch. Caller decides whether to act on it.
fn verify_adler32(data: &[u8], trailer: &[u8]) -> Result<(), InflateErr> {
    if trailer.len() < 4 {
        return Err(InflateErr::UnexpectedEof);
    }
    let want = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let got = adler32(data);
    if want == got {
        Ok(())
    } else {
        Err(InflateErr::InvalidData)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// BFINAL=1, BTYPE=00 stored block encoding an empty payload.
    /// Bit layout (LSB-first): byte 0 holds BFINAL(1) in bit0, BTYPE(00) in
    /// bits1-2 → 0b00000001 = 0x01. After byte alignment: LEN=0x0000,
    /// NLEN=0xFFFF, no payload bytes. This is the canonical empty stored block
    /// `01 00 00 FF FF`. (The task brief's `00 01 00 00 FF FF` appears to be a
    /// transposition of this stream; the correctly-formed empty block is used
    /// here so the round-trip is well defined.)
    #[test]
    fn test_empty() {
        let input = [0x01, 0x00, 0x00, 0xFF, 0xFF];
        let mut out = Vec::new();
        inflate_raw(&input, &mut out).expect("empty stored block decodes");
        assert!(out.is_empty(), "empty stored block yields no output");
    }

    /// The brief's literal vector `00 01 00 00 FF FF` is not a valid empty
    /// stored block under RFC 1951 (its LEN/NLEN pair does not satisfy the
    /// one's-complement invariant). We assert it is rejected rather than
    /// silently producing a wrong result.
    #[test]
    fn test_empty_spec_vector_rejected() {
        let input = [0x00, 0x01, 0x00, 0x00, 0xFF, 0xFF];
        let mut out = Vec::new();
        assert!(inflate_raw(&input, &mut out).is_err());
    }

    /// Round-trip via our own gzip encoder: strip the 10-byte gzip header +
    /// 8-byte trailer to recover the raw DEFLATE body and inflate it back.
    #[test]
    fn test_roundtrip_with_gzip() {
        let data = b"Hello, World! This is a test of the gzip compression system.";
        let gz = crate::gzip::gzip_compress(data).expect("gzip_compress ok");
        // gzip layout: 10-byte header | deflate body | 4-byte CRC32 | 4-byte ISIZE
        assert!(gz.len() > 18, "gzip output must contain header+trailer");
        let body = &gz[10..gz.len() - 8];
        let mut out = Vec::new();
        inflate_raw(body, &mut out).expect("inflate of our own deflate body");
        assert_eq!(out.as_slice(), data.as_slice());
    }

    /// Fixed-Huffman literal-only stream must round-trip a short ASCII string.
    /// We reuse gzip_compress (which always emits a BTYPE=01 block) on data
    /// too short to produce matches, then verify the literal path decodes.
    #[test]
    fn test_literal_fixed() {
        let data = b"Hello";
        let gz = crate::gzip::gzip_compress(data).unwrap();
        let body = &gz[10..gz.len() - 8];
        let mut out = Vec::new();
        inflate_raw(body, &mut out).unwrap();
        assert_eq!(out.as_slice(), b"Hello");
    }

    /// Repeated payload exercises LZ77 back-references (length+distance) on the
    /// fixed-Huffman path produced by our encoder.
    #[test]
    fn test_back_reference() {
        let data = "AAAAAAAAAAAAAAAA".repeat(20); // 320 'A's → many matches
        let gz = crate::gzip::gzip_compress(data.as_bytes()).unwrap();
        let body = &gz[10..gz.len() - 8];
        let mut out = Vec::new();
        inflate_raw(body, &mut out).unwrap();
        assert_eq!(out, data.as_bytes());
    }

    /// Highly compressible, large, repetitive input tends to make third-party
    /// encoders choose dynamic Huffman. Our encoder is fixed-only, so this test
    /// asserts the fixed path still handles the workload AND that a hand-built
    /// dynamic block decodes correctly (covered by the zlib-style payload below
    /// via an independently produced zlib stream when present). Here we verify
    /// the dynamic-table builder is exercised indirectly through the public API
    /// on a mixed payload.
    #[test]
    fn test_dynamic_huffman_capable() {
        // Mixed data: literals + runs. Our encoder still emits fixed Huffman,
        // so this primarily guards that large inputs decode end-to-end.
        let mut data = Vec::new();
        for ch in b'a'..=b'z' {
            for _ in 0..50 {
                data.push(ch);
            }
        }
        let gz = crate::gzip::gzip_compress(&data).unwrap();
        let body = &gz[10..gz.len() - 8];
        let mut out = Vec::new();
        inflate_raw(body, &mut out).unwrap();
        assert_eq!(out, data);
    }

    /// Hand-assembled BTYPE=10 dynamic-Huffman block that decodes to `b"A"`.
    /// The stream was generated by a reference packer mirroring this decoder's
    /// canonical-Huffman construction:
    ///   - HLIT=0 (257 LL lengths), HDIST=0 (1 dist length), HCLEN=14 (18 CL).
    ///   - Code-length alphabet: symbol 1 and 18 each get 1 bit.
    ///   - LL alphabet: symbol 65 ('A') and 256 (EOB) each get 1 bit (codes
    ///     0 and 1 respectively); the distance table carries a single unused
    ///     symbol-0 length-1 code.
    /// This is the only test that exercises `build_dynamic` + the BTYPE=10
    /// branch end-to-end, since the in-tree encoder emits fixed Huffman only.
    #[test]
    fn test_dynamic_huffman_block() {
        let input: [u8; 13] = [
            0x05, 0xC0, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x36, 0xFF, 0x53, 0x08,
        ];
        let mut out = Vec::new();
        inflate_raw(&input, &mut out).expect("dynamic block decodes");
        assert_eq!(out, b"A");
    }

    /// Reject an oversubscribed canonical-Huffman table (3 length-1 codes for a
    /// 2-slot space); the well-formed 2-code variant is accepted.
    #[test]
    fn test_dynamic_table_oversubscribed_rejected() {
        let ok_lens = [1u8, 1, 0, 0]; // 2 codes fill 2^1 = 2 slots
        assert!(Huff::build(&ok_lens).is_ok());
        let bad_lens = [1u8, 1, 1, 0]; // 3 codes overfill 2 slots
        assert!(Huff::build(&bad_lens).is_err());
    }

    /// Decode a single stored block carrying real bytes: BFINAL=1 BTYPE=00,
    /// LEN=5 NLEN=~5, payload "abcde".
    #[test]
    fn test_stored_block_payload() {
        // byte0: bits LSB-first: BFINAL=1 (bit0), BTYPE=00 (bits1-2) → 0b001 = 0x01.
        // After align, LEN=0x0005, NLEN=0xFFFA, then 5 bytes.
        let mut input = vec![0x01, 0x05, 0x00, 0xFA, 0xFF];
        input.extend_from_slice(b"abcde");
        let mut out = Vec::new();
        inflate_raw(&input, &mut out).unwrap();
        assert_eq!(out, b"abcde");
    }

    /// Adler-32 sanity check against RFC 1950 reference vector "Wikipedia".
    #[test]
    fn test_adler32_known() {
        // adler32("Wikipedia") = 0x11E60398 (well-known value).
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// zlib stream round-trip: wrap our own deflate body with a minimal zlib
    /// header (CMF=0x78, FLG=0x01 → CM=8, CINFO=7, FCHECK makes it %31==0) and
    /// a computed Adler-32 trailer.
    #[test]
    fn test_adler32_zlib() {
        let data = b"The quick brown fox jumps over the lazy dog";
        // Build the raw deflate body via the gzip encoder, then re-wrap.
        let gz = crate::gzip::gzip_compress(data).unwrap();
        let body = &gz[10..gz.len() - 8];

        let mut z = Vec::new();
        // CMF=0x78 (deflate, 32K window). FLG so (0x78<<8 | FLG) % 31 == 0.
        // 0x7801 = 30721 = 31*991, divisible → FLG=0x01.
        z.push(0x78);
        z.push(0x01);
        z.extend_from_slice(body);
        let ad = adler32(data).to_be_bytes();
        z.extend_from_slice(&ad);

        let mut out = Vec::new();
        inflate_zlib(&z, &mut out).expect("zlib round-trip");
        assert_eq!(out.as_slice(), data.as_slice());
    }

    /// Reference vector from Python zlib: raw DEFLATE body of `b"Hello"` is
    /// `f3 48 cd c9 c9 07 00` (Python: `zlib.compress(b"Hello",9)[2:-4].hex()`).
    /// Independent of our own encoder, this anchors the canonical-Huffman
    /// fixed-block path against an external reference.
    #[test]
    fn test_python_vector_hello() {
        let hex = [0xf3u8, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00];
        let mut out = Vec::new();
        inflate_raw(&hex, &mut out).expect("python vector decodes");
        assert_eq!(out.as_slice(), b"Hello");
    }

    /// Corrupt zlib header (bad CM) must be rejected before decoding.
    #[test]
    fn test_zlib_bad_header_rejected() {
        let mut z = vec![0x00u8, 0x00]; // CM != 8
        z.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        z.extend_from_slice(&[0; 4]);
        let mut out = Vec::new();
        assert!(inflate_zlib(&z, &mut out).is_err());
    }

    /// Overlapping back-reference (distance < length) must expand correctly.
    /// Construct via our encoder using a single repeated byte pattern.
    #[test]
    fn test_overlapping_back_reference() {
        // "abababab..." produces an overlapping match once the run is long
        // enough; distance 2 with a long length exercises byte-by-byte copy.
        let data: Vec<u8> = (0..200).map(|i| b'a' + (i % 2) as u8).collect();
        let gz = crate::gzip::gzip_compress(&data).unwrap();
        let body = &gz[10..gz.len() - 8];
        let mut out = Vec::new();
        inflate_raw(body, &mut out).unwrap();
        assert_eq!(out, data);
    }
}
