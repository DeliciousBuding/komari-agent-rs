# Gzip Compression Strategy for komari-agent-rs

**Date**: 2026-06-20
**Status**: Design proposal (pending implementation)

---

## 1. Go Agent Gzip Usage Analysis

### 1.1 Call Sites

The Go agent uses gzip in exactly **two** places, both for HTTP POST body compression:

| Site | File | Function | Data |
|------|------|----------|------|
| A | `server/websocket.go:256-263` | `postV2RequestContext()` | JSON-RPC v2 request body (reports, pulls) |
| B | `server/task.go:370-377` | `postV2RPC()` | Ping task result JSON |

Both sites follow the identical pattern:

```go
compressed := false
if !flags.DisableCompression {
    if gz, err := gzipBytes(body); err == nil {
        body = gz
        compressed = true
    }
}
// ...
if compressed {
    req.Header.Set("Content-Encoding", "gzip")
}
```

### 1.2 The gzipBytes Function

Defined **twice** (duplication noted in module inventory), identical logic:

```go
func gzipBytes(data []byte) ([]byte, error) {
    var buf bytes.Buffer
    zw := gzip.NewWriter(&buf)   // default compression level (-1 → 6)
    zw.Write(data)
    zw.Close()
    return buf.Bytes(), nil
}
```

Key properties of Go's default gzip:
- Compression level 6 (balanced speed/ratio)
- Dynamic Huffman coding (not fixed)
- Full LZ77 with 32KB sliding window
- Standard gzip header + CRC32 + ISIZE trailer

### 1.3 What Gets Compressed

| Payload Type | Typical Size (uncompressed) | Content Characteristics |
|---|---|---|
| `agent.report` | 2-15 KB | JSON: repeated key strings, numeric metrics, timestamps, boolean flags |
| `agent.pull` | 200-500 B | JSON: capabilities array, ack_event_ids |
| `agent.pingResult` | 200-400 B | JSON: task_id, ping_type, value, timestamp |
| `agent.taskResult` | variable | JSON: task_id, result (text), exit_code, timestamp |

**Conclusion**: The dominant use case is monitoring reports (2-15KB of structured JSON). Small payloads (<500B) may not benefit meaningfully from compression.

### 1.4 WebSocket Compression (Separate Concern)

The Go dialer also sets `EnableCompression: !flags.DisableCompression`, which enables WebSocket per-message deflate (RFC 7692). This is a **separate mechanism** handled at the WebSocket library level and is out of scope for this design.

### 1.5 Control Flow

```
flags.DisableCompression  ────true────>  no compression whatsoever
         │
        false (default)
         │
    ┌────┴────┐
    │         │
WebSocket    HTTP POST
per-message  application-level
deflate      gzip
(tungstenite (our custom encoder)
 extension)
```

The Rust agent should preserve this: one boolean flag controls both mechanisms, maintaining wire-protocol compatibility with the Go agent.

---

## 2. Fixed-Huffman DEFLATE Encoder Design

### 2.1 Why Fixed Huffman (not Dynamic)

| Factor | Fixed Huffman | Dynamic Huffman |
|--------|--------------|-----------------|
| Code size | ~350 lines | ~600+ lines |
| Compression ratio | ~5-15% worse than dynamic | Optimal for given data |
| Encoding speed | Faster (skip tree building) | Slower (build Huffman tree) |
| Correctness risk | Low (codes are in the spec) | Higher (tree serialization bugs) |
| Go interop | Wire-compatible (both are valid DEFLATE) | Same |

**Decision**: Fixed Huffman is the right choice for an agent that prioritizes binary size and simplicity. The compression ratio penalty is acceptable for monitoring JSON (the repeated structure ensures LZ77 matches dominate the savings).

### 2.2 DEFLATE Fixed Huffman Code Tables (RFC 1951 §3.2.6)

#### Literal/Length Alphabet (symbols 0-285)

