# Komari-Agent-RS Acceptance Criteria

**Version**: 1.0.0
**Date**: 2026-06-20
**Derived from**: `architecture-reference.md` (SSOT blueprint), `spec.md` (confirmed design decisions), `phased-implementation-plan.md`, `module-inventory.md`

---

## 0. Meta-Principles

### 0.1 Criteria Hierarchy

Every task in every phase must satisfy:

1. **Unit tests** (framework: `#[test]`, no external test deps)
2. **Integration tests** (where applicable: live Komari server or mock)
3. **Performance criteria** (binary size, RSS, zero-alloc, tick jitter)
4. **Code quality** (rustfmt, clippy, unsafe audit)
5. **Cross-platform compilation** (per-task target matrix)
6. **Regression gates** (what existing behavior must not break)

### 0.2 Universal Gates (apply to ALL tasks)

| Gate | Criteria | Check Method |
|------|----------|-------------|
| **G1: rustfmt** | `cargo fmt --check` must pass with zero diffs | CI step |
| **G2: clippy** | `cargo clippy -- -D clippy::all -D clippy::pedantic` must pass. Allowed lints (per `#[allow(clippy::...)]`): `module_name_repetitions`, `cast_precision_loss`, `cast_sign_loss`, `cast_possible_truncation` (monitoring code), `must_use_candidate`. All other clippy lints are errors. | CI step |
| **G3: unsafe** | No `unsafe` except in platform FFI modules (`monitor/gpu/*.rs`, `monitor/*/windows.rs`, `terminal/*.rs`, `monitor/cpu/windows.rs`, platform syscall wrappers). Every `unsafe` block must have a `// SAFETY:` comment citing the invariant. | grep `unsafe` + manual review |
| **G4: no unwrap** | No `.unwrap()` on fallible operations. Use `?` operator, `.expect("invariant: ...")` with a justification, or explicit match. Exceptions: test code, `Mutex::lock().unwrap()` (poison indicates unrecoverable state). | grep `.unwrap()` |
| **G5: git** | Each phase completion = one commit. Within phase, each file group = one commit. Push after each commit. | git log |

### 0.3 Confirmed Design Decisions (from `spec.md` §已确认设计决策)

These override the architecture reference where they conflict:

| # | Decision | Constraint |
|---|----------|-----------|
| **DD1** | CLI parsing: hand-written ~150 lines | NOT clap. Binary saving: ~15 KB + unicode dep chain |
| **DD5** | TLS cert store: OS native roots | NOT webpki-roots. Linux=`/etc/ssl/certs`, Windows=CryptoAPI, macOS=Security.framework |
| **DD9** | Gzip: fixed-Huffman encode-only ~200 lines | NOT full DEFLATE. NOT flate2/miniz_oxide |
| **DD13** | Feature gates: `default=[]` (core ~600 KB), `full` enables all (~876 KB) | Features: `gpu-detection`, `terminal`, `ping`, `self-update` |

Where spec decisions DD1-DD13 conflict with architecture-reference appendices or module descriptions, **the spec wins**.

### 0.4 Dependency Verification

Before phase start: `cargo tree --depth 1` must show expected direct deps (no surprises).

---

## PHASE 1: Foundation + Handshake (~600 lines)

**Goal**: Binary compiles on 4 targets. TLS WebSocket connection. Sends static heartbeat JSON. Crypto + encoding fully tested.

### Task 1.1: Cargo.toml + `.cargo/config.toml` + CI skeleton

| Category | Criteria |
|----------|----------|
| Unit test | Verify `cargo metadata` resolves without errors. Verify `[profile.release]` settings: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`. |
| Integration | N/A |
| Performance | Binary size placeholder: `cargo build --release` on empty `main.rs` must produce <400 KB stripped. |
| Code quality | `Cargo.toml` must list only approved deps (see Appendix A of architecture-reference, overridden by spec DD1-DD13). No wildcard versions. |
| Cross-platform | `.github/workflows/ci.yml` must have 4-OS matrix (ubuntu-latest, windows-latest, macos-latest, freebsd via cross). All jobs must be green with empty project. |
| Regression gate | N/A (first task) |

### Task 1.2: `src/config.rs` — CLI + env config

| Category | Criteria |
|----------|----------|
| Unit test | Test all 34 config fields: parse from args, parse from env var, parse from config file, default values. Test `mem_mode()` derivation: mode 0 (default), mode 1 (`memory_include_cache=true`), mode 2 (`memory_report_raw_used=true`). Test `parse_duration` / `parse_interval`. |
| Integration | N/A |
| Performance | Config struct must `#[derive(Debug, Clone)]`. No heap strings for default values (use `&'static str` where possible, conversion to String at final use). |
| Code quality | Hand-written CLI ~150 lines per DD1. Parser must accept `--token`, `--endpoint`, `--interval`, all flags, `--config-file`. Env override via manual `env::var()` check (no `#[arg(env)]` since no clap). |
| Cross-platform | Must compile and pass unit tests on all 4 platforms. |
| Regression gate | Config struct must have exactly the same field names and semantics as Go `Config` struct (34 fields). Changing a field name or default is a regression. |

### Task 1.3: `src/crypto.rs` — SHA-1 + base64

| Category | Criteria |
|----------|----------|
| Unit test | **SHA-1 RFC 3174 test vectors**: (1) `"abc"` -> `a9993e36 4706816a ba3e2571 7850c26c 9cd0d89d`, (2) `""` -> `da39a3ee 5e6b4b0d 3255bfef 95601890 afd80709`, (3) `"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"` -> `84983e44 1c3bd26e baae4aa1 f95129e5 e54670f1`. **WebSocket accept vector**: `sha1(base64_decode("dGhlIHNhbXBsZSBub25jZQ==") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")` -> base64 must equal `"s3pPLMBiTxaQ9kYGzzhZRbK+xOo="`. **Base64 RFC 4648 vectors**: `""` -> `""`, `"f"` -> `"Zg=="`, `"fo"` -> `"Zm8="`, `"foo"` -> `"Zm9v"`, `"foob"` -> `"Zm9vYg=="`, `"fooba"` -> `"Zm9vYmE="`, `"foobar"` -> `"Zm9vYmFy"`. **Round-trip**: base64_encode(sha1(input)) for 256 random inputs, verify deterministic. **Zero-alloc**: SHA-1 context fits on stack (no `Box`, no `Vec`). |
| Integration | N/A |
| Performance | SHA-1 < 100 lines. Base64 < 60 lines. No external crate deps (not `sha1`, not `base64`, not `digest`). All stack-allocated. |
| Code quality | `// SAFETY:` comments for any raw pointer casts during block permutation. |
| Cross-platform | Must compile on all 4 platforms (pure Rust, no platform-specific code). |
| Regression gate | SHA-1 output must be bit-identical to RFC 3174 reference implementation. Base64 must be bit-identical to RFC 4648. **Wire compatibility**: `Sec-WebSocket-Accept` computed by this module must match what Go's `gorilla/websocket` computes for the same `Sec-WebSocket-Key`. |

### Task 1.4: `src/json.rs` — JsonBuf + Field + EncodeJson

| Category | Criteria |
|----------|----------|
| Unit test | **JsonBuf push correctness**: push_u64(0) -> `"0"`, push_u64(42) -> `"42"`, push_u64(18446744073709551615) -> `"18446744073709551615"`. push_i64(-1) -> `"-1"`, push_i64(0) -> `"0"`, push_i64(-9223372036854775808) -> `"-9223372036854775808"`. push_f64_prec2(0.0) -> `"0.00"`, push_f64_prec2(12.5) -> `"12.50"`, push_f64_prec2(-3.14) -> `"-3.14"`, push_f64_prec2(0.001) -> `"0.00"` (actual check of rounding behavior), push_f64_prec2(f64::NAN) -> `"null"`. **Field encoding round-trip**: serialize known JSON objects, compare byte-for-byte with expected output. **String escaping**: `"` -> `\"`, `\` -> `\\`, `\n` -> `\n` (literal), `\r` -> `\r`, `\t` -> `\t`, control chars 0x00-0x1F -> `\u00XX`. **Arena overflow**: when arena is full, verify JsonBuf falls back to heap Vec. **Buffer reuse**: verify `arena.reset()` clears and JsonBuf::new() writes to beginning. |
| Integration | N/A |
| Performance | JsonBuf::new() zero heap allocation. push_byte in arena path: no alloc. push_u64: stack-only (itoa-style). push_f64_prec2: stack-only. Field::encode_into: no alloc for simple types, minimal alloc for Object/Array only if strings borrowed. **Benchmark**: encode a 2 KB monitoring report < 50 microseconds. |
| Code quality | `EncodeJson` trait must be implemented by all monitoring types. Trait method signature: `fn encode_json(&self, buf: &mut JsonBuf)`. No `serde` in hot path. `serde_json` may be used for parsing incoming JSON (server responses) — parsing is not hot path. |
| Cross-platform | Pure Rust, must compile on all 4 platforms. |
| Regression gate | **Wire compatibility gate**: JSON output must be byte-identical to Go agent output for the same metric values. Key JSON differences that would break compatibility: (1) float formatting precision, (2) field ordering, (3) string escaping, (4) null vs missing fields. |

### Task 1.5: `src/tls.rs` — TLS configuration

| Category | Criteria |
|----------|----------|
| Unit test | Test `build_client_config(insecure: bool)` returns valid rustls `ClientConfig`. Test that insecure mode skips certificate verification. Test that secure mode requires valid certs. |
| Integration | Connect to a real HTTPS endpoint (e.g., `https://httpbin.org`) to verify TLS handshake succeeds. Connect to `https://expired.badssl.com` to verify rejection (unless insecure mode). |
| Performance | Must use OS native root certificates (DD5). On Linux: read from `/etc/ssl/certs` or `/etc/ssl/certs/ca-certificates.crt`. On Windows: use CryptoAPI via `rustls-platform-verifier` or manual SCHANNEL FFI. On macOS: use Security.framework via `rustls-platform-verifier`. **No webpki-roots crate** (saves ~80 KB binary). |
| Code quality | No `unsafe` except for platform FFI to certificate stores. |
| Cross-platform | Must compile on all 4 platforms with platform-appropriate cert store access. |
| Regression gate | TLS configuration must match Go agent: `InsecureSkipVerify` config flag maps to `dangerous_configuration` on rustls. |

