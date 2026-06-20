// komari-agent-rs — Hand-rolled JSON encoder
//
// Zero-dependency.  No serde.  No format!/Display in hot paths.
// Stack-allocated byte buffer + compile-time Field constants.

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// The one and only error: buffer exhausted.
#[derive(Debug, Clone, Copy)]
pub struct JsonErr;

// ---------------------------------------------------------------------------
// Compile-time field-name constants
// ---------------------------------------------------------------------------

/// Every JSON key the agent ever emits, as a compile-time constant.
///
/// Use `Field::as_bytes()` to get the `&'static [u8]` representation.
/// Variant names mirror their wire form unless the wire form would be an
/// illegal Rust identifier (e.g. `MessageType → "type"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    // --- System ---
    Cpu,         // "cpu"
    Ram,         // "ram"
    Disk,        // "disk"
    Net,         // "network"
    Gpu,         // "gpu"
    Load,        // "load"
    Connections, // "connections"
    Process,     // "process"
    Uptime,      // "uptime"
    Os,          // "os"
    Arch,        // "arch"

    // --- Protocol ---
    Version,     // "version"
    Token,       // "token"
    MessageType, // "type"
    Timestamp,   // "timestamp"
    Count,       // "count"
    Name,        // "name"
    Error,       // "error"
    Message,     // "message"
    Code,        // "code"
    Data,        // "data"
    Id,          // "id"

    // --- Metrics ---
    Usage,     // "usage"
    Total,     // "total"
    Used,      // "used"
    Up,        // "up"
    Down,      // "down"
    TotalUp,   // "totalUp"
    TotalDown, // "totalDown"
    Swap,      // "swap"
    Tcp,       // "tcp"
    Udp,       // "udp"
    Load1,     // "load1"
    Load5,     // "load5"
    Load15,    // "load15"
    Cores,     // "cores"

    // --- GPU ---
    Backend,      // "backend"
    Devices,      // "devices"
    MemoryTotal,  // "memory_total"
    MemoryUsed,   // "memory_used"
    Utilization,  // "utilization"
    Temperature,  // "temperature"
    AverageUsage, // "average_usage"
    DetailedInfo, // "detailed_info"
    Models,       // "models"
}

impl Field {
    /// Wire-form byte string for this field name.
    #[inline]
    pub const fn as_bytes(&self) -> &'static [u8] {
        match self {
            Field::Cpu => b"cpu",
            Field::Ram => b"ram",
            Field::Disk => b"disk",
            Field::Net => b"network",
            Field::Gpu => b"gpu",
            Field::Load => b"load",
            Field::Connections => b"connections",
            Field::Process => b"process",
            Field::Uptime => b"uptime",
            Field::Os => b"os",
            Field::Arch => b"arch",
            Field::Version => b"version",
            Field::Token => b"token",
            Field::MessageType => b"type",
            Field::Timestamp => b"timestamp",
            Field::Count => b"count",
            Field::Name => b"name",
            Field::Error => b"error",
            Field::Message => b"message",
            Field::Code => b"code",
            Field::Data => b"data",
            Field::Id => b"id",
            Field::Usage => b"usage",
            Field::Total => b"total",
            Field::Used => b"used",
            Field::Up => b"up",
            Field::Down => b"down",
            Field::TotalUp => b"totalUp",
            Field::TotalDown => b"totalDown",
            Field::Swap => b"swap",
            Field::Tcp => b"tcp",
            Field::Udp => b"udp",
            Field::Load1 => b"load1",
            Field::Load5 => b"load5",
            Field::Load15 => b"load15",
            Field::Cores => b"cores",
            // --- GPU ---
            Field::Backend => b"backend",
            Field::Devices => b"devices",
            Field::MemoryTotal => b"memory_total",
            Field::MemoryUsed => b"memory_used",
            Field::Utilization => b"utilization",
            Field::Temperature => b"temperature",
            Field::AverageUsage => b"average_usage",
            Field::DetailedInfo => b"detailed_info",
            Field::Models => b"models",
        }
    }
}

// ---------------------------------------------------------------------------
// JsonBuf
// ---------------------------------------------------------------------------