| Symbol Range | Meaning | Code Bits | Code Range (binary) |
|---|---|---|---|
| 0-143 | Literal byte 0x00-0x8F | 8 | `00110000` - `10111111` |
| 144-255 | Literal byte 0x90-0xFF | 9 | `110010000` - `111111111` |
| 256 | End-of-block | 7 | `0000000` |
| 257-264 | Length 3-10 (extra bits: 0) | 7 | `0000001` - `0001000` |
| 265-268 | Length 11-18 (extra bits: 1) | 7 + 1 | `0001001` - `0001100` |
| 269-272 | Length 19-34 (extra bits: 2) | 7 + 2 | `0001101` - `0010000` |
| 273-276 | Length 35-66 (extra bits: 3) | 7 + 3 | `0010001` - `0010100` |
| 277-280 | Length 67-130 (extra bits: 4) | 7 + 4 | `0010101` - `0011000` |
| 280 | Length 131-258 (extra 5) | 8 + 5 | `11000000` |
| 281-284 | Length 131-258 (extra 5) | 8 + 5 | `11000001` - `11000100` |
| 284 | Length 227-257 (note overlap) | 8 + 5 | `11000101` |
| 285 | Length 258 | 8 + 0 | `11000110` |

#### Length Extra Bits (symbols 257-285)

These are standard DEFLATE lengths (RFC 1951 §3.2.5):

```
sym: 257 258 259 260 261 262 263 264 265 266 267 268
len:   3   4   5   6   7   8   9  10  11  13  15  17
xtr:   0   0   0   0   0   0   0   0   1   1   1   1

sym: 269 270 271 272 273 274 275 276 277 278 279 280
len:  19  23  27  31  35  43  51  59  67  83  99 115
xtr:   2   2   2   2   3   3   3   3   4   4   4   4

sym: 281 282 283 284 285
len: 131 163 195 227 258
xtr:   5   5   5   5   0
```

#### Distance Alphabet (symbols 0-29)

All symbols use **5-bit** fixed codes:

```
sym:   0   1   2   3   4   5   6   7   8   9  10  11  12  13  14
dist:  1   2   3   4   5   7   9  13  17  25  33  49  65  97 129
xtr:   0   0   0   0   1   1   2   2   3   3   4   4   5   5   6

sym:  15  16  17  18  19  20  21  22  23  24  25  26  27  28  29
dist:193 257 385 513 769 1025 1537 2049 3073 4097 6145 8193 12289 16385 24577
xtr:   6   7   7   8   8    9    9   10   10   11   11   12    12    13    13
```

### 2.3 Bit Writer

The core primitive for the encoder:

```rust
struct BitWriter {
    buf: Vec<u8>,
    bits: u64,       // pending bit accumulator (LSB first)
    nbits: u8,       // bits currently in accumulator (0-63)
}
```

Key operations:
- `write_bits(value: u16, n: u8)` — append `n` LSBs of `value`; flush bytes when accumulator fills
- `write_literal(byte: u8)` — write one literal using fixed Huffman code
- `write_match(len: u16, dist: u16)` — write length symbol + extra bits, then distance symbol + extra bits
- `flush_byte()` — force next write to be byte-aligned (for block header alignment in stored blocks)
- `finish()` — flush remaining bits, pad to byte boundary with zeros

### 2.4 LZ77 Matcher

**Approach**: Hash-chain based with 3-byte hash, greedy matching.

```
Parameters:
  MIN_MATCH    = 3          (shortest match worth encoding)
  MAX_MATCH    = 258        (longest encodable match)
  WINDOW_SIZE  = 32768      (max distance)
  HASH_SIZE    = 32768      (hash table for 3-byte sequences)

Hash function:
  h = (data[i] << 10) ^ (data[i+1] << 5) ^ data[i+2]
  bucket = h & (HASH_SIZE - 1)     [or h % HASH_SIZE for non-power-of-2]

Algorithm:
  head[0..HASH_SIZE] ← NIL
  prev[0..data.len]   ← NIL

  for pos in 0..data.len:
    if pos + MIN_MATCH > data.len: break

    hash ← compute_hash(data, pos)
    match_pos ← head[hash]

    best_len ← 0
    best_dist ← 0
    chain_depth ← 0

    while match_pos ≠ NIL and chain_depth < MAX_CHAIN:
      if pos - match_pos > WINDOW_SIZE: break

      len ← common_prefix_len(data, pos, match_pos)
      if len > best_len:
        best_len ← len
        best_dist ← pos - match_pos
        if best_len >= MAX_MATCH: break
        if best_len >= 32: break  // good enough

      match_pos ← prev[match_pos]
      chain_depth += 1

    if best_len >= MIN_MATCH:
      emit match(best_len, best_dist)
      // Update hash for all positions in the match
      for j in 0..best_len-1:
        if pos + j + 2 < data.len:
          update_chain(data, pos + j, hash)
      pos += best_len - 1
    else:
      emit literal(data[pos])
      update_chain(data, pos, hash)
```