### Task 1.6: `src/ws.rs` — WebSocket connect + handshake

| Category | Criteria |
|----------|----------|
| Unit test | Test WS handshake request builder: method=GET, path=`/api/clients/v2/rpc?token=<token>`, headers: `Upgrade: websocket`, `Connection: Upgrade`, `Sec-WebSocket-Version: 13`, `Sec-WebSocket-Key` (16 random bytes, base64). Test `verify_handshake_response()`: valid 101 + correct `Sec-WebSocket-Accept` -> Ok(()), wrong accept key -> Err, non-101 status -> Err. Test `generate_key()` produces 16 random bytes, base64 encoded, length=24. |
| Integration | Connect to a real Komari server (or a dummy server that speaks WebSocket handshake). Verify: (1) 101 response received, (2) `Sec-WebSocket-Accept` validated, (3) first text frame sent successfully, (4) ping/pong works. |
| Performance | WebSocket frame codec must be hand-implemented per DD3 (NOT tungstenite). Frame format: FIN=1, opcode=1 (text), mask=1 (client->server always masked), payload length (7-bit or 16-bit or 64-bit extended), 4-byte masking key. Frame send < 100 lines. Frame receive (unmask only text frames, close frames, ping frames) < 150 lines. |
| Code quality | Per DD3: manual frame codec ~350 lines total. SHA-1 + base64 from `crypto.rs`. Frame masking key from OS CSPRNG (`getrandom` or `/dev/urandom`). |
| Cross-platform | Must compile on all 4 platforms. On Windows, `std::net::TcpStream` works identically. |
| Regression gate | **Wire compatibility**: WebSocket upgrade headers must be byte-identical to Go's `gorilla/websocket` output (same header order, same casing). Frame format must be RFC 6455 compliant. |

### Task 1.7: `src/http.rs` — HTTP POST client (stub for Phase 1)

| Category | Criteria |
|----------|----------|
| Unit test | Test HTTP/1.1 POST request builder: method=POST, path with query params, headers: `Host`, `Content-Type: application/json`, optional `Content-Encoding: gzip`, optional `CF-Access-*`. Test response parse: status code extraction, header extraction. |
| Integration | N/A in Phase 1 (stub). |
| Performance | Manual HTTP/1.1 POST per DD4 (~70 lines, NOT reqwest). Must use `std::net::TcpStream` over TLS from `tls.rs`. |
| Code quality | No `unsafe`. |
| Cross-platform | Must compile on all 4 platforms. |
| Regression gate | Not yet functional in Phase 1; placeholder only. |

### Task 1.8: `src/protocol/v2.rs` + `v1.rs` + `mod.rs` — JSON-RPC types

| Category | Criteria |
|----------|----------|
| Unit test | **v2 notification build**: `build_notification("agent.report", b"{}")` -> `{"jsonrpc":"2.0","method":"agent.report","params":{}}`. **v2 request build**: `build_request("id-1", "agent.pull", b"{}")` -> `{"jsonrpc":"2.0","method":"agent.pull","params":{},"id":"id-1"}`. **Report payload**: `build_report_payload(b"{...}")` must wrap in `{"report":{...}}` and then in v2 envelope. **V1 payload**: must be flat JSON without jsonrpc envelope, matching Go v1 format byte-for-byte. **Ack event ids**: verify JSON array formatting with 0, 1, multiple event IDs. **All 11 method constants**: verify string values match Go agent: `agent.report`, `agent.basicInfo`, `agent.pingResult`, `agent.taskResult`, `agent.exec`, `agent.ping`, `agent.message`, `agent.event`, `agent.terminal.request`, `agent.pull`. |
| Integration | N/A |
| Performance | All build functions use `Vec<u8>` with capacity pre-allocation. No re-allocation during build. |
| Code quality | Method constants as `&'static str`. `EncodeJson` impls for `Request`, `Event`, `EventResult`. |
| Cross-platform | Pure Rust. |
| Regression gate | **Wire compatibility**: JSON output must be byte-identical to Go agent for identical inputs. Test with recorded Go agent payloads. |

### Task 1.9: `src/server/mod.rs` — initial run() with static heartbeat

| Category | Criteria |
|----------|----------|
| Unit test | Test `run()` entry: config parsed, WS connected. Test static heartbeat JSON: `{"jsonrpc":"2.0","method":"agent.report","params":{"report":{"cpu":{"usage":0.0},"message":"heartbeat"}}}`. |
| Integration | Connect to a real Komari server. Verify: (1) WS upgrade succeeds, (2) server receives heartbeat, (3) server logs show agent connected. |
| Performance | First connect: TCP + TLS + WS upgrade < 5 seconds (with DNS). Heartbeat send: < 1 ms (WS frame encode + send). |
| Code quality | Single-threaded. No `spawn`, no `tokio`. `std::thread::sleep` for timing. |
| Cross-platform | Must compile on all 4 platforms. |
| Regression gate | Must connect to real Komari server cleanly. Server must not error on JSON format. |

### Task 1.10: `src/app.rs` + `src/main.rs` — entry point

| Category | Criteria |
|----------|----------|
| Unit test | Test `main()` exit code 0 on normal run. Test flag parsing: `--token abc` sets token, `--endpoint https://example.com` sets endpoint. Test help flag output. |
| Integration | N/A |
| Performance | `main.rs` < 15 lines (pure delegation). `app.rs` < 80 lines. |
| Code quality | `main()` returns `i32` exit code. No `process::exit()` except for self-update (exit 42). |
| Cross-platform | Must compile on all 4 platforms. |
| Regression gate | CLI interface must be identical to Go agent: same flag names (long form), same env var names, same defaults. |

### Phase 1 Gate Checklist

```
[ ] cargo build --release green on linux-amd64, windows-amd64, macos-amd64, freebsd-amd64
[ ] Binary size < 2 MB stripped on all platforms (Phase 1 threshold; final <1 MB by Phase 6)
[ ] Binary connects to real Komari server, completes WS handshake (101)
[ ] Server receives valid static heartbeat JSON
[ ] cargo test passes all tests (SHA-1 RFC vectors, base64 RFC vectors, JSON round-trip)
[ ] SHA-1 test vector: dGhlIHNhbXBsZSBub25jZQ== + GUID -> s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
[ ] rustfmt clean, clippy clean (with allowed lints)
[ ] No unsafe except FFI/platform modules
[ ] Git commit at Phase 1 boundary
```

---

## PHASE 2: Linux Metrics + Zero-Alloc Loop (~750 lines)

**Goal**: Full Linux monitoring suite. Scratch arena. Zero-allocation 1-second loop. Prove RSS < 3 MB.

### Task 2.1: `src/arena.rs` — ScratchArena + SmallVec

| Category | Criteria |
|----------|----------|
| Unit test | **ScratchArena**: (1) alloc 1 byte -> returns Some, (2) alloc 8192 bytes -> returns Some, (3) alloc 8193 bytes -> returns None, (4) alloc aligned types (u64 at offset 1 -> returns aligned pointer), (5) reset() -> subsequent alloc writes to beginning, (6) alloc after reset -> returns data, previous data overwritten (verify by writing after reset, reading back). **SmallVec**: (1) push 0..N elements -> all in inline storage, as_slice() returns correct order, (2) push N+1 -> spills to heap Vec, (3) as_slice() after spill returns all elements, (4) new() -> len=0. |
| Integration | N/A |
| Performance | Arena.alloc() < 5 CPU instructions (align + bounds check + bump). Arena.reset() = single `offset = 0` assignment. **Zero-alloc proof**: allocate and measure that no system malloc is called (verify via custom `#[global_allocator]` that panics/promotes during arena operations). |
| Code quality | Arena uses `UnsafeCell` internally but presents safe API. `// SAFETY:` comments on every UnsafeCell access explaining single-threaded invariant. |
| Cross-platform | Pure Rust. |
| Regression gate | N/A (new module). |

### Task 2.2: `src/platform/mod.rs` + `src/platform/linux.rs` — Type aliases

| Category | Criteria |
|----------|----------|
| Unit test | Verify `cfg(target_os)` resolves correctly on each platform: `platform::current` module exists and has expected types. |
| Integration | N/A |
| Performance | Zero-cost: type aliases are compile-time, no runtime dispatch. |
| Code quality | Each platform module file has exactly one `#[cfg(target_os = "...")]` gate at the module level. |
| Cross-platform | Must compile on all 4 platforms with correct platform module selected. |
| Regression gate | Type alias naming must match Go monitoring types: `CpuInfo`, `RamInfo`, `DiskInfo`, `LoadInfo`, etc. |

### Task 2.3: `src/monitor/cpu/linux.rs` — CPU collection

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: test with recorded `/proc/stat` and `/proc/cpuinfo` fixtures. Verify: (1) cpu_usage computation correct (delta from previous sample), (2) per-core counts, (3) model name extraction, (4) MHz extraction. **Edge cases**: (1) single-core system, (2) /proc/stat with guest/guest_nice fields (kernel 2.6.33+), (3) /proc/cpuinfo with tabs, mixed whitespace, (4) CPU name with special chars "Intel(R) Core(TM) i7-13700K". **First tick**: usage must be 0.0 (no previous sample). |
| Integration | Compare output against Go agent on same Linux host: same cpu_name, same cpu_cores, same cpu_physical_cores, cpu_usage within 0.5 percentage points. |
| Performance | File read via `std::fs::read_to_string` into arena buffer. No `String` allocation (borrow from arena). Parse with byte-level scanning (no regex). |
| Code quality | No unsafe. |
| Cross-platform | Linux only (`#[cfg(target_os = "linux")]`). Must compile on non-Linux (guarded). |
| Regression gate | **Wire compatibility**: CPU field names and structure must match Go agent exactly: `{"cpu":{"usage":12.5}}`. |