/// Stack-allocated JSON byte buffer with cursor.
///
/// ## Comma tracking
///
/// A small fixed-depth stack tracks whether a comma is needed before the next
/// item at each nesting level (8 levels max — more than enough for monitoring
/// JSON which rarely exceeds depth 3).
///
/// ## Usage pattern
///
/// ```ignore
/// let mut buf = [0u8; 4096];
/// let mut j = JsonBuf::new(&mut buf);
///
/// j.begin_obj()?;                          // {
/// j.str_field(Field::MessageType, "cpu")?; // "type":"cpu"
/// j.u64_field(Field::Timestamp, 12345)?;   // ,"timestamp":12345
/// j.begin_arr_field(Field::Data)?;         // ,"data":[
/// push_u64(&mut j, 42)?;                   //   42
/// push_u64(&mut j, 99)?;                   //   ,99
/// j.end_arr()?;                            // ]
/// j.end_obj()?;                            // }
///
/// let out = j.finish();
/// ```
pub struct JsonBuf<'a> {
    buf: &'a mut [u8],
    cursor: usize,
    /// Per-level comma-needed flag.  `need_comma[i]` is set to true after the
    /// first item at nesting level `i` has been written.
    need_comma: [bool; 8],
    /// Current nesting depth (0 = top-level, 1 = inside first `{` or `[`, …).
    depth: u32,
}