**Tuning for JSON**:
- `MAX_CHAIN = 32` — good balance; JSON has many repeated short patterns
- `MIN_MATCH = 3` — standard
- For payloads < 256 bytes, brute-force search (no hash table) is simpler and sufficient

**Edge case**: For very small payloads (<100B), LZ77 overhead can exceed savings. We can add a threshold: if `data.len() < 128`, skip matching and emit all literals in a single fixed-Huffman block. The gzip header + CRC32 overhead still applies for Content-Encoding correctness.

### 2.5 Encoder Pseudocode

```rust
fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    let mut bw = BitWriter::new();

    // BFINAL=1 (final block), BTYPE=1 (fixed Huffman)
    bw.write_bits(1, 1);  // BFINAL
    bw.write_bits(0b01, 2);  // BTYPE

    let mut pos = 0;
    while pos < data.len() {
        if let Some((len, dist)) = find_match(data, pos) {
            let sym = length_to_symbol(len);
            bw.write_bits(FIXED_LL_CODE[sym], FIXED_LL_BITS[sym]);
            if let Some(extra) = length_extra_bits(sym) {
                bw.write_bits(len - base_length(sym), extra);
            }

            let dsym = distance_to_symbol(dist);
            bw.write_bits(dsym as u16, 5);  // fixed 5-bit distance code
            if let Some(extra) = distance_extra_bits(dsym) {
                bw.write_bits(dist - base_distance(dsym), extra);
            }
            pos += len;
        } else {
            let byte = data[pos];
            bw.write_bits(FIXED_LL_CODE[byte as usize], FIXED_LL_BITS[byte as usize]);
            pos += 1;
        }
    }

    // End-of-block (symbol 256)
    bw.write_bits(FIXED_LL_CODE[256], FIXED_LL_BITS[256]);  // 7 bits: 0000000
    bw.flush_to_byte();

    bw.finish()
}
```

### 2.6 Fixed Huffman Code Tables (Precomputed)

The `FIXED_LL_CODE` and `FIXED_LL_BITS` tables can be generated at compile time or with a build script:

```rust
// Pre-computed fixed Huffman codes for literal/length alphabet (0-285)
// Generated from RFC 1951 §3.2.6 canonical code construction.

const FIXED_LL_BITS: [u8; 288] = [
    // 0-143: 8-bit codes
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,  // 16
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,  // 32
    // ... (144 entries of 8)
    // 144-255: 9-bit codes
    9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,  // 160
    // ... (112 entries of 9)
    // 256-279: 7-bit codes
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,  // 272
    7, 7, 7, 7, 7, 7, 7, 7,                              // 280
    // 280-287: 8-bit codes
    8, 8, 8, 8, 8, 8, 8, 8,
];
```

The actual code values follow the canonical construction algorithm. Rather than a giant hand-written table, the encoder should use a `const`-evaluated function or a build script (`build.rs`) that calls the canonical code generation function at compile time, avoiding both magic numbers and runtime overhead.