### Task 2.4: `src/monitor/mem/linux.rs` — Memory collection (3-mode)

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: test with recorded `/proc/meminfo` fixtures. Verify all 3 modes: (1) mode 0: used = total - MemAvailable, (2) mode 1: used = total - MemFree - Buffers - Cached, (3) mode 2: used = total - MemFree. **Swap**: SwapTotal, SwapFree, SwapCached extraction. **Edge cases**: (1) MemAvailable missing (kernel < 3.14) -> fallback to MemFree+Buffers+Cached, (2) 0 total RAM (should never happen, but test it), (3) swap disabled (SwapTotal=0). **CallFree fallback**: test `free -b` output parse (matches `CallFree()` in Go). |
| Integration | Compare output against Go agent on same Linux host for all 3 modes (reconfigure and restart). Verify used/total values match within 1% (rounding differences). |
| Performance | File read into arena. Byte-level parse (no regex). Zero heap allocation. |
| Code quality | 3-mode dispatch via config parameter (not compile-time). Must match Go `Ram()` exactly. |
| Cross-platform | Linux only. Shared `mem/mod.rs` has the 3-mode dispatch logic (cross-platform, tested as part of this task). |
| Regression gate | **Wire compatibility**: RAM/swap JSON fields must match Go agent: `{"ram":{"total":N,"used":N},"swap":{"total":N,"used":N}}`. Numbers must match Go agent on same host for same mode. |

### Task 2.5: `src/monitor/disk/linux.rs` — Disk collection

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: test with recorded `/proc/mounts` fixture + mock `statvfs` results. Verify: (1) physical disk filter: exclude all 30+ patterns from Appendix C (loop, sys, proc, run, snap, docker, overlay, tmpfs, devtmpfs, cgroup, cgroup2, pstore, bpf, debugfs, tracefs, fusectl, configfs, securityfs, hugetlbfs, devpts, mqueue, binfmt_misc, squashfs, ramfs, aufs, devfs, autofs, efivarfs, nfs, nfs4, cifs, smbfs, rpc_pipefs, kubelet), (2) total = sum of all physical mounts, (3) used = sum of all physical mounts. **Edge cases**: (1) zero physical disks (only virtual FS), (2) mount with spaces in path (should be excluded), (3) very long mount list. |
| Integration | Compare mount list against Go agent on same host: same devices, same total/used bytes. |
| Performance | `is_physical_disk()` checks against static string list (no regex, no dynamic allocation). `statvfs` per mount via unsafe FFI (platform syscall). |
| Code quality | The 30+ exclude patterns must be maintained as a `const &[&str]` slice. Must match Go's exact list. |
| Cross-platform | Linux only. Shared `disk/mod.rs` has `is_physical_disk()` dispatch (Linux-specific implementation in linux.rs). |
| Regression gate | **Wire compatibility**: Disk field: `{"disk":{"total":N,"used":N}}`. Same mounts, same totals as Go agent. |

### Task 2.6: `src/monitor/net/linux.rs` — Network collection

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: test with recorded `/proc/net/dev` fixture. Verify: (1) RX/TX bytes per interface, (2) speed = delta from previous sample / interval, (3) first tick speed = 0 (no previous sample), (4) NIC include/exclude filter (wildcard matching), (5) connection count from `/proc/net/tcp` + `/proc/net/tcp6`. **Edge cases**: (1) interface with no traffic, (2) interface with only RX or only TX, (3) loopback interface excluded by default, (4) virtual interfaces (docker, tun, veth) filtered per config. |
| Integration | Compare network speed values against Go agent on same host (simultaneous runs): up/down within 10% (timing differences). |
| Performance | Delta calculation: store previous sample in Monitor struct (u64 fields). No allocation. |
| Code quality | NIC filter: `include_nics` and `exclude_nics` config strings (comma-separated, wildcard support). Must match Go's wildcard matching exactly (`*` matches any substring). |
| Cross-platform | Linux only. |
| Regression gate | **Wire compatibility**: Network JSON: `{"network":{"up":F,"down":F,"totalUp":N,"totalDown":N}}`. Delta calculation must match Go agent logic (speed = (current_rx - prev_rx) / interval_seconds). |

### Task 2.7: `src/monitor/load/linux.rs` — Load average

| Category | Criteria |
|----------|----------|
| Unit test | Test with `/proc/loadavg` fixture: `"0.25 0.32 0.28 1/456 12345\n"` -> load1=0.25, load5=0.32, load15=0.28. **Edge cases**: (1) high load >100, (2) fields separated by multiple spaces (should not happen but parse gracefully). |
| Integration | Compare against Go agent on same host: all 3 values match to 2 decimal places. |
| Performance | Single file read, 3 float parses. Stack-only. |
| Code quality | Trivial. < 35 lines. |
| Cross-platform | Linux only. |
| Regression gate | **Wire compatibility**: `{"load":{"load1":F,"load5":F,"load15":F}}`. Values match Go agent. |

### Task 2.8: `src/monitor/connections/linux.rs` — Connection count

| Category | Criteria |
|----------|----------|
| Unit test | Test with `/proc/net/tcp` + `/proc/net/tcp6` fixtures. Count entries (excluding header line). Test empty files. Test files with only header. |
| Integration | Compare against Go agent: tcp_count, udp_count. (Note: Go only counts TCP; connections.rs may also count UDP if server supports it.) |
| Performance | Byte-level scan: count newlines - 1. No allocation. |
| Code quality | < 30 lines. |
| Cross-platform | Linux only. |
| Regression gate | **Wire compatibility**: `{"connections":{"tcp":N,"udp":N}}`. |

### Task 2.9: `src/monitor/process/linux.rs` — Process count

| Category | Criteria |
|----------|----------|
| Unit test | Test with mock `/proc` directory structure. Verify: PID counting (only numeric directory names), running process count from `/proc/<pid>/status` State field. **Edge cases**: (1) empty /proc, (2) non-numeric entries (self, thread-self, fs, etc.) excluded, (3) /proc/<pid>/status missing (process exited between readdir and read). |
| Integration | Compare process count and running count against Go agent on same host. |
| Performance | `std::fs::read_dir` + filename parse. No heap for path construction (use arena-borrowed scratch). |
| Code quality | < 40 lines. |
| Cross-platform | Linux only. |
| Regression gate | Values match Go agent: total process count, running process count. |

### Task 2.10: `src/monitor/uptime/linux.rs` — Uptime

| Category | Criteria |
|----------|----------|
| Unit test | Test with `/proc/uptime` fixture: `"86400.00 12345.67\n"` -> uptime=86400. **Edge cases**: (1) very large uptime (years), (2) missing file, (3) single field. |
| Integration | Compare against Go agent on same host (within 1 second drift due to collection timing). |
| Performance | Single file read, one float parse, convert to u64 seconds. |
| Code quality | < 25 lines. |
| Cross-platform | Linux only. |
| Regression gate | `{"uptime":N}` matches Go agent. |

### Task 2.11: `src/monitor/mod.rs` — Monitor struct + tick() orchestrator

| Category | Criteria |
|----------|----------|
| Unit test | Test `Monitor::new()` initializes with zero state. Test `tick()` integration: call tick twice, verify second call has non-zero cpu_usage, non-zero net speed. Test `Monitor` struct contains: arena, prev_net_rx, prev_net_tx, total_up, total_down, boot_time_secs. |
| Integration | N/A (tested via individual collector integration tests). |
| Performance | **Zero-alloc proof**: run `monitor.tick()` with a custom `#[global_allocator]` that panics on any allocation. Tick must complete without triggering the panic. Verify via `dhat` heap profiler: zero allocations in tick call tree. **Tick timing**: complete tick < 10 ms on idle 4-core system. **Arena**: reset at start of each tick. After tick, arena cursor < 4096 (well within 8192 capacity). |
| Code quality | Collectors called in fixed order. Each collector receives `&Config` or `&mut Monitor` (for stateful collectors). Arena borrowed via `&mut ScratchArena` — only one live borrow at a time per collector. |
| Cross-platform | Linux only in Phase 2. Dispatch stubs for other platforms return zero/default values (compiles but does nothing on non-Linux). |
| Regression gate | JSON output from `tick()` must be valid JSON. All field names must match Go agent. |

### Phase 2 Gate Checklist

```
[ ] All Linux collectors produce JSON identical in structure to Go agent (field names, types, nesting)
[ ] 3-mode RAM calculation matches Go output (modes 0, 1, 2) on same host
[ ] is_physical_disk() produces same mount list as Go on same host
[ ] Zero heap allocation in monitor.tick() hot path (verified via dhat or custom allocator)
[ ] RSS < 3 MB after 60s of running (measured via /proc/<pid>/status VmRSS)
[ ] 1-second tick jitter < 50ms under idle system
[ ] cargo test passes all collector unit tests with fixture /proc data
[ ] rustfmt clean, clippy clean
[ ] Git commit at Phase 2 boundary
```

---

## PHASE 3: Protocol FSM + Fallback (~500 lines)

**Goal**: v2/v1 protocol negotiation. HTTP POST fallback. Exponential backoff. Exec/ping stubs. **Recorded session replay passes.**

### Task 3.1: `src/protocol/fsm.rs` — FallbackFsm + ConnectionFsm