impl<'a> JsonBuf<'a> {
    /// Create a new `JsonBuf` backed by `buf`.
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            cursor: 0,
            need_comma: [false; 8],
            depth: 0,
        }
    }

    /// Return the slice of written bytes.  The returned slice is valid UTF-8
    /// (ASCII subset) and is a prefix of the original buffer.
    #[inline]
    pub fn finish(&self) -> &[u8] {
        &self.buf[..self.cursor]
    }

    // ------------------------------------------------------------------
    // Low-level primitives
    // ------------------------------------------------------------------

    /// Write a single byte.  Returns `Err(JsonErr)` if the buffer is full.
    #[inline]
    pub fn push_byte(&mut self, b: u8) -> Result<(), JsonErr> {
        if self.cursor < self.buf.len() {
            self.buf[self.cursor] = b;
            self.cursor += 1;
            Ok(())
        } else {
            Err(JsonErr)
        }
    }

    /// Write a raw byte slice (no escaping).
    #[inline]
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), JsonErr> {
        for &b in bytes {
            self.push_byte(b)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Comma / depth helpers
    // ------------------------------------------------------------------

    /// Write `,` if this nesting level already has at least one item, then
    /// mark this level as "has items".  Safe to call before every value.
    #[inline]
    pub fn comma(&mut self) -> Result<(), JsonErr> {
        if self.depth > 0 {
            let idx = (self.depth - 1) as usize;
            if idx < 8 {
                if self.need_comma[idx] {
                    self.push_byte(b',')?;
                }
                self.need_comma[idx] = true;
            }
        }
        Ok(())
    }

    /// Clear the comma flag at the current nesting level (do **not** write
    /// `,`).  Used internally by field methods so the subsequent value push
    /// (which also calls `comma()`) does not emit a second comma.
    #[inline]
    fn clear_comma(&mut self) {
        if self.depth > 0 {
            let idx = (self.depth - 1) as usize;
            if idx < 8 {
                self.need_comma[idx] = false;
            }
        }
    }

    fn push_depth(&mut self) -> Result<(), JsonErr> {
        if self.depth >= 8 {
            return Err(JsonErr);
        }
        self.need_comma[self.depth as usize] = false;
        self.depth += 1;
        Ok(())
    }

    fn pop_depth(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
        }
    }

    // ------------------------------------------------------------------
    // Structure methods
    // ------------------------------------------------------------------

    /// Begin a JSON object: writes `{` and pushes a new comma-tracking level.
    /// If called inside an array (e.g. `[{…}, {…}]`), a comma is emitted
    /// automatically.
    pub fn begin_obj(&mut self) -> Result<(), JsonErr> {
        self.comma()?;
        self.push_byte(b'{')?;
        self.push_depth()
    }

    /// End a JSON object: writes `}` and pops the comma-tracking level.
    pub fn end_obj(&mut self) -> Result<(), JsonErr> {
        self.push_byte(b'}')?;
        self.pop_depth();
        Ok(())
    }

    /// Begin a JSON array as a bare value (no key).  Writes `[` and pushes a
    /// new comma-tracking level.  Use `begin_arr_field` when writing a named
    /// field inside an object.
    pub fn begin_arr(&mut self) -> Result<(), JsonErr> {
        self.comma()?;
        self.push_byte(b'[')?;
        self.push_depth()
    }

    /// End a JSON array: writes `]` and pops the comma-tracking level.
    pub fn end_arr(&mut self) -> Result<(), JsonErr> {
        self.push_byte(b']')?;
        self.pop_depth();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Typed field writers (for use inside objects)
    //
    // Each field method:
    // 1. Calls `comma()` to emit `,` if there is a previous field.
    // 2. Writes `"key":`.
    // 3. Clears the comma flag so the value-writing free function does not
    //    emit a spurious second comma.
    // 4. Delegates to the appropriate free function (which calls `comma()`
    //    itself, re-setting the flag for the next field).
    // ------------------------------------------------------------------

    /// Write `"field_name":"escaped_value"`.
    pub fn str_field(&mut self, field: Field, value: &str) -> Result<(), JsonErr> {
        self.write_field_prefix(field)?;
        self.clear_comma();
        push_json_str(self, value)
    }

    /// Write `"field_name":<u64>`.
    pub fn u64_field(&mut self, field: Field, value: u64) -> Result<(), JsonErr> {
        self.write_field_prefix(field)?;
        self.clear_comma();
        push_u64(self, value)
    }

    /// Write `"field_name":<f64>` (one decimal place).
    pub fn f64_field(&mut self, field: Field, value: f64) -> Result<(), JsonErr> {
        self.write_field_prefix(field)?;
        self.clear_comma();
        push_f64_one_decimal(self, value)
    }

    /// Write `"field_name":true` or `"field_name":false`.
    pub fn bool_field(&mut self, field: Field, value: bool) -> Result<(), JsonErr> {
        self.write_field_prefix(field)?;
        if value {
            self.push_bytes(b"true")
        } else {
            self.push_bytes(b"false")
        }
    }

    /// Begin a named object field: writes `"field_name":{` and pushes a new
    /// comma-tracking level for the object contents.  Pair with `end_obj()`.
    pub fn begin_obj_field(&mut self, field: Field) -> Result<(), JsonErr> {
        self.comma()?;
        self.push_byte(b'"')?;
        self.push_bytes(field.as_bytes())?;
        self.push_bytes(b"\":{")?;
        self.push_depth()
    }

    /// Begin a named array field: writes `"field_name":[` and pushes a new
    /// comma-tracking level for the array contents.  Pair with `end_arr()`.
    pub fn begin_arr_field(&mut self, field: Field) -> Result<(), JsonErr> {
        self.comma()?;
        self.push_byte(b'"')?;
        self.push_bytes(field.as_bytes())?;
        self.push_bytes(b"\":[")?;
        self.push_depth()
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Write comma (if needed), then `"field_name":`.
    #[inline]
    fn write_field_prefix(&mut self, field: Field) -> Result<(), JsonErr> {
        self.comma()?;
        self.push_byte(b'"')?;
        self.push_bytes(field.as_bytes())?;
        self.push_bytes(b"\":")
    }
}

// ---------------------------------------------------------------------------
// Public formatting free-functions
// ---------------------------------------------------------------------------

/// Push a correctly-escaped JSON string (including surrounding `"` quotes),
/// with comma if needed at the current nesting level.
///
/// Escapes: `\"`, `\\`, `\n`, `\r`, `\t`, `\u00XX` for control chars < 0x20.
/// All other bytes (including UTF-8 continuation bytes ≥ 0x80) pass through
/// unchanged.
pub fn push_json_str(buf: &mut JsonBuf, s: &str) -> Result<(), JsonErr> {
    buf.comma()?;
    buf.push_byte(b'"')?;
    for &b in s.as_bytes() {
        match b {
            b'"' => buf.push_bytes(b"\\\"")?,
            b'\\' => buf.push_bytes(b"\\\\")?,
            b'\n' => buf.push_bytes(b"\\n")?,
            b'\r' => buf.push_bytes(b"\\r")?,
            b'\t' => buf.push_bytes(b"\\t")?,
            0x00..=0x1F => {
                buf.push_bytes(b"\\u00")?;
                let hi = b >> 4;
                let lo = b & 0x0F;
                buf.push_byte(hex_digit(hi))?;
                buf.push_byte(hex_digit(lo))?;
            }
            _ => buf.push_byte(b)?,
        }
    }
    buf.push_byte(b'"')
}

/// Push a `u64` as decimal ASCII, with comma if needed at the current
/// nesting level.  Uses a stack-local `[u8; 20]` buffer and fills it in
/// reverse — no heap, no `format!`.
pub fn push_u64(buf: &mut JsonBuf, n: u64) -> Result<(), JsonErr> {
    buf.comma()?;
    let mut scratch: [u8; 20] = [0; 20];
    let mut i = 20;
    let mut v = n;
    if v == 0 {
        scratch[19] = b'0';
        i = 19;
    } else {
        while v > 0 {
            i -= 1;
            scratch[i] = (v % 10) as u8 + b'0';
            v /= 10;
        }
    }
    for &b in &scratch[i..] {
        buf.push_byte(b)?;
    }
    Ok(())
}

/// Push an `f64` with exactly one decimal digit, with comma if needed.
///
/// Truncation semantics (not rounding): the fractional part beyond the first
/// decimal is discarded.  NaN and infinities are rendered as JSON `null`.
pub fn push_f64_one_decimal(buf: &mut JsonBuf, n: f64) -> Result<(), JsonErr> {
    buf.comma()?;
    // JSON does not support NaN/Infinity — emit null.
    if n.is_nan() || n.is_infinite() {
        return buf.push_bytes(b"null");
    }
    if n < 0.0 {
        buf.push_byte(b'-')?;
    }
    let abs = n.abs();
    // `as u64` truncates toward zero — safe here because `abs` is non-negative.
    let int_part = abs as u64;
    // Compute the first fractional digit with a tiny epsilon to counteract
    // floating-point representation error (e.g. 0.1 represented as
    // 0.09999999…).  The epsilon is 1e-9, far below the precision of one
    // decimal digit.
    let dec_digit = ((abs - int_part as f64) * 10.0 + 1e-9) as u8;
    push_u64_raw(buf, int_part)?;
    buf.push_byte(b'.')?;
    buf.push_byte(b'0' + dec_digit.min(9))
}

// ---------------------------------------------------------------------------
// Internal raw-push helpers (no comma, no escaping — for use inside the
// formatted-value functions that already handled the comma).
// ---------------------------------------------------------------------------

/// Push `n` as decimal digits — no comma check.  (Internal.)
fn push_u64_raw(buf: &mut JsonBuf, n: u64) -> Result<(), JsonErr> {
    let mut scratch: [u8; 20] = [0; 20];
    let mut i = 20;
    let mut v = n;
    if v == 0 {
        scratch[19] = b'0';
        i = 19;
    } else {
        while v > 0 {
            i -= 1;
            scratch[i] = (v % 10) as u8 + b'0';
            v /= 10;
        }
    }
    for &b in &scratch[i..] {
        buf.push_byte(b)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[inline]
fn hex_digit(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + n - 10 }
}

// ---------------------------------------------------------------------------
// EncodeJson trait
// ---------------------------------------------------------------------------

/// Types that can serialize themselves into a `JsonBuf`.
///
/// Implementors write valid JSON fragments.  The caller is responsible for
/// ensuring the fragment is used in a valid JSON context (e.g. a field value,
/// an array element, or the top-level document).
pub trait EncodeJson {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr>;
}

// ---------------------------------------------------------------------------
// EncodeJson impls for common standard-library types
// ---------------------------------------------------------------------------

impl EncodeJson for String {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        push_json_str(j, self.as_str())
    }
}

impl EncodeJson for str {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        push_json_str(j, self)
    }
}

impl EncodeJson for u64 {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        push_u64(j, *self)
    }
}

impl EncodeJson for i64 {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        j.comma()?;
        if *self < 0 {
            j.push_byte(b'-')?;
            push_u64_raw(j, self.unsigned_abs())
        } else {
            push_u64_raw(j, *self as u64)
        }
    }
}

impl EncodeJson for f64 {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        push_f64_one_decimal(j, *self)
    }
}

impl EncodeJson for bool {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        j.comma()?;
        if *self {
            j.push_bytes(b"true")
        } else {
            j.push_bytes(b"false")
        }
    }
}

impl<T: EncodeJson> EncodeJson for &[T] {
    fn encode_json(&self, j: &mut JsonBuf) -> Result<(), JsonErr> {
        j.comma()?;
        j.begin_arr()?;
        // `begin_arr` already pushed a comma level, so item writes will see
        // fresh `comma=false`.  Re-open without an extra nesting.
        for item in self.iter() {
            item.encode_json(j)?;
        }
        j.end_arr()
    }
}