Alternatively, for maximum simplicity and to stay within 300-400 lines, inline the 288-entry code table directly (it's ~50 lines of Rust array literals).

### 2.7 Gzip Container (RFC 1952)

The complete gzip output structure:

```
Offset  Size  Field         Value
------  ----  -----         -----
0       1     ID1           0x1F
1       1     ID2           0x8B
2       1     CM            0x08 (deflate)
3       1     FLG           0x00 (no name, no comment, no extra, no CRC16)
4       4     MTIME         0x00000000 (no timestamp, or current unix time)
8       1     XFL           0x02 (maximum compression used) or 0x04 (fastest)
9       1     OS            0xFF (unknown)
------  ----  -----         -----
10      N     Compressed    DEFLATE stream (our fixed-Huffman blocks)
------  ----  -----         -----
10+N    4     CRC32         IEEE 802.3 CRC-32 of uncompressed data
10+N+4  4     ISIZE         Original size modulo 2^32 (low 32 bits)
```

Total overhead per message: **18 bytes** (header 10 + trailer 8).

### 2.8 Expected Compression Ratio

Empirical estimates for monitoring JSON (based on gzip -6 on similar data):

| Payload Size | Typical Ratio | Compressed Size | Savings |
|---|---|---|---|
| 500 B | 1.5:1 - 2:1 | 250-333 B | 33-50% |
| 2 KB | 2.5:1 - 3.5:1 | 570-800 B | 60-71% |
| 5 KB | 3:1 - 4:1 | 1.25-1.67 KB | 67-75% |
| 15 KB | 4:1 - 5:1 | 3-3.75 KB | 75-80% |

**Fixed vs Dynamic Huffman penalty**: For JSON, which is dominated by ASCII characters (0x20-0x7E), the fixed Huffman codes are actually quite efficient — ASCII letters fall in the 8-bit code range (0-143), which is optimal. The 9-bit codes (144-255) are rarely emitted for JSON (only occasional UTF-8 multi-byte sequences, curly quotes, etc.). The penalty vs dynamic Huffman is typically **3-5%** for JSON payloads.

**Worst case** (random / already-compressed / encrypted data): Fixed Huffman DEFLATE expands data by approximately **0.03%** (3 bits per 1000 bytes) from the block header, plus gzip framing (18 bytes). For incompressible data, the output is essentially the same size as input.

---

## 3. Stored-Blocks-Only Alternative

### 3.1 What It Is

DEFLATE supports a "stored block" type (BTYPE=00) where data is stored verbatim with no compression. The block structure is:

```
Byte 0:     BFINAL (1 bit) + BTYPE (2 bits = 00) + padding (5 bits)
Bytes 1-2:  LEN   (u16 LE) — uncompressed length
Bytes 3-4:  NLEN  (u16 LE) — one's complement of LEN
Bytes 5+:   LEN bytes of raw data
```

Total overhead per stored block: **5 bytes** (1 header + 2 len + 2 nlen).
Plus gzip framing overhead: **18 bytes** (header + trailer).
**Total overhead: 23 bytes.**

### 3.2 Comparison

| Dimension | Fixed Huffman DEFLATE | Stored Blocks Only |
|---|---|---|
| Code size | ~350 lines | ~40 lines |
| Dependencies | None (self-contained) | None (self-contained) |
| Compression | 2:1 to 5:1 for JSON | None (overhead only: +23 bytes) |
| Wire compatibility | Full gzip/DEFLATE | Full gzip/DEFLATE |
| CPU cost | LZ77 matching (hash chain scan) | Near-zero (memcpy + framing) |
| Risk | Moderate (LZ77 edge cases) | Very low |

### 3.3 When Stored Blocks Make Sense

- **Development bootstrap**: Implement stored-blocks first to validate the gzip framing, HTTP Content-Encoding integration, and server-side acceptance. Then upgrade to fixed Huffman.
- **Extremely constrained targets**: If the agent runs on a device with severe CPU or code-size limits where even a 350-line LZ77 encoder is too expensive.
- **Already-small payloads**: Under ~100 bytes, stored blocks produce smaller output than fixed Huffman (which has the 1-bit BFINAL + 2-bit BTYPE + 7-bit EOB = 10-bit block overhead plus the per-literal Huffman coding).

### 3.4 Recommendation

Stored-blocks-only is a **fallback, not the primary strategy**. The primary strategy is fixed Huffman DEFLATE. The stored-blocks path can be conditionally compiled or selected at runtime for payloads below a threshold (e.g. 200 bytes).

---

## 4. Decompression Needs Assessment

### 4.1 Does the Agent Need to Decompress?

**No.** The agent is a **gzip producer only**. It compresses outbound HTTP POST bodies and sets `Content-Encoding: gzip`. It never receives gzip-compressed responses from the server (the server returns plain JSON-RPC responses).

Evidence from the Go codebase:
- All `gzipBytes()` calls are on the **request path** (encoding outbound data)
- No `gzip.NewReader()` or `Content-Encoding: gzip` response handling exists anywhere in the Go agent
- WebSocket messages are plain JSON (WebSocket per-message deflate is handled transparently by the library)

### 4.2 Implications

- **No decompression code needed** in the Rust agent
- No `inflate` implementation required
- The encoder can be write-only (simpler API, less code, less attack surface)
- If the server ever starts sending gzip-compressed responses in the future, decompression would need to be added, but this is unlikely given the JSON-RPC 2.0 protocol constraints

### 4.3 Forward Compatibility

If decompression becomes necessary, options in order of preference:
1. Use `flate2` crate (miniz_oxide backend) — the original constraint ("no flate2") only applies to the **encoder** to keep the binary small; adding a decompressor later for responses doesn't bloat the encoding path
2. Implement a DEFLATE decoder (~200 additional lines) — straightforward since DEFLATE decoding is simpler than encoding (no LZ77 search needed)

---

## 5. Recommendation with Function Signatures

### 5.1 Primary Recommendation

**Fixed Huffman DEFLATE encoder** as the default compression path, with a fallback to stored-blocks for payloads under a configurable threshold (default: 200 bytes).

### 5.2 Crate Structure

```
src/
  compress/
    mod.rs       — public API, feature flags, module docs
    crc32.rs     — table-driven CRC32 (~80 lines)
    deflate.rs   — fixed-Huffman DEFLATE encoder (~300-350 lines)
    gzip.rs      — gzip container wrapper (~60 lines)
```

**Total estimated lines**: ~450-500 (slightly over the 300-400 estimate due to CRC32 separation and gzip framing)

### 5.3 Public API

```rust
// === src/compress/mod.rs ===

/// Compress data with gzip (fixed-Huffman DEFLATE).
///
/// Returns `Some(compressed_bytes)` on success, or `None` if compression
/// fails (e.g., memory allocation failure).  An empty input produces
/// a valid gzip stream with an empty DEFLATE block.
///
/// The output is a complete gzip file suitable for HTTP `Content-Encoding: gzip`.
pub fn gzip_bytes(data: &[u8]) -> Option<Vec<u8>>;

/// Compress with a strategy chosen based on payload size.
///
/// - Payloads under `stored_threshold` bytes: stored-blocks-only (no LZ77)
/// - Payloads at or above the threshold: fixed-Huffman DEFLATE
///
/// `stored_threshold` defaults to 200. Use `gzip_bytes` directly to force
/// fixed Huffman regardless of size.
pub fn gzip_bytes_adaptive(data: &[u8], stored_threshold: usize) -> Option<Vec<u8>>;

/// Internal: compress using stored-blocks-only DEFLATE (no LZ77).
/// Always produces valid gzip output, but with zero compression.
/// Overhead: 23 bytes (gzip header 10 + trailer 8 + stored block header 5).
pub(crate) fn gzip_bytes_stored(data: &[u8]) -> Option<Vec<u8>>;
```

```rust
// === src/compress/crc32.rs ===

/// IEEE 802.3 CRC-32 (Ethernet / gzip / PNG polynomial: 0xEDB88320 reflected).
///
/// Returns the CRC-32 checksum of `data`.
pub fn crc32(data: &[u8]) -> u32;

/// Update a running CRC-32 checksum with additional data.
///
/// Equivalent to `crc32(previous_data + new_data)` but avoids re-scanning.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32;

/// CRC-32 lookup table (256 × u32 = 1024 bytes).
/// Generated from polynomial 0xEDB88320 (reflected form of 0x04C11DB7).
pub const CRC32_TABLE: [u32; 256];
```

```rust
// === src/compress/deflate.rs ===

/// Fixed-Huffman DEFLATE LZ77 match parameters.
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW_SIZE: usize = 32768;
const HASH_SIZE: usize = 32768;
const MAX_CHAIN: usize = 32;

/// Emit a complete DEFLATE stream using fixed Huffman coding (BTYPE=01).
///
/// Produces a single final block containing the entire input.
pub(crate) fn deflate_fixed(data: &[u8]) -> Vec<u8>;

/// Emit a complete DEFLATE stream using stored blocks (BTYPE=00, no compression).
pub(crate) fn deflate_stored(data: &[u8]) -> Vec<u8>;

// Internal helpers:
pub(crate) struct BitWriter { /* ... */ }
pub(crate) const FIXED_LL_BITS: [u8; 288];
pub(crate) const FIXED_LL_CODE: [u16; 288];
pub(crate) const LENGTH_BASE: [u16; 29];    // base length for symbols 257-285
pub(crate) const LENGTH_EXTRA: [u8; 29];    // extra bits for symbols 257-285
pub(crate) const DIST_BASE: [u16; 30];      // base distance for symbols 0-29
pub(crate) const DIST_EXTRA: [u8; 30];      // extra bits for symbols 0-29
```

```rust
// === src/compress/gzip.rs ===

/// Assemble a complete gzip file from raw DEFLATE data + original input.
///
/// Inputs:
///   - `deflate_data`: the DEFLATE-compressed stream
///   - `original_data`: the uncompressed input (for CRC32 and ISIZE)
///
/// Output: complete gzip file (RFC 1952) with 10-byte header and 8-byte trailer.
pub(crate) fn wrap_gzip(deflate_data: &[u8], original_data: &[u8]) -> Vec<u8>;
```

### 5.4 Integration Points in Message Pipeline

```rust
// === src/server/post.rs (new file) ===

use crate::compress::gzip_bytes;

/// Send a v2 JSON-RPC request via HTTP POST, optionally gzip-compressed.
/// Mirrors Go's postV2RequestContext / postV2RPC.
pub fn post_v2_request(
    cfg: &Config,
    payload: &[u8],
) -> Result<Response, PostError> {
    let (body, is_compressed) = if cfg.disable_compression {
        (payload.to_vec(), false)
    } else {
        match gzip_bytes(payload) {
            Some(gz) => (gz, true),
            None => (payload.to_vec(), false),  // graceful degradation
        }
    };

    let mut req = build_http_request(cfg, &body)?;
    if is_compressed {
        req.set_header("Content-Encoding", "gzip");
    }

    send_request(req)
}
```

**Integration summary**:
1. `src/compress/mod.rs` — re-exports `gzip_bytes`, `gzip_bytes_adaptive`
2. `src/server/websocket.rs` — calls `gzip_bytes()` in the POST fallback path
3. `src/server/task.rs` — calls `gzip_bytes()` in `post_v2_rpc()`
4. `src/config.rs` — exposes `disable_compression: bool` flag
5. The compression flag also controls WebSocket per-message deflate (via tungstenite extension config)

### 5.5 Feature Gating (Optional)

```toml
# Cargo.toml
[features]
default = ["gzip-fixed"]
gzip-fixed = []          # Fixed-Huffman DEFLATE (recommended, ~350 lines)
gzip-stored = []         # Stored-blocks only (fallback, ~40 lines)
# If neither feature is enabled, compression is a no-op
```

This allows the user to strip out the LZ77 matcher entirely if they only want stored blocks (trading compression for a smaller binary).

---

## 6. CRC32 Table-Driven Implementation

### 6.1 Design

```rust
// src/compress/crc32.rs
// ~80 lines

/// Polynomial: 0xEDB88320 (reflected form of IEEE 802.3 CRC-32 polynomial 0x04C11DB7).
///
/// This is the standard CRC-32 used by gzip, PNG, Ethernet, and zip.
/// The reflected form simplifies byte-at-a-time table-driven computation:
///   table[i] = CRC of the single byte i (with zero initial value)
///
/// Generated once at compile time via const evaluation.

pub const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Compute CRC-32 checksum of `data`.
///
/// Conforms to the gzip/PNG/Ethernet CRC-32 standard.
/// Initial value: 0xFFFFFFFF
/// Final XOR:     0xFFFFFFFF
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Update a running CRC-32 checksum.
///
/// `crc` is the current checksum value (from a previous `crc32_update` call,
/// or the initial value 0xFFFFFFFF for a new computation).
///
/// Returns the updated checksum. Do NOT apply the final XOR until all data
/// has been processed — that is the caller's responsibility (handled by
/// `crc32()` above).
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[index] ^ (crc >> 8);
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        // CRC32 of empty data = 0x00000000 (after final XOR)
        assert_eq!(crc32(b""), 0x0000_0000);
    }

    #[test]
    fn test_known_vectors() {
        // "123456789" → 0xCBF43926 (standard CRC-32 check value)
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // Single byte
        assert_eq!(crc32(b"\x00"), 0xD202_EF8D);
        assert_eq!(crc32(b"\xFF"), 0xFF00_0000);
    }

    #[test]
    fn test_update_equivalent() {
        let data = b"hello world";
        // Full computation should equal chunked computation
        let full = crc32(data);
        let mut part = 0xFFFF_FFFF;
        part = crc32_update(part, b"hello ");
        part = crc32_update(part, b"world");
        assert_eq!(full, part ^ 0xFFFF_FFFF);
    }

    #[test]
    fn test_streaming() {
        // Build up CRC across many small updates
        let data = vec![0x41u8; 1000]; // 1000 'A' bytes
        let mut crc = 0xFFFF_FFFF;
        for chunk in data.chunks(7) {
            crc = crc32_update(crc, chunk);
        }
        assert_eq!(crc ^ 0xFFFF_FFFF, crc32(&data));
    }
}
```

### 6.2 Key Properties

- **Table size**: 256 × 4 bytes = 1024 bytes (fits in L1 cache)
- **Computed at compile time**: No runtime initialization, no lazy_static/OnceLock needed
- **Performance**: ~1 CPU cycle per byte on modern x86 (simple table lookup + shift + XOR)
- **Correctness**: Uses the standard gzip polynomial with reflected form for byte-at-a-time processing

---

## 7. Integration in Message Pipeline

### 7.1 Architecture Diagram

```
┌─────────────────────────────────────────────────┐
│                   Config                         │
│  disable_compression: bool  ←── CLI flag         │
└─────────────┬───────────────────────────────────┘
              │
    ┌─────────┴──────────┐
    │                    │
    ▼ false              ▼ true
┌───────────┐      ┌───────────┐
│ compress  │      │  raw      │
│ pipeline  │      │  passthru │
└─────┬─────┘      └─────┬─────┘
      │                  │
      ▼                  ▼
┌──────────────────────────────────────────┐
│           post_v2_request()               │
│                                           │
│  let (body, compressed) =                │
│    if cfg.disable_compression {           │
│      (payload.to_vec(), false)            │
│    } else {                               │
│      (gzip_bytes(payload)?, true)         │
│    };                                     │
│                                           │
│  req.set_header("Content-Encoding",       │
│    if compressed { "gzip" } else { ... }) │
└──────────────────────────────────────────┘
```

### 7.2 Call Flow

```
monitoring::generate_report()
        │
        ▼
    [u8] JSON bytes (2-15 KB typical)
        │
        ▼
v2::build_report_payload() / build_report_request()
        │
        ▼
    [u8] JSON-RPC v2 envelope bytes
        │
        ├── WebSocket path (same thread, no compression here — per-message deflate is
        │   handled transparently by tungstenite extension)
        │
        └── POST fallback path
                │
                ▼
            post_v2_request(cfg, payload)
                │
                ├── cfg.disable_compression → body = payload
                └── else → body = gzip_bytes(payload)
                │
                ▼
            HTTP POST with optional Content-Encoding: gzip
```

### 7.3 Edge Cases

| Scenario | Behavior |
|---|---|
| Empty payload `b""` | Produce valid gzip: 10-byte header + empty deflate block (BFINAL=1, BTYPE=01, EOB symbol 256, padded to byte) + 8-byte trailer. Total: ~30 bytes for empty data. |
| Payload < 200 bytes | `gzip_bytes_adaptive()` falls back to stored blocks. `gzip_bytes()` does fixed Huffman anyway (still valid, but may expand slightly). |
| Very large payload (>100KB) | Unlikely for monitoring reports, but the LZ77 window is 32KB, so repeated patterns beyond 32KB apart won't match. Fixed Huffman still produces valid output. |
| `gzip_bytes()` returns `None` | Allocation failure. Caller falls back to sending uncompressed. This is a graceful degradation — the server accepts both. |
| Binary (non-UTF-8) data | The DEFLATE literal alphabet (0-255) handles arbitrary bytes. Fixed Huffman codes are equally valid for binary data. |
| Server rejects gzip | The `Content-Encoding: gzip` header is conditional. If the server returns 415/400 for compressed requests, the protocol fallback state machine will eventually switch to v1 where compression is not used. This is already handled by the Go agent's 3-strike protocol fallback. |

### 7.4 Binary Size Impact

```
Module                Approximate size (release, stripped)
─────────────────────────────────────────────────────────
CRC32 table             1,024 bytes (data)
CRC32 functions           150 bytes (code)
Fixed Huffman tables     ~800 bytes (data: LL codes, length/dist bases)
BitWriter                 200 bytes (code)
LZ77 matcher              400 bytes (code)
Gzip framing              100 bytes (code)
Public API glue            80 bytes (code)
─────────────────────────────────────────────────────────
TOTAL                  ~2,754 bytes
```

Compared to `flate2` + `miniz_oxide` which adds ~20-50KB to the binary, a custom fixed-Huffman encoder adds less than 3KB. For an agent binary targeting 500KB-1MB (release with `opt-level="z"`, `lto="fat"`), this is negligible.

---

## 8. Implementation Sequence

1. **CRC32 + gzip framing** (1-2 hours)
   - `src/compress/crc32.rs` — compile-time table, `crc32()`/`crc32_update()`
   - `src/compress/gzip.rs` — `wrap_gzip()` with magic bytes, header, trailer
   - Unit tests against known gzip outputs

2. **Stored-blocks DEFLATE** (1 hour)
   - `src/compress/deflate.rs` — `deflate_stored()` function
   - `src/compress/mod.rs` — `gzip_bytes_stored()` public API
   - Integration test: compress + verify with external `gzip -d`

3. **Fixed-Huffman DEFLATE** (3-4 hours)
   - `BitWriter` struct with `write_bits()`, `flush()`
   - Precomputed fixed Huffman code tables
   - Literal emission path
   - LZ77 hash-chain matcher
   - Match emission path (length/distance symbols + extra bits)
   - End-to-end test: round-trip through `gzip -d`

4. **Integration** (1 hour)
   - Hook into `post_v2_request()` / `post_v2_rpc()` 
   - Wire up `Config::disable_compression`
   - Test with actual komari server

---

## Appendix A: Gzip Magic Number Reference

```
Byte sequence for an empty gzip file:
1F 8B 08 00 00 00 00 00 02 FF 03 00 00 00 00 00 00 00 00 00 00
│     │  │  │           │  │  │                       │        │
ID    CM FLG MTIME      XFL OS DEFLATE (empty)        CRC32   ISIZE=0
```

## Appendix B: References

- RFC 1950: ZLIB Compressed Data Format Specification
- RFC 1951: DEFLATE Compressed Data Format Specification
- RFC 1952: GZIP file format specification
- Go `compress/gzip` source: https://pkg.go.dev/compress/gzip
- CRC-32 catalogue: https://reveng.sourceforge.io/crc-catalogue/17plus.htm