| Category | Criteria |
|----------|----------|
| Unit test | **All 12 FSM transitions** (from Appendix B.5): (1) V2WebSocket + success -> V2WebSocket, counter reset, (2) V2WebSocket + error (strike 1) -> V2WebSocket, counter=1, (3) V2WebSocket + error (strike 2) -> V2HttpPost, counter=2, (4) V2WebSocket + error (strike 3) -> V1HttpPost, counter=3, (5) V2HttpPost + success -> V2WebSocket, reset, (6) V2HttpPost + error -> V1HttpPost, counter=3, (7) V1HttpPost + success (any) -> V2WebSocket, reset, (8) V1HttpPost + error -> V1HttpPost (stays), (9) Any (Connected) + disconnect -> Disconnected, (10) Disconnected + reconnect v2 success -> V2WebSocket, (11) Disconnected + reconnect v1 success -> V1HttpPost, (12) Disconnected + reconnect while v1 active -> V1HttpPost. **Network error vs protocol error**: verify that `record_failure(false)` does NOT increment counter. **Reset**: `reset()` -> V2WebSocket, counter=0. **Config**: `new(1)` -> starts in V1HttpPost. `new(2)` -> V2WebSocket. |
| Integration | N/A |
| Performance | FSM transitions are pure match arms on Copy enums. Zero allocations. < 120 lines. |
| Code quality | No unsafe. Enums `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`. |
| Cross-platform | Pure Rust. |
| Regression gate | **Recorded session replay**: Go agent's protocol negotiation sequence must be replayable against this FSM. The FSM must reach the same state as Go agent for the same sequence of events. |

### Task 3.2: `src/server/backoff.rs` — Backoff with jitter

| Category | Criteria |
|----------|----------|
| Unit test | **Base sequence**: backoff(1s, 300s): next_delay() ≈ 1s (with jitter), next_delay() ≈ 2s, next_delay() ≈ 4s, ... after 10 calls, delay capped at 300s. **Jitter bounds**: for 100 calls with deterministic seed, all delays within +/- 25% of base. **Reset**: after reset(), next_delay() ≈ 1s again. **Seed determinism**: same seed produces same sequence. **Min/max**: initial 1ms, max 1ms -> all delays ≈ 1ms. |
| Integration | N/A |
| Performance | XorShift PRNG: no `std::rand`, no external crate. < 50 lines. |
| Code quality | Jitter +/- 25% must match Go agent's jitter logic exactly. |
| Cross-platform | Pure Rust. |
| Regression gate | Backoff timing must produce the same statistical distribution as Go agent (deterministic PRNG with same seed). |

### Task 3.3: `src/server/reconnection.rs` — Reconnection loop

| Category | Criteria |
|----------|----------|
| Unit test | **Select-style polling**: test that data tick fires at interval, heartbeat fires at 30s, read timeout returns. **Reconnect trigger**: on WS error, verify backoff.next_delay() called, then reconnect. **Max retries**: after max_retries attempts, agent logs fatal and exits. **Timer precision**: data tick interval within 50ms of configured interval. |
| Integration | Test reconnection against real Komari server: (1) start agent, (2) kill server, (3) agent detects disconnect within 30s (heartbeat timeout), (4) agent reconnects when server restarts, (5) backoff increases across failures, resets on success. |
| Performance | Non-blocking socket read with timeout (poll/select). No busy-wait. Sleep between ticks (not spin). |
| Code quality | Match Go's select pattern: dataTicker, heartbeatTicker, readDone. In sync Rust: `thread::sleep(interval)` for data tick, track wall-clock for heartbeat. |
| Cross-platform | Must compile on all 4 platforms. On Windows: `select()` instead of `poll()` (POSIX poll not available on Windows sockets). Thin `Poller` abstraction. |
| Regression gate | Reconnection behavior must match Go agent: same heartbeat interval (30s), same data tick interval (configurable, default 1s), same backoff cap (5 min). |

### Task 3.4: `src/http.rs` (expand) — Full HTTP POST fallback

| Category | Criteria |
|----------|----------|
| Unit test | Test `post_json(endpoint, body, timeout)`: builds correct HTTP/1.1 POST, includes all required headers: `Host`, `Content-Type: application/json`, optional `Content-Encoding: gzip`, optional `CF-Access-Client-Id`, `CF-Access-Client-Secret`. Test response parse: status 200 -> Ok, 401 -> auth error, 500 -> server error. Test timeout: response exceeds timeout -> timeout error. |
| Integration | POST to real Komari server `/api/clients/v2/rpc` endpoint. Verify server receives identical JSON to WebSocket mode. |
| Performance | POST body reuses arena buffer (copy to heap Vec for send). Connection: TCP + TLS (if HTTPS). No persistent connection pool (one POST per fallback cycle). |
| Code quality | Manual HTTP/1.1 per DD4. < 70 lines. |
| Cross-platform | Must compile on all 4 platforms. |
| Regression gate | **Wire compatibility**: HTTP POST body must be byte-identical to WS text frame body for the same monitoring data. Same JSON, same Content-Type. |

### Task 3.5: `src/server/task.rs` — stub exec + ping handlers

| Category | Criteria |
|----------|----------|
| Unit test | Test `handle_exec("ls -la")` returns stub result with task_id. Test `handle_ping("icmp", "8.8.8.8")` returns stub result with ping_type and task_id. Test task_id parsing from incoming JSON-RPC event. |
| Integration | N/A in Phase 3 (stub). |
| Performance | N/A (stub). |
| Code quality | Stub returns well-formed JSON matching Go `agent.taskResult` and `agent.pingResult` format. |
| Cross-platform | Must compile on all 4 platforms. |
| Regression gate | Stub responses must parse correctly by Komari server. Task ID format must match Go agent (string or u64 depending on ping type). |

### Task 3.6: `src/protocol/v2.rs` (expand) — BuildReportPayload et al.

| Category | Criteria |
|----------|----------|
| Unit test | `build_report_payload(json)`: enveloped in `{"report":<json>}` then in v2 notification envelope. `build_report_request(id, json, ack_ids)`: includes `ack_event_ids` array. `build_basic_info_payload(json)`: enveloped in `{"info":<json>}`. `build_ping_result(task_id, type, value, finished_at)`: formatted correctly. |
| Integration | Send built payload to real Komari server. Verify server accepts and parses correctly. |
| Performance | Pre-allocated Vec with exact capacity (known sizes). |
| Code quality | All builders return `Vec<u8>`. No serde_json::Value in build path (use `EncodeJson` trait or direct byte pushing). |
| Cross-platform | Pure Rust. |
| Regression gate | **Wire compatibility**: JSON output must be byte-identical to Go agent for same inputs. |

### Task 3.7: `src/server/mod.rs` (expand) — Integrate FSM + reconnection + backoff

| Category | Criteria |
|----------|----------|
| Unit test | Test full lifecycle: (1) connect v2 WS -> (2) send heartbeat -> (3) inject v2 protocol error -> (4) verify strike 1 -> (5) inject 2 more errors -> (6) verify v1 HTTP fallback -> (7) inject success -> (8) verify return to v2 WS. |
| Integration | **Recorded session replay**: Capture Go agent's full session (connect, heartbeat x N, disconnect) using tcpdump or mitmproxy. Replay server-side responses to Rust agent. Compare Rust agent's output frames byte-for-byte with Go agent's output frames. This is the **ultimate wire compatibility proof**. |
| Performance | End-to-end tick: collect + encode + send < 10ms (excluding network latency). |
| Code quality | Integration of all Phase 1-3 modules: config + WS + HTTP + FSM + backoff + reconnection + monitor + protocol. |
| Cross-platform | Must compile and function on all 4 platforms (Linux primary; others with stub monitors). |
| Regression gate | **Regression test**: Re-run Phase 1 heartbeat test. Re-run Phase 2 monitor correctness test. Both must still pass. |

### Phase 3 Gate Checklist

```
[ ] FSM transitions verified: connect -> WS fail x3 -> HTTP POST -> v1 fallback
[ ] 3-strike counter: 3 consecutive v2 failures -> v1; 1 success resets counter
[ ] HTTP POST fallback sends identical JSON body to WS mode
[ ] Backoff: ~1s after 3 failures; caps at 5 min after 10 failures
[ ] Reconnection survives server restart (agent reconnects within backoff window)
[ ] cargo test includes FSM transition table test (all 12 edges)
[ ] Recorded session replay: Go agent session replayed against Rust agent, JSON matches byte-for-byte
[ ] rustfmt clean, clippy clean
[ ] Git commit at Phase 3 boundary
```

---

## PHASE 4: Cross-Platform Metrics (~1,535 lines)

**Goal**: Windows/macOS/FreeBSD metrics. GPU across 4 platforms. OS + virtualization detection. CI fully green on all 4 platforms.

### Task 4.1: Platform gate files (windows.rs, macos.rs, freebsd.rs)

| Category | Criteria |
|----------|----------|
| Unit test | Verify `#[cfg(target_os)]` gates compile the correct platform module on each host. On wrong target, module is empty (no compile error for missing types — use stub or conditional compilation). |
| Integration | N/A |
| Performance | Zero-cost: all platform dispatch resolved at compile time. |
| Code quality | Each platform file has function signatures identical to Linux counterparts. `platform::current::collect_cpu(...)` same signature on all platforms. |
| Cross-platform | Each platform module must compile on its target OS. CI must verify. |
| Regression gate | Linux monitor still works after adding other platform modules. |

### Task 4.2: Windows collectors (cpu, mem, disk, net, load, connections, process, uptime, ip)

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based where possible**: (1) CPU: mock `GetSystemInfo` + registry reads, (2) Memory: mock `GlobalMemoryStatusEx` + `GetPerformanceInfo`, (3) Disk: mock `GetLogicalDrives` + `GetDiskFreeSpaceExW`, (4) Process: mock `K32EnumProcesses`, (5) Network: mock `GetIfTable2` + `GetTcpTable2`, (6) Uptime: mock `GetTickCount64`. **3-mode memory**: same as Linux, must pass same mode logic tests with Windows-sized numbers. **IP detection**: mock `GetAdaptersAddresses`. **Edge cases**: (1) zero network interfaces, (2) >26 drives, (3) system with 0 available memory (should not happen), (4) uptime overflow `GetTickCount64` (49.7 days for 32-bit, effectively never for 64-bit). |
| Integration | Compare output against Go agent on same Windows host: all collector values within 1%. **GPU VRAM values**: run Go and Rust side-by-side, compare `memory_total` and `memory_used`. If DXGI reports different values, document the difference and add `gpu_mem_mode` config flag. |
| Performance | All Win32 API calls are FFI. No heap allocation in hot path (results stored in stack structs). |
| Code quality | Use `windows-rs` crate for Win32 API (DD11). Not raw COM vtables. Each `unsafe` FFI call must have `// SAFETY:` comment. |
| Cross-platform | Windows only (`#[cfg(target_os = "windows")]`). Must not compile on non-Windows (or compile with errors — prefer compile error for missing windows-rs). |
| Regression gate | **Wire compatibility**: Windows JSON output must match Go agent's Windows JSON output in structure (same field names, same JSON types). |

### Task 4.3: macOS collectors (cpu, mem, disk, net, load, connections, process, uptime, ip)

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: (1) CPU: mock `sysctl hw.logicalcpu` / `machdep.cpu.brand_string`, (2) Memory: mock `sysctl hw.memsize` + `host_statistics64`, (3) Disk: mock `getmntinfo` + `statfs` results, (4) Load: mock `getloadavg()`, (5) Process: mock `sysctl kern.proc.all`, (6) Uptime: mock `sysctl kern.boottime`, (7) IP: mock `getifaddrs`. |
| Integration | Compare output against Go agent on same macOS host for available collectors. Note: Go agent macOS support may be limited (check `monitoring/unit/` for darwin build tags). |
| Performance | All sysctl calls are FFI. Stack-allocated results. |
| Code quality | Use `libc` crate for sysctl and Mach APIs. `unsafe` FFI with `// SAFETY:` comments. Feature-gate: `#[cfg(target_os = "macos")]`. |
| Cross-platform | macOS only. |
| Regression gate | **Wire compatibility**: macOS JSON structure must match Go agent's darwin output. |

### Task 4.4: FreeBSD collectors (cpu, mem, disk, net, load, connections, process, uptime, ip)

| Category | Criteria |
|----------|----------|
| Unit test | **Fixture-based**: FreeBSD uses `sysctl` heavily. Mock sysctl results for all collectors. (1) CPU: `hw.model`, `hw.ncpu`, `kern.cp_times`, (2) Memory: `hw.physmem`, `hw.usermem`, `kvm_getswapinfo` via libc KVM interface, (3) Disk: `getmntinfo` + `statfs`, (4) Load: `getloadavg()`, (5) Process: `sysctl kern.proc.all`, (6) Uptime: `sysctl kern.boottime`, (7) IP: `getifaddrs`. |
| Integration | Compare output against Go agent on same FreeBSD host if available. Cross-compilation CI ensures build correctness; runtime testing via QEMU VM or Cirrus CI. |
| Performance | sysctl + libc FFI. Stack-allocated. |
| Code quality | Use `libc` crate for sysctl and KVM. `#[cfg(target_os = "freebsd")]`. |
| Cross-platform | FreeBSD only. |
| Regression gate | FreeBSD JSON structure matches Go agent's freebsd output. |

### Task 4.5: `src/monitor/gpu/` — GPU detection (all 4 platforms)

| Category | Criteria |
|----------|----------|
| Unit test | **Linux**: (1) Parse nvidia-smi CSV output: `name, temperature.gpu, utilization.gpu, memory.used, memory.total` -> verify correct field extraction, (2) Parse rocm-smi key-scanner output, (3) Parse `/sys/class/drm/card0/device/vendor` = `0x10de` (NVIDIA), (4) Test nvidia-smi not available -> DRM fallback (presence only, no detailed metrics), (5) Test rocm-smi not available -> DRM fallback. **Windows**: Test DXGI adapter enumeration mock. Verify: adapter count, name extraction, VRAM (DedicatedVideoMemory), shared memory. **macOS**: Parse `system_profiler SPDisplaysDataType -json` mock. **FreeBSD**: Parse `pciconf -lv` mock output. **Edge cases**: (1) zero GPUs, (2) multi-GPU (2, 4, 8 GPUs), (3) GPU names with special characters, (4) VRAM values at 32-bit boundary (4 GB = 4294967296), (5) 0% utilization, (6) 100% utilization, (7) temperature sensor absent (nvidia-smi reports [Not Supported]). |
| Integration | **Cross-validation with Go agent on same host**: (1) Same GPU names, (2) Same VRAM total (within 1%), (3) Same full JSON structure. |
| Performance | External process call (`nvidia-smi`, `rocm-smi`, `system_profiler`, `pciconf`). Not hot path — called once per basicInfo upload (every 30 min) and optionally per tick (when `enable_gpu=true`). Process call timeout: 5 seconds. |
| Code quality | GPU detection is feature-gated: `gpu-detection` feature (DD13). Without feature, returns empty GPU report. `unsafe` for DXGI COM FFI on Windows (if not using windows-rs for DXGI). Each ff-block: `// SAFETY:` comment. |
| Cross-platform | All 4 platforms, each with native detection. |
| Regression gate | **Wire compatibility**: GPU JSON structure must match Go agent exactly: `{"gpu":{"count":N,"average_usage":F,"detailed_info":[{...}]}}`. GPU names must be the same strings as Go agent on the same hardware. |

### Task 4.6: `src/monitor/os.rs` — OS name + kernel version

| Category | Criteria |
|----------|----------|
| Unit test | **Linux**: Parse `/etc/os-release` fixture. Verify: (1) Ubuntu 22.04 -> `os_name = "Ubuntu 22.04.3 LTS"`, (2) Debian 12 -> `os_name = "Debian GNU/Linux 12 (bookworm)"`, (3) Android (build.prop) -> `os_name = "Android ..."`, (4) Synology -> `os_name = "Synology DSM ..."`, (5) Proxmox VE -> `os_name = "Proxmox VE ..."`, (6) fnOS -> `os_name = "fnOS ..."`, (7) unknown -> `os_name = "Linux"`. **Windows**: Registry-based (mock). **macOS**: Parse `sw_vers` output. **FreeBSD**: Parse `uname -r` + `freebsd-version`. **Kernel version**: `uname -r` on all platforms. |
| Integration | Verify OS name matches Go agent on same host. |
| Performance | One-time at startup (not per tick). Acceptable to allocate Strings. |
| Code quality | OS-specific platform dispatch via `#[cfg(target_os)]`. < 115 lines total across all platforms. |
| Cross-platform | Must correctly identify all 4 platforms. |
| Regression gate | OS name strings must match Go agent's output exactly (including version formatting). |

### Task 4.7: `src/monitor/virtualization.rs` — VM/container detection

| Category | Criteria |
|----------|----------|
| Unit test | **Container**: (1) `/proc/self/cgroup` containing "docker" -> container=docker, (2) containing "kubepods" -> container=k8s, (3) containing "lxc" -> container=lxc, (4) `/.dockerenv` exists -> container=docker, (5) no indicators -> not container. **VM**: (1) CPUID hypervisor bit set -> VM detected, (2) DMI product_name = "KVM" -> kvm, (3) "VMware Virtual Platform" -> vmware, (4) "VirtualBox" -> virtualbox, (5) "Virtual Machine" -> hyper-v. **macOS**: `sysctl kern.hv_support` = 1 -> running in VM. **Edge cases**: (1) container-inside-VM (both detected), (2) WSL2 detection (special case), (3) bare metal (no detection). |
| Integration | Compare against Go agent on same host: same virtualization string (docker, kvm, vmware, hyper-v, or empty). |
| Performance | One-time at startup. CPUID via `std::arch::x86_64::__cpuid` on x86_64; on aarch64, skip CPUID (no hypervisor bit detection — rely on DMI/filesystem heuristics). |
| Code quality | Use `#[cfg(target_arch = "x86_64")]` for CPUID. < 85 lines. |
| Cross-platform | Linux, Windows, macOS, FreeBSD. Windows: use `__cpuid` intrinsic. macOS: `sysctl kern.hv_support`. |
| Regression gate | Virtualization string must exactly match Go agent (same values, same format). |

### Task 4.8: CI expansion — full 4-platform matrix

| Category | Criteria |
|----------|----------|
| Unit test | CI config itself is declarative. Verify via `act` or push-to-branch. |
| Integration | All CI jobs must be green. |
| Performance | CI runtime < 15 minutes per platform. |
| Code quality | `.github/workflows/ci.yml`: matrix with 4 OS targets. Steps: checkout, install Rust, `cargo test`, `cargo build --release`, `cargo clippy -- -D warnings`, `cargo fmt --check`. |
| Cross-platform | Ubuntu (x86_64-unknown-linux-gnu), Windows (x86_64-pc-windows-msvc), macOS (x86_64-apple-darwin, aarch64-apple-darwin), FreeBSD (x86_64-unknown-freebsd via cross-compile or Cirrus CI). |
| Regression gate | All Phase 1-3 tests must still pass on all platforms. |

### Phase 4 Gate Checklist

```
[ ] All 4 platform binaries pass cargo test in CI
[ ] GPU detection produces same GPU names as Go agent on each platform
[ ] Memory 3-mode: validated against Go on Windows
[ ] OS detection: correctly identifies Ubuntu 22.04, Debian 12, Windows 11, macOS 15, FreeBSD 14
[ ] Virtualization: detects Docker, KVM, VMware, Hyper-V
[ ] CI matrix: 4 OS x (build + test + clippy + fmt) all green
[ ] No platform-specific code in shared modules (all behind cfg(target_os))
[ ] rustfmt clean, clippy clean
[ ] Git commit at Phase 4 boundary
```

---

## PHASE 5: Terminal + Ping + Gzip + DNS + Update (~1,045 lines)

**Goal**: Full terminal PTY/ConPTY. ICMP/TCP/HTTP ping. Gzip compression. DNS resolver. Self-update. Full integration test.

### Task 5.1: `src/gzip.rs` — Fixed-Huffman DEFLATE encoder

| Category | Criteria |
|----------|----------|
| Unit test | **CRC32**: (1) `""` -> 0x00000000, (2) `"123456789"` -> 0xCBF43926, (3) `"\x00"` -> 0xD202EF8D, (4) `"\xFF"` -> 0xFF000000, (5) `"hello world"` -> 0x0D4A1185. **Gzip round-trip**: `gzip_bytes(b'{"cpu":{"usage":12.5}}')` -> output gunzip-able, produces identical JSON. **Fixed Huffman tables**: verify code lengths match RFC 1951 Section 3.2.6. **Boundary cases**: (1) empty input -> valid gzip, (2) single byte input, (3) 64 KB input (maximum LZ77 window), (4) input with all possible byte values (0x00-0xFF), (5) highly repetitive input (LZ77 back-references). **BitWriter**: (1) bit-level write correctness, (2) byte alignment at block end. **Stored blocks**: payload < 200 bytes -> should use BTYPE=00 (stored), verify. |
| Integration | Compress a 2 KB monitoring payload -> send to real Komari server with `Content-Encoding: gzip`. Server must accept and parse correctly. |
| Performance | Gzip encode of 2 KB JSON < 500 microseconds. Fixed Huffman: no tree construction, use pre-computed code tables. LZ77 hash-chain matcher: 32 KB sliding window. Total gzip module < 200 lines per DD9. |
| Code quality | No `unsafe`. Pure Rust. Pre-computed Huffman tables as `const` arrays. CRC32 table as `const` array (1 KB). |
| Cross-platform | Pure Rust, all platforms. |
| Regression gate | **Wire compatibility**: gzip output must be accepted by Komari server's Go `compress/gzip` reader. gunzip must produce bit-identical original JSON. |

### Task 5.2: `src/dns.rs` — Custom DNS resolver

| Category | Criteria |
|----------|----------|
| Unit test | **DNS query build**: (1) build A query for "example.com" -> correct QNAME encoding (length-prefixed labels), correct QTYPE=1, QCLASS=1, (2) build AAAA query -> QTYPE=28, (3) query ID is random 16-bit. **DNS response parse**: (1) parse A response (single IP), (2) parse AAAA response, (3) parse response with CNAME chain, (4) parse NXDOMAIN response -> empty result, (5) parse truncated response -> fallback to TCP. **Cache**: (1) resolve once -> cached, (2) resolve again -> cache hit (no UDP send), (3) TTL expired -> re-resolve, (4) cache max 5 min TTL cap. **Server list**: test each of 10 built-in DNS servers resolves. **IPv4/IPv6 preference**: (1) prefer_ipv4=true -> return A records first, (2) prefer_ipv6=true -> return AAAA records first. |
| Integration | Resolve real hostnames against real DNS servers. Verify: (1) `dns.google` resolves, (2) custom DNS server config works, (3) TTL honored, (4) timeouts handled gracefully (2s per server). |
| Performance | UDP socket: one send + recv per DNS server. Cache hit: O(1) HashMap lookup (or linear scan for small cache, <50 entries). < 165 lines. |
| Code quality | DNS query/response built from raw bytes (no external DNS library). Match Go agent's DNS server list exactly (`[2606:4700:4700::1111]:53`, etc., from Appendix D). |
| Cross-platform | Must compile and work on all 4 platforms. On Windows: UDP socket via `std::net::UdpSocket` (works). |
| Regression gate | DNS resolution must produce the same IP addresses as Go agent for the same hostname + server combination. |

### Task 5.3: `src/terminal/` — PTY (Unix) + ConPTY (Windows)

| Category | Criteria |
|----------|----------|
| Unit test | **Unix PTY**: (1) open PTY master -> get fd, (2) grantpt/unlockpt -> slave name, (3) fork + exec "echo hello" -> read "hello\n" from master, (4) resize pty -> verify `TIOCSWINSZ` ioctl, (5) close master -> waitpid child exits. **Windows ConPTY**: (1) `CreatePseudoConsole` with COORD size, (2) `CreateProcess` with `STARTUPINFOEX` + `PPROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, (3) write "dir\r\n" -> read output, (4) `ResizePseudoConsole` -> resize, (5) close -> `TerminateProcess`. **Edge cases**: (1) executable not found, (2) command fails (exit code != 0), (3) very long output (>64 KB), (4) binary output (non-UTF8). |
| Integration | Test with real Komari server terminal protocol: (1) server sends `agent.terminal.request`, (2) agent opens PTY/ConPTY, (3) Terminal WebSocket connects, (4) input/output flows bidirectionally, (5) terminal close -> WS close -> process exits. |
| Performance | Feature-gated: `terminal` feature (DD13). Without feature, terminal module compiles to empty. PTY fork: process creation overhead (OS-dependent, not in hot path). I/O buffers: 16 KB per direction. |
| Code quality | Unix: `unsafe` for `fork()`, `execvp()`, `ioctl()`, signal handling. Each block: `// SAFETY:` comment. Windows: `unsafe` for ConPTY API, `STARTUPINFOEX`, overlapped I/O. Each block: `// SAFETY:` comment. |
| Cross-platform | Unix (Linux + macOS + FreeBSD): `posix_openpt`. Windows: `CreatePseudoConsole` (10 1809+). Old Windows: runtime detection -> error "ConPTY requires Windows 10 1809+". |
| Regression gate | Terminal behavior must match Go agent: (1) same WebSocket sub-protocol, (2) same JSON message format for stdin/stdout/resize, (3) same graceful shutdown (SIGHUP on Unix, TerminateProcess on Windows), (4) same exit code reporting. |

### Task 5.4: `src/server/ping_icmp.rs` + `ping_tcp.rs` + `ping_http.rs` — Ping

| Category | Criteria |
|----------|----------|
| Unit test | **ICMP**: (1) build ICMP echo request (type=8, code=0) with correct checksum, (2) parse ICMP echo reply (type=0), (3) measure RTT correctly, (4) timeout after N seconds returns error. **TCP**: (1) connect to open port -> RTT measured, (2) connect to closed port -> error "connection refused", (3) connect timeout -> error. **HTTP**: (1) GET to httpbin.org/status/200 -> 200, (2) GET to httpbin.org/status/404 -> 404 (not error), (3) timeout -> error, (4) invalid URL -> error. **Ping type dispatch**: `handle_ping("icmp", target)`, `handle_ping("tcp", target)`, `handle_ping("http", target)`. |
| Integration | On Linux with CAP_NET_RAW: `icmp ping 8.8.8.8` -> RTT < 500ms. TCP ping to `google.com:443` -> RTT measured. HTTP ping to `https://google.com` -> 200. On Linux without CAP_NET_RAW: ICMP returns permission error, TCP/HTTP still work. |
| Performance | Feature-gated: `ping` feature (DD13). ICMP: one sendto + recvfrom per ping. TCP: `TcpStream::connect_timeout`. HTTP: manual HTTP/1.1 GET over TLS. |
| Code quality | ICMP: `unsafe` for raw socket creation + checksum computation. `// SAFETY:` comments. Windows ICMP: `IcmpSendEcho2` via iphlpapi FFI. |
| Cross-platform | All 4 platforms. Linux: raw socket (needs CAP_NET_RAW). Windows: `IcmpSendEcho2`. macOS: raw socket (needs root). FreeBSD: raw socket. |
| Regression gate | **Wire compatibility**: Ping result format `agent.pingResult` must match Go agent: `{"task_id":N,"ping_type":"icmp","value":23,"finished_at":"..."}`. Value = -1 on failure. finished_at = RFC 3339 nano. |

### Task 5.5: `src/server/cf_access.rs` — Cloudflare Access headers

| Category | Criteria |
|----------|----------|
| Unit test | Test header injection: when `cf_access_client_id` and `cf_access_client_secret` are set, WS upgrade and HTTP POST include `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers. When not set, headers absent. |
| Integration | Test with real CF Access-protected Komari server. Agent must connect successfully through CF Access. |
| Performance | Header lookup: `&str` comparison (config fields are empty or populated). No allocation. |
| Code quality | < 55 lines. |
| Cross-platform | Pure Rust (header manipulation). |
| Regression gate | Headers must match Go agent's CF Access header format exactly (same header names, same values). |

### Task 5.6: `src/task.rs` — ExecTask + PingTask types

| Category | Criteria |
|----------|----------|
| Unit test | Test `ExecTask` deserialization from JSON-RPC event: `task_id`, `command` fields. Test `PingTask` deserialization: `ping_task_id`, `ping_type`, `ping_target`. Test task result upload format: `agent.taskResult` JSON. |
| Integration | N/A (types only). |
| Performance | Stack-allocated Copy types where possible (ping task). Exec task may need String for command. |
| Code quality | < 40 lines. |
| Cross-platform | Pure Rust. |
| Regression gate | JSON field names must match Go agent: `task_id` (not `taskId`), `ping_task_id`, `ping_type`, `ping_target`. |

### Task 5.7: `src/server/task.rs` (expand) — Real exec + ping implementations

| Category | Criteria |
|----------|----------|
| Unit test | **Exec**: (1) `handle_exec("echo hello")` -> output = "hello\n", exit_code = 0, (2) `handle_exec("nonexistent-command")` -> error, exit_code = -1, (3) `handle_exec("sleep 30")` with timeout=1s -> killed, exit_code = -1, (4) Windows: `powershell "Write-Output hello"` -> output = "hello", (5) command with special chars (quotes, pipes, redirects). **Ping**: integration of ping_icmp/tcp/http dispatch. |
| Integration | Full task lifecycle via real Komari server: (1) server sends `exec` event, (2) agent executes, (3) agent uploads result via WS/HTTP, (4) server receives and displays result. |
| Performance | Exec: `std::process::Command::output()` with timeout. Result upload: JSON encode into Vec<u8> (not hot path, allocation acceptable). |
| Code quality | Platform dispatch for shell: Windows = `powershell -Command ...`, Unix = `sh -s`. Timeout via `wait_timeout` on child process. |
| Cross-platform | All 4 platforms. |
| Regression gate | **Wire compatibility**: Task result format must match Go agent: `{"task_id":"...","result":"...","exit_code":...,"finished_at":"..."}`. Command execution behavior must match: same shell selection, same timeout behavior, same output capture. |

### Task 5.8: `src/update.rs` — Self-update

| Category | Criteria |
|----------|----------|
| Unit test | Test version comparison: (1) current < latest -> update needed, (2) current == latest -> up to date, (3) current > latest -> no update. Test GitHub releases API response parse. Test platform-specific asset name matching: `komari-agent-linux-amd64`, `komari-agent-windows-amd64.exe`, etc. Test SHA256 verification (valid + invalid). |
| Integration | Test against test GitHub repo: (1) agent detects new version, (2) downloads asset, (3) verifies SHA256, (4) replaces current binary, (5) exits with code 42. Test that old binary runs until replacement. |
| Performance | Feature-gated: `self-update` feature (DD13). Download: HTTP GET with `std::net::TcpStream` + TLS. Not hot path. |
| Code quality | < 85 lines. Exit code 42 must match Go agent (DD12). On Windows: `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`. On Unix: write to `.new`, `chmod +x`, `rename` (atomic). |
| Cross-platform | All 4 platforms. Platform-specific binary replacement logic. |
| Regression gate | Exit code 42 must match Go agent for service manager compatibility. |

### Task 5.9: `tests/integration.rs` — Full integration test with dummy server

| Category | Criteria |
|----------|----------|
| Unit test | This IS the integration test. |
| Integration | **Dummy Komari server**: (1) start TCP listener, (2) respond to WebSocket upgrade (101 + correct accept key), (3) receive agent heartbeat, (4) send `agent.exec` event, (5) receive `agent.taskResult`, (6) send ping, receive pong, (7) close connection, (8) verify agent reconnects. **Lifecycle** (from Phase 5 spec): connect -> heartbeat -> task exec -> disconnect -> reconnect. |
| Performance | Integration test runs in < 30 seconds. Dummy server and agent run in same process (separate threads). |
| Code quality | `#[cfg(test)] mod integration_tests`. Dummy server via `std::net::TcpListener`. No external test dependencies. |
| Cross-platform | Must pass on all 4 platforms (with platform-appropriate task commands). |
| Regression gate | This test IS the regression gate for Phase 1-5. Failing integration test = blocker. |

### Phase 5 Gate Checklist

```
[ ] Terminal: exec ls -la works on Linux; dir works on Windows 10+
[ ] Terminal graceful shutdown: closing WS sends SIGHUP / TerminateProcess
[ ] ICMP ping to 8.8.8.8 returns RTT < 500ms (with CAP_NET_RAW)
[ ] TCP ping to google.com:443 returns RTT
[ ] HTTP ping to https://google.com returns 200
[ ] Gzip: encoding 2 KB JSON produces valid gzip accepted by gunzip
[ ] DNS: resolve with each configured DNS server; IPv4 prefer; IPv6 flag routes to AAAA
[ ] Self-update: detects newer release, downloads, replaces binary, exits 42
[ ] CF Access: WS upgrade includes CF headers when configured
[ ] Full integration test: connect -> heartbeat -> task exec -> disconnect -> reconnect
[ ] All tests pass on CI (4 platforms)
[ ] rustfmt clean, clippy clean
[ ] Git commit at Phase 5 boundary
```

---

## PHASE 6: Polish + Packaging (~510 lines)

**Goal**: Auto-discovery. Persistent network stats. Windows toast. Install scripts. Documentation.

### Task 6.1: `src/monitor/netstatic.rs` — Persistent traffic history

| Category | Criteria |
|----------|----------|
| Unit test | Test `TrafficData` serialize/deserialize to/from JSON. Test `VecDeque` with max 720 entries: push beyond max -> oldest ejected. Test atomic file write: write to `.tmp`, rename to final. Test file read on startup: valid file -> load, corrupted file -> discard and start fresh, missing file -> start fresh. Test month rotation: `month_rotate` config triggers reset of cumulative counters. |
| Integration | Run agent for 5 minutes, verify `net_static.json` grows. Restart agent, verify previous entries loaded and new entries appended. |
| Performance | `net_static.json` read/write is not hot path (once per minute). Acceptable to allocate. File size < 50 KB (720 entries × ~70 bytes). Atomic write via temp+rename (no corruption on crash). |
| Code quality | < 85 lines. `VecDeque<TrafficData>` as state. Serialize with `EncodeJson` trait. |
| Cross-platform | All platforms. File paths: `./net_static.json` relative to working directory (matching Go). |
| Regression gate | File format must match Go's `net_static.json` format (same JSON keys, same structure). |

### Task 6.2: `src/autodiscovery.rs` — Auto-registration

| Category | Criteria |
|----------|----------|
| Unit test | Test `POST /api/agent/discover` payload: includes hostname, OS, IP, agent_version. Test response handling: 200 with agent ID -> save to `auto-discovery.json`, 4xx -> retry after 60s, 5xx -> retry after 60s. Test config file persistence: load on startup, skip discovery if already registered. |
| Integration | Test against real Komari server with auto-discovery enabled. Fresh agent -> registers, receives ID. Restart -> skips discovery (already registered). |
| Performance | One-time at startup (not hot path). HTTP POST with small JSON body. Poll interval: 60s if unregistered. |
| Code quality | < 60 lines. `serde_json` for response parse (not hot path, acceptable). |
| Cross-platform | All platforms. |
| Regression gate | Auto-discovery request format must match Go agent: same endpoint, same JSON fields, same retry behavior. |

### Task 6.3: `src/platform/windows.rs` — Windows toast notification

| Category | Criteria |
|----------|----------|
| Unit test | Test `show_notification(title, body)` on Windows. Fallback to `MessageBoxW` if toast API unavailable. |
| Integration | Run on Windows 10/11: verify notification appears. Run on older Windows: verify MessageBoxW fallback works. |
| Performance | Feature-gated behind `#[cfg(target_os = "windows")]` + `#[cfg(feature = "toast")]` or always included (since it is Windows-only and small). |
| Code quality | Use `windows-rs` for toast API. `unsafe` for Win32 calls with `// SAFETY:` comments. |
| Cross-platform | Windows only. Compiles to empty on non-Windows. |
| Regression gate | Toast content must match Go agent's Windows toast. |

### Task 6.4: Install scripts (`scripts/install.sh`, `scripts/install.ps1`)

| Category | Criteria |
|----------|----------|
| Unit test | Test `install.sh` on Ubuntu 22.04 and macOS 15: (1) downloads correct binary, (2) installs to `/usr/local/bin/komari-agent`, (3) creates systemd/launchd service, (4) starts service, (5) service survives reboot. Test `install.ps1` on Windows 11: (1) downloads correct binary, (2) installs to `$env:ProgramFiles\komari-agent`, (3) creates scheduled task or nssm service, (4) starts service. |
| Integration | End-to-end: run install script on fresh VM, verify agent connects to Komari server and heartbeats. |
| Performance | Script runtime < 60 seconds (mostly download time). Download from GitHub releases. |
| Code quality | Shell scripts: `set -euo pipefail`. PowerShell: `Set-StrictMode -Version Latest`. |
| Cross-platform | `install.sh`: Linux + macOS. `install.ps1`: Windows. |
| Regression gate | Service management must match Go agent: same service name, same restart policy, same exit code handling (42 = restart after update). |

### Task 6.5: Documentation (`README.md`, `docs/protocol.md`, `docs/building.md`)

| Category | Criteria |
|----------|----------|
| Unit test | N/A (documentation). |
| Integration | Verify all links are valid. Verify code examples are correct. |
| Performance | N/A. |
| Code quality | README.md covers: overview, features, platform support, installation, configuration (all 34 flags with env vars), building from source (prerequisites, `cargo build --release`), architecture overview, troubleshooting. `docs/protocol.md`: v2 JSON-RPC methods, v1 fallback format, heartbeat schema, task schema, ping schema. `docs/building.md`: Rust toolchain requirements, platform toolchains, cross-compilation setup, CI. |
| Cross-platform | Documentation covers all 4 platforms. |
| Regression gate | Documentation must accurately reflect current implementation (not stale). |

### Task 6.6: Final verification — All gates re-checked

| Category | Criteria |
|----------|----------|
| Unit test | All tests from Phase 1-6 must pass. |
| Integration | Real Komari server integration must pass: connect, heartbeat, monitor, basicInfo, exec, ping, terminal, disconnect, reconnect. |
| Performance | **Binary**: < 1 MB stripped linux-amd64, < 1.2 MB stripped windows-amd64. **RSS**: < 3 MB after 60s. **Tick jitter**: < 50ms. **Zero alloc**: verified by dhat or custom allocator. |
| Code quality | `cargo fmt --check` clean. `cargo clippy -- -D clippy::all -D clippy::pedantic` clean (with allowed lints). No `unsafe` except FFI modules. All `// SAFETY:` comments present and correct. |
| Cross-platform | All 4 CI platforms green (build + test + clippy + fmt). |
| Regression gate | Full Go agent compatibility: (1) recorded session replay passes, (2) JSON output matches byte-for-byte, (3) protocol FSM matches, (4) GPU names match, (5) OS names match, (6) memory values match, (7) disk mount list matches. |

### Phase 6 Gate Checklist

```
[ ] Auto-discovery: fresh agent registers; restart picks up existing config
[ ] Network stats: net_static.json persists across restarts
[ ] Windows toast: notification appears on Windows 10/11 (or MessageBoxW fallback)
[ ] Install scripts complete successfully on Ubuntu 22.04, macOS 15, Windows 11
[ ] Agent starts and heartbeats after scripted install
[ ] All Phase 1-5 success criteria still pass (no regressions)
[ ] README covers: overview, features, platform support, install, configure, build, troubleshoot
[ ] Binary size checkpoints met: linux < 1 MB, windows < 1.2 MB
[ ] RSS < 3 MB after 60s
[ ] rustfmt clean, clippy clean
[ ] Git tag v0.1.0 created
```

---

## Appendix A: Integration Testing Against Real Komari Server — Verification Procedure

### A.1 Prerequisites

1. A running Komari server instance (e.g., `https://monitor.example.com`)
2. A valid agent token for that server
3. The Go agent binary (for comparison) and the Rust agent binary (under test)
4. Network access from test machine to server

### A.2 Recorded Fixture Collection (Go Agent)

```bash
# Step 1: Start tcpdump to capture WebSocket traffic
sudo tcpdump -i any -w /tmp/go-agent-ws.pcap 'host monitor.example.com and port 443' &

# Step 2: Run Go agent for 30 seconds
./komari-agent-go --token=<token> --endpoint=https://monitor.example.com &
GO_PID=$!
sleep 30

# Step 3: Send exec event from server UI, wait for result

# Step 4: Stop Go agent
kill $GO_PID
sleep 2

# Step 5: Stop tcpdump
sudo pkill tcpdump
```

### A.3 JSON Wire Compatibility Test

```bash
# Step 1: Load fixture from Go agent's recorded session
# Each text frame in the pcap is one JSON message.
# Extract frames using tshark: tshark -r /tmp/go-agent-ws.pcap -Y 'websocket' -T fields -e websocket.payload

# Step 2: Run Rust agent with recorded server responses
# The integration test harness (tests/integration.rs dummy server) replays
# server-side responses frame-by-frame.

# Step 3: Compare Rust agent's output frames with Go agent's frames
# For each tick N, diff the JSON (ignoring timestamp fields if they differ by up to 1s).
for frame in $(seq 1 30); do
    diff <(go_frame_$frame.json | jq -S .) <(rust_frame_$frame.json | jq -S .)
done
# All diffs must be empty (or only contain expected differences like timestamp).
```

### A.4 Byte-for-Byte Compatibility Checkpoints

Specifically compare these exact JSON fields:

| Checkpoint | Go output | Rust output | Tolerance |
|-----------|-----------|-------------|-----------|
| `cpu.usage` | 12.5 | 12.50 | Exactly match (2 decimal places) |
| `ram.total` | 17179869184 | 17179869184 | Exact |
| `ram.used` | varies | varies | Exact (same mem_mode) |
| `load.load1` | 0.25 | 0.25 | Exact (2 decimal places) |
| `network.up` | 125000.0 | 125000.0 | Exact (1 decimal place) |
| `connections.tcp` | 42 | 42 | Exact |
| `uptime` | 86400 | 86400 | Within 1s |
| `process` | 234 | 234 | Within 5 (processes start/stop) |
| `gpu.name` | "NVIDIA GeForce RTX 4090" | "NVIDIA GeForce RTX 4090" | Exact string match |
| `os_name` | "Ubuntu 22.04.3 LTS" | "Ubuntu 22.04.3 LTS" | Exact string match |

### A.5 Protocol FSM Verification

```bash
# Test 1: Normal operation
# Start agent with v2 endpoint -> verify connected via v2 WS
curl -s https://monitor.example.com/api/admin/agents | jq '.[] | select(.version=="komari-agent-rs")'

# Test 2: v2 WS failure (block WS port via iptables, or use wrong endpoint)
# Agent must: attempt v2 WS -> fail -> attempt v2 HTTP POST after strike 2 -> fall back to v1 after strike 3
# Check server logs for protocol version used

# Test 3: Recovery
# Unblock WS, agent must: detect v2 WS available -> return to v2 WS
# Check server logs: protocol version should change from v1 to v2
```

### A.6 GPU Detection Verification

```bash
# On each platform:
# 1. Run Go agent with --enable-gpu, capture basicInfo
# 2. Run Rust agent with --enable-gpu, capture basicInfo
# 3. Compare gpu.name, gpu.memory_total, gpu.memory_used

# Linux:
nvidia-smi --query-gpu=name,memory.total,memory.used --format=csv,noheader
# Compare with both agents' output

# Windows:
# Run both agents; compare DXGI output

# macOS:
system_profiler SPDisplaysDataType -json | jq '.SPDisplaysDataType[0].spdisplays_vram'

# FreeBSD:
pciconf -lv | grep -B3 -A15 VGA
```

### A.7 Zero-Alloc Verification

```rust
// In tests/zero_alloc.rs or as a dev-dependency test:
// Use dhat heap profiler:

#[cfg(test)]
mod zero_alloc {
    use dhat;

    #[test]
    fn tick_has_zero_heap_allocations() {
        let _profiler = dhat::Profiler::new_heap();
        let mut monitor = Monitor::new();
        let config = Config::default();
        // Warm up: first tick may allocate for initialization
        monitor.tick(&config);
        // Second tick must be zero-alloc
        let _guard = dhat::HeapStats::get().unwrap();
        monitor.tick(&config);
        let stats = dhat::HeapStats::get().unwrap();
        assert_eq!(stats.total_bytes, 0, "monitor.tick() allocated {} bytes", stats.total_bytes);
    }
}

// Alternative: custom #[global_allocator] that panics on alloc:
// #[cfg(test)]
// static ALLOC: PanicAlloc = PanicAlloc;
// struct PanicAlloc;
// unsafe impl GlobalAlloc for PanicAlloc { ... }
// -> This MUST be scoped to the specific test, not global.
```

### A.8 RSS Measurement

```bash
# Step 1: Start agent
./target/release/komari-agent --token=<token> --endpoint=<endpoint> &
AGENT_PID=$!

# Step 2: Wait 60 seconds for steady state
sleep 60

# Step 3: Measure RSS
# Linux:
cat /proc/$AGENT_PID/status | grep VmRSS
# Must be < 3072 KB (3 MB)

# macOS:
ps -o rss= -p $AGENT_PID
# Note: macOS RSS is in KB; must be < 3072

# Windows (PowerShell):
Get-Process -Id $AGENT_PID | Select-Object WorkingSet64
# WorkingSet64 is in bytes; must be < 3145728 (3 MB)

# Step 4: Kill agent
kill $AGENT_PID
```

### A.9 CI Verification Pipeline

```yaml
# .github/workflows/acceptance.yml (adds to ci.yml)
jobs:
  acceptance:
    needs: [build-linux, build-windows, build-macos, build-freebsd]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Binary size check
        run: |
          SIZE=$(stat -c%s target/release/komari-agent)
          if [ $SIZE -gt 1048576 ]; then
            echo "Binary too large: $SIZE bytes (max 1 MB)"
            exit 1
          fi
      - name: RSS check
        run: |
          ./target/release/komari-agent --token=${{ secrets.TEST_TOKEN }} &
          PID=$!
          sleep 60
          RSS=$(cat /proc/$PID/status | grep VmRSS | awk '{print $2}')
          kill $PID
          if [ $RSS -gt 3072 ]; then
            echo "RSS too high: ${RSS} KB (max 3072 KB)"
            exit 1
          fi
      - name: Zero-alloc check
        run: cargo test zero_alloc::tick_has_zero_heap_allocations
      - name: Integration test
        run: cargo test integration::full_lifecycle
```

---

## Appendix B: Allowable Clippy Lint Exceptions

```rust
// Per-task or per-module, add as needed:
#![allow(clippy::module_name_repetitions)]   // monitor/cpu/linux.rs is clear enough
#![allow(clippy::cast_precision_loss)]       // u64 -> f64 for usage percentages
#![allow(clippy::cast_sign_loss)]            // platform APRs return unsigned
#![allow(clippy::cast_possible_truncation)]  // u64 -> u32 for platform API limits
#![allow(clippy::must_use_candidate)]        // fire-and-forget writes

// NOT allowed (must fix):
// clippy::unwrap_used (use expect or ? instead)
// clippy::expect_used (prefer proper error handling, not expect)
// clippy::panic (return Result instead)
// clippy::wildcard_imports
// clippy::missing_safety_doc (every unsafe fn must have Safety section)
```

---

## Appendix C: Feature Gate Matrix

| Feature | Cargo flag | Bin size impact | Default | Tasks requiring it |
|---------|-----------|:---------------:|:-------:|-------------------|
| Core monitoring | (none, always compiled) | ~600 KB | always | P1, P2, P3 |
| `gpu-detection` | `--features gpu-detection` | +80 KB | off | P4 GPU tasks |
| `terminal` | `--features terminal` | +60 KB | off | P5 terminal tasks |
| `ping` | `--features ping` | +30 KB | off | P5 ping tasks |
| `self-update` | `--features self-update` | +15 KB | off | P5 update task |
| `full` | `--features full` (enables all) | ~876 KB total | off | All |

Every task must compile with both `--no-default-features` and `--features full`.
Tests must pass with all feature combinations.

---

## Appendix D: Summary — Per-Phase Binary Size Checkpoints

| Phase | linux-amd64 | windows-amd64 | macos-amd64 | freebsd-amd64 |
|-------|:-----------:|:-------------:|:-----------:|:-------------:|
| P1 (Foundation) | < 2.0 MB | < 2.5 MB | < 2.5 MB | < 2.5 MB |
| P2 (Linux Metrics) | < 1.5 MB | < 2.0 MB | < 2.0 MB | < 2.0 MB |
| P3 (Protocol FSM) | < 1.3 MB | < 1.8 MB | < 1.8 MB | < 1.8 MB |
| P4 (Cross-Platform) | < 1.5 MB | < 2.0 MB | < 2.0 MB | < 2.0 MB |
| P5 (Terminal+Ping+Gzip+DNS) | < 1.2 MB | < 1.5 MB | < 1.5 MB | < 1.5 MB |
| **P6 (Final, -full)** | **< 1.0 MB** | **< 1.2 MB** | **< 1.2 MB** | **< 1.2 MB** |

`--no-default-features` (core only) must be under 700 KB on all platforms at Phase 6.
