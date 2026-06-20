# Komari-Agent-RS — Definitive Task Breakdown

**Version**: 1.0.0
**Date**: 2026-06-20
**Status**: SSOT — all decompositions and cross-reviews synthesized
**Sources synthesized**:
- P1+P2 decomposition: `phased-implementation-plan.md` Phase 1-2 + `acceptance-criteria.md` Phase 1-2
- P3 decomposition: `phased-implementation-plan.md` Phase 3 + `acceptance-criteria.md` Phase 3
- P4 decomposition: `phased-implementation-plan.md` Phase 4 + `acceptance-criteria.md` Phase 4
- P5+P6 decomposition: `phased-implementation-plan.md` Phase 5-6 + `acceptance-criteria.md` Phase 5-6
- Cross-review #1: `dependency-graph.md` — dependency graph, critical path, S.U.P.E.R scorecard, bottleneck analysis
- Cross-review #2: `architecture-reference.md` — module map, API contract, memory/binary budget, design decisions
- Cross-review #3: `spec.md` — confirmed design decisions DD1-DD13, hard constraints, feature matrix

---

## 0. Design Decision Resolutions (DD Conflicts Resolved)

These conflicts were found during cross-review synthesis. Where the architecture reference and spec.md disagree, **the spec wins** (per §0.3 of acceptance-criteria.md: "spec decisions DD1-DD13 conflict with architecture-reference appendices or module descriptions, the spec wins").

### DD1: CLI Parsing — RESOLVED: Hand-written

| Aspect | Architecture Reference | Spec (wins) |
|--------|----------------------|-------------|
| Approach | clap derive (~25 KB) | Hand-written ~150 lines |
| Rationale | Replaces Go's cobra; 34 fields | Saves ~15 KB + unicode dependency chain |
| Resolution | **Hand-written CLI**. Manual `std::env::args()` iteration + `std::env::var()` for env override. No clap. |

### DD5: TLS Certificate Store — RESOLVED: OS Native Roots

| Aspect | Architecture Reference | Spec (wins) |
|--------|----------------------|-------------|
| Approach | webpki-roots (~100 KB blob) | OS native roots per platform |
| Rationale | rustls default; simple | Saves ~80 KB; Linux=`/etc/ssl/certs`, Windows=CryptoAPI, macOS=Security.framework |
| Resolution | **OS native roots**. Use `rustls-platform-verifier` or manual FFI per platform. No `webpki-roots` crate. |

### DD9: Gzip Encoder — RESOLVED: ~200 lines Fixed-Huffman

| Aspect | Architecture Reference | Spec (wins) |
|--------|----------------------|-------------|
| Approach | ~450 lines (CRC32 table 1KB, LZ77 hash chain, BitWriter) | ~200 lines fixed-Huffman encode-only |
| Rationale | Full LZ77 search for better compression | Saves ~250 lines; fixed Huffman is 3-5% worse but sufficient |
| Resolution | **~200 lines max**. Fixed Huffman with pre-computed code tables. CRC32 as small const array. No LZ77 hash chain (use stored blocks for small payloads, fixed Huffman for larger ones). |

### DD13: Feature Gates — RESOLVED: default=[] confirmed

| Aspect | Architecture Reference | Spec (wins) |
|--------|----------------------|-------------|
| Approach | All features compiled by default | `default=[]` (core ~600 KB), `full` enables all (~876 KB) |
| Resolution | **default=[]**. Core only compiles monitoring + v1/v2 + HTTP fallback. `gpu-detection`, `terminal`, `ping`, `self-update` behind feature gates. |

### Additional Resolved Issues from Cross-Reviews

| # | Issue Found In | Description | Resolution |
|---|---------------|-------------|------------|
| R1 | dependency-graph §6.5 RH-1 | tungstenite lock-in — ws.rs couples to tungstenite API | Accept. tungstenite is stable; ws.rs is a 95-line thin wrapper. Replaceable by rewriting 1 file if needed. |
| R2 | dependency-graph §6.4 EH-2 | ICMP ping requires CAP_NET_RAW; returns -1 silently | Accept. 3-tier fallback (ICMP→TCP→HTTP) already handles this. Startup warning logged. |
| R3 | dependency-graph §6.1 SH-1 | monitor/mod.rs tick() scope creep risk | Mitigated. tick() capped at 60 lines. CollectorFn registry pattern if it grows. |
| R4 | dependency-graph §4.1 | json.rs has 35+ fan-in — highest blast radius | Lock API in Phase 1 before any monitor code. Comprehensive test vectors before Phase 2. |
| R5 | architecture-ref vs spec | ws.rs uses tungstenite (DD-009) but spec implies manual frame codec? | **Resolved: tungstenite for WS**. Spec DD3 says "manual frame codec ~350 lines" was the original intent, but architecture DD-009 (tungstenite, sync mode, 30 KB) is the accepted decision. Tungstenite handles frame masking, fragmentation, ping/pong, close handshake correctly per RFC 6455. |
| R6 | dependency-graph §8 Risk-002 | RSS budget 2.97 MB — thin margin | RSS gate in CI (warn-not-fail in CI, manual gate for merge). Arena 8 KB default; bump to 16 KB if overflow observed. DNS cache hard-cap at 50 entries. |
| R7 | phased-plan Phase 1 | Phase 1 subtotal "~600" but file breakdown sums to ~500 | Resolved: Phase 1 files are ~500 lines; the 600 figure includes comments, blanks, inline tests. Use 500 for task line accounting, 600 for budget. |
| R8 | cross-review #2 §7 | Binary budget shows ~659 KB but spec says ~600 KB core | Differences are rounding. The 600 KB figure is the `--no-default-features` stripped binary. The 659 KB includes tungstenite. Both are under 1 MB. Accept. |

---

## 1. Task Table

Each task: ID, Phase, Description, Files, Lines, Priority, Effort, Dependencies, S.U.P.E.R Drivers, Acceptance Criteria, Test.

**Priority**: P0 (blocking, must pass before next task) | P1 (high, phase gate) | P2 (medium, should pass within phase).

**Effort**: S (Small, <2h) | M (Medium, 2-4h) | L (Large, 4-8h) | XL (Extra Large, 1-2d).

### P1 — Foundation + Handshake

### T-11: Cargo.toml + .cargo/config.toml + CI skeleton
- **Phase**: P1
- **Files**: `Cargo.toml`, `.cargo/config.toml`, `.github/workflows/ci.yml`
- **Lines**: ~85 (35 + 8 + 42)
- **Priority**: P0
- **Effort**: S
- **Depends on**: —
- **S.U.P.E.R**: S (config-only, single concern), P (4-OS CI matrix), E (release profile = env-independent binary)
- **Acceptance**: (a) `cargo metadata` resolves without errors. (b) `[profile.release]` settings verified: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`. (c) Empty `main.rs` builds <400 KB stripped. (d) CI 4-OS matrix all green with empty project. (e) Dependencies: only `tungstenite` (sync), `rustls` (ring, tls12), `log`, `env_logger`, `serde`/`serde_json` (parse only), platform `windows` (Windows-only). No clap, no webpki-roots, no tokio.
- **Tests**: `cargo build --release` on all 4 targets; `cargo tree --depth 1` shows expected deps.

### T-12: src/config.rs — Config struct with hand-written CLI
- **Phase**: P1
- **Files**: `src/config.rs`
- **Lines**: ~150 (hand-written per DD1)
- **Priority**: P0
- **Effort**: L
- **Depends on**: —
- **S.U.P.E.R**: S (pure config parsing), U (no global — `&Config` passed explicitly), E (env var override via `std::env::var()`)
- **Acceptance**: (a) 34 config fields matching Go `Config` exactly. (b) Parse from `--flag` args, from `--config-file` JSON, from env vars. (c) `mem_mode()` derivation: mode 0 (default), mode 1 (`--memory-include-cache`), mode 2 (`--memory-report-raw-used`). (d) Flag names match Go agent: `--token`, `--endpoint`, `--interval`, all 34 flags. (e) No `clap`, no derive macros for CLI.
- **Tests**: All 34 fields parsed from args; env var override; config file merge; `mem_mode()` test; default values test.

### T-13: src/crypto.rs — SHA-1 + base64
- **Phase**: P1
- **Files**: `src/crypto.rs`
- **Lines**: ~195 (SHA-1 ~120 + base64 ~75)
- **Priority**: P0
- **Effort**: L
- **Depends on**: —
- **S.U.P.E.R**: S (crypto primitives only), R (self-implemented, zero external deps), E (pure Rust, stack-only)
- **Acceptance**: (a) SHA-1 RFC 3174 vectors: `"abc"` → `a9993e36...`, `""` → `da39a3ee...`, 448-bit message → `84983e44...`. (b) WebSocket accept vector: base64(sha1(decode("dGhlIHNhbXBsZSBub25jZQ==") + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")) = `"s3pPLMBiTxaQ9kYGzzhZRbK+xOo="`. (c) Base64 RFC 4648 vectors: `""`→`""`, `"f"`→`"Zg=="`, `"fo"`→`"Zm8="`, `"foo"`→`"Zm9v"`, `"foob"`→`"Zm9vYg=="`, `"fooba"`→`"Zm9vYmE="`, `"foobar"`→`"Zm9vYmFy"`. (d) All stack-allocated, no `Box` or `Vec` in hot path. (e) No external crate deps.
- **Tests**: All RFC vectors + 256 random round-trip: base64_encode(sha1(input)) deterministic.

### T-14: src/json.rs — JsonBuf + Field + EncodeJson trait
- **Phase**: P1
- **Files**: `src/json.rs`
- **Lines**: ~105
- **Priority**: P0
- **Effort**: L
- **Depends on**: —
- **S.U.P.E.R**: S (JSON encoding only, no parsing), U (no serde coupling), E (stack buffer, no alloc), R (trait-based extension)
- **Acceptance**: (a) `push_u64(0)`→`"0"`, `push_u64(18446744073709551615)`→`"18446744073709551615"`. (b) `push_i64(-9223372036854775808)` correct. (c) `push_f64_prec2(12.5)`→`"12.50"`, `push_f64_prec2(NaN)`→`"null"`. (d) String escaping: `"`→`\"`, `\`→`\\`, `\n`→`\n`, control chars→`\u00XX`. (e) Arena overflow → heap Vec fallback. (f) `arena.reset()` clears, `JsonBuf::new()` writes to beginning. (g) Encode a 2 KB monitoring report <50 microseconds.
- **Tests**: push_u64/push_i64/push_f64_prec2 correctness; string escaping all edge cases; Field::encode_into round-trip; overflow fallback; buffer reuse after reset.

### T-15: src/tls.rs — TLS configuration (OS native roots)
- **Phase**: P1
- **Files**: `src/tls.rs`
- **Lines**: ~40
- **Priority**: P1
- **Effort**: M
- **Depends on**: T-12 (config)
- **S.U.P.E.R**: S (TLS config only), E (OS native roots per DD5 — no bundled certs), R (wraps rustls)
- **Acceptance**: (a) `build_client_config(insecure: bool)` returns valid `rustls::ClientConfig`. (b) Linux: reads from `/etc/ssl/certs/ca-certificates.crt`. (c) Windows: uses CryptoAPI native cert store. (d) macOS: uses Security.framework. (e) No `webpki-roots` crate. (f) `insecure=true` skips cert verification (maps to Go's `InsecureSkipVerify`).
- **Tests**: Connect to `https://httpbin.org` → TLS handshake succeeds. Connect to `https://expired.badssl.com` → rejected (unless insecure mode).

### T-16: src/ws.rs — WebSocket connect + handshake
- **Phase**: P1
- **Files**: `src/ws.rs`
- **Lines**: ~95
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-13 (crypto), T-15 (tls)
- **S.U.P.E.R**: S (WS handshake only), U (thin wrapper over rustls + tungstenite), E (sync, no async)
- **Acceptance**: (a) TCP connect → TLS wrap → HTTP upgrade GET `/api/clients/v2/rpc?token=<token>`. (b) Headers: `Upgrade: websocket`, `Connection: Upgrade`, `Sec-WebSocket-Version: 13`, `Sec-WebSocket-Key` (16 random bytes base64). (c) Verify 101 response + correct `Sec-WebSocket-Accept`. (d) Send first text frame (masked, opcode=1). (e) Send ping, receive pong. (f) Receive and unmask text frames, close frames, ping frames. (g) Uses `tungstenite` (sync mode) per DD-009/R5 resolution.
- **Tests**: Handshake request builder correctness; `verify_handshake_response()` valid/invalid; `generate_key()` produces 24-char base64; frame send/receive round-trip.

### T-17: src/http.rs — HTTP POST client (stub)
- **Phase**: P1
- **Files**: `src/http.rs`
- **Lines**: ~70
- **Priority**: P2
- **Effort**: M
- **Depends on**: T-15 (tls)
- **S.U.P.E.R**: S (HTTP POST only), E (manual HTTP/1.1 — no reqwest per DD4), R (self-contained)
- **Acceptance**: (a) Manual HTTP/1.1 POST with `Content-Type: application/json`, `Host`, optional `Content-Encoding: gzip`, optional `CF-Access-*` headers. (b) Response parse: status code + header extraction. (c) TLS via rustls for HTTPS endpoints. (d) No `reqwest` crate.
- **Tests**: POST request builder; response status parser.

### T-18: src/protocol/ — JSON-RPC types (v2, v1, mod)
- **Phase**: P1
- **Files**: `src/protocol/v2.rs`, `src/protocol/v1.rs`, `src/protocol/mod.rs`
- **Lines**: ~115 (65 + 45 + 5)
- **Priority**: P0
- **Effort**: M
- **Depends on**: T-14 (json)
- **S.U.P.E.R**: S (protocol types only), U (depends only on json.rs), E (pure data types), R (v1/v2 separation)
- **Acceptance**: (a) 11 method constants: `agent.report`, `agent.basicInfo`, `agent.pingResult`, `agent.taskResult`, `agent.exec`, `agent.ping`, `agent.message`, `agent.event`, `agent.terminal.request`, `agent.pull`. (b) `build_notification(method, params)` → `{"jsonrpc":"2.0","method":"<method>","params":{...}}`. (c) `build_request(id, method, params)` → same + `"id":"..."`. (d) V1 types: flat JSON without jsonrpc envelope. (e) All builders use pre-allocated Vec with exact capacity.
- **Tests**: Notification build; request build; report payload wrapper; ack_event_ids array formatting; V1 payload byte-identical to Go.

### T-19: src/server/mod.rs — Initial run() with static heartbeat
- **Phase**: P1
- **Files**: `src/server/mod.rs`
- **Lines**: ~95
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-16 (ws), T-18 (protocol)
- **S.U.P.E.R**: S (server orchestration entry), U (single entry point for main loop)
- **Acceptance**: (a) `run(&config)`: TCP connect via DNS → TLS → WS upgrade → send static heartbeat JSON. (b) Heartbeat: `{"jsonrpc":"2.0","method":"agent.report","params":{"report":{"cpu":{"usage":0.0},"message":"heartbeat"}}}`. (c) Single-threaded: `std::thread::sleep` for timing. (d) Connect to real Komari server → server receives valid heartbeat.
- **Tests**: `run()` with mock server; static heartbeat JSON matches expected format.

### T-1A: src/app.rs + src/main.rs — Entry point
- **Phase**: P1
- **Files**: `src/app.rs`, `src/main.rs`
- **Lines**: ~95 (80 + 15)
- **Priority**: P1
- **Effort**: S
- **Depends on**: T-12 (config), T-19 (server)
- **S.U.P.E.R**: S (pure delegation), U (no logic in main.rs), R (exit code return)
- **Acceptance**: (a) `main()` parses config, calls `app::run()`, exits with return code. (b) `main.rs` <15 lines. (c) CLI flags identical to Go agent: same flag names (long form), same env var names, same defaults. (d) Help flag outputs all 34 config fields. (e) Exit code 0 on normal run; exit code 42 on self-update (DD12).
- **Tests**: Flag parsing; help output; exit code propagation.

### P2 — Linux Metrics + Zero-Alloc Loop

### T-21: src/arena.rs — ScratchArena + SmallVec
- **Phase**: P2
- **Files**: `src/arena.rs`
- **Lines**: ~140
- **Priority**: P0
- **Effort**: L
- **Depends on**: —
- **S.U.P.E.R**: S (memory management only), U (zero external deps), E (stack buffer, no global allocator coupling), R (self-contained data structure)
- **Acceptance**: (a) `ScratchArena`: alloc 1 byte → Some; alloc 8192 bytes → Some; alloc 8193 bytes → None. (b) Aligned alloc (u64 at offset 1 → aligned pointer). (c) `reset()` → subsequent alloc writes to beginning. (d) `SmallVec<T, N>`: push 0..N → inline storage; push N+1 → heap spill; `as_slice()` correct after spill. (e) `alloc()` <5 CPU instructions; `reset()` = single assignment.
- **Tests**: Alloc/fail/reset cycle; alignment; SmallVec inline + spill; zero-alloc proof (custom `#[global_allocator]` panics in arena path).

### T-22: src/platform/mod.rs + src/platform/linux.rs
- **Phase**: P2
- **Files**: `src/platform/mod.rs`, `src/platform/linux.rs`
- **Lines**: ~20 (15 + 5)
- **Priority**: P1
- **Effort**: S
- **Depends on**: —
- **S.U.P.E.R**: S (platform dispatch only), P (cfg-gated, zero-cost), U (no runtime checks)
- **Acceptance**: (a) `#[cfg(target_os)]` gates select correct platform module. (b) `platform::current` re-exports correct platform types. (c) Type aliases: `type CurrentCpu = linux::Cpu` etc. Compile-time dispatch, no vtable.
- **Tests**: Compiles on all 4 targets with correct platform selected.

### T-23: src/monitor/cpu/ — CPU collection (Linux)
- **Phase**: P2
- **Files**: `src/monitor/cpu/mod.rs`, `src/monitor/cpu/linux.rs`
- **Lines**: ~100 (15 + 85)
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-21 (arena), T-14 (json)
- **S.U.P.E.R**: S (CPU metrics only), E (/proc filesystem), U (platform-isolated via cfg)
- **Acceptance**: (a) Parse `/proc/stat`: total + per-core CPU time. Compute usage % from delta between ticks. (b) Parse `/proc/cpuinfo`: model name, cores, MHz. (c) First tick: usage = 0.0. (d) CPU model name with special chars: `"Intel(R) Core(TM) i7-13700K"`. (e) All reads into arena; byte-level scan; no regex; no `String` allocation.
- **Tests**: Fixture-based: recorded `/proc/stat` + `/proc/cpuinfo`; usage delta computation; single-core system; guest/guest_nice fields.

### T-24: src/monitor/mem/ — Memory collection (Linux, 3-mode)
- **Phase**: P2
- **Files**: `src/monitor/mem/mod.rs`, `src/monitor/mem/linux.rs`
- **Lines**: ~190 (50 + 140)
- **Priority**: P0
- **Effort**: XL
- **Depends on**: T-21 (arena), T-14 (json), T-12 (config for mem_mode)
- **S.U.P.E.R**: S (memory + swap only), U (3-mode dispatch in mod.rs, platform access in linux.rs), E (/proc/meminfo)
- **Acceptance**: (a) Parse `/proc/meminfo`: MemTotal, MemAvailable, MemFree, Buffers, Cached, SwapTotal, SwapFree, SwapCached. (b) Mode 0: used = total - MemAvailable. (c) Mode 1: used = total - MemFree - Buffers - Cached. (d) Mode 2: used = total - MemFree. (e) MemAvailable missing (kernel <3.14) → fallback to MemFree+Buffers+Cached. (f) `free -b` fallback for `CallFree()` mode. (g) Cross-validate against Go agent on same host for all 3 modes: values match within 1%.
- **Tests**: Fixture-based all 3 modes; MemAvailable missing fallback; swap disabled (SwapTotal=0); `free -b` output parse; Go agent comparison test vectors.

### T-25: src/monitor/disk/ — Disk collection (Linux)
- **Phase**: P2
- **Files**: `src/monitor/disk/mod.rs`, `src/monitor/disk/linux.rs`
- **Lines**: ~115 (35 + 80)
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-21 (arena), T-14 (json)
- **S.U.P.E.R**: S (disk metrics only), E (/proc/mounts + statvfs), U (physical filter in mod.rs)
- **Acceptance**: (a) Parse `/proc/mounts`. (b) `is_physical_disk()`: exclude 30+ patterns matching Go exactly: `/dev/loop*`, `/sys/*`, `/proc/*`, `/run/*`, `/snap/*`, `/var/lib/docker/*`, `overlay`, `tmpfs`, `devtmpfs`, `cgroup`, `cgroup2`, `pstore`, `bpf`, `debugfs`, `tracefs`, `fusectl`, `configfs`, `securityfs`, `hugetlbfs`, `devpts`, `mqueue`, `binfmt_misc`, `squashfs`, `ramfs`, `aufs`, `devfs`, `autofs`, `efivarfs`, `nfs`, `nfs4`, `cifs`, `smbfs`, `rpc_pipefs`, `/var/lib/kubelet/*`. (c) `statvfs` per physical mount for total/used/free. (d) Total = sum of all physical mounts; Used = sum.
- **Tests**: Fixture-based with recorded `/proc/mounts` + mock `statvfs`; physical filter correctness (30+ patterns); zero physical disks; Go agent mount list comparison.

### T-26: src/monitor/net/ — Network collection (Linux)
- **Phase**: P2
- **Files**: `src/monitor/net/mod.rs`, `src/monitor/net/linux.rs`
- **Lines**: ~130 (40 + 90)
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-21 (arena), T-14 (json), T-12 (config for NIC filter)
- **S.U.P.E.R**: S (network metrics only), E (/proc/net/dev + /proc/net/tcp), U (delta logic in mod.rs)
- **Acceptance**: (a) Parse `/proc/net/dev`: RX/TX bytes + packets per interface. (b) Speed = delta from previous sample / interval. First tick: speed = 0. (c) NIC include/exclude filter with wildcard matching (`*` = any substring). (d) Connection count: `/proc/net/tcp` + `/proc/net/tcp6` → count entries (exclude header). (e) Loopback excluded by default; virtual interfaces (docker, tun, veth) filtered per config.
- **Tests**: Fixture-based `/proc/net/dev`; delta calculation; first-tick zero speed; NIC wildcard filter; `/proc/net/tcp` + tcp6 entry counting; Go agent comparison.

### T-27: src/monitor/load/ — Load average (Linux)
- **Phase**: P2
- **Files**: `src/monitor/load/mod.rs`, `src/monitor/load/linux.rs`
- **Lines**: ~45 (10 + 35)
- **Priority**: P1
- **Effort**: S
- **Depends on**: T-21 (arena)
- **S.U.P.E.R**: S (load average only), E (/proc/loadavg), U (trivial platform dispatch)
- **Acceptance**: (a) Parse `/proc/loadavg`: load1, load5, load15 as f64. (b) Example: `"0.25 0.32 0.28 1/456 12345\n"` → load1=0.25, load5=0.32, load15=0.28. (c) High load >100 handled.
- **Tests**: Fixture-based; Go agent comparison.

### T-28: src/monitor/connections/ — Connection count (Linux)
- **Phase**: P2
- **Files**: `src/monitor/connections/mod.rs`, `src/monitor/connections/linux.rs`
- **Lines**: ~40 (10 + 30)
- **Priority**: P1
- **Effort**: S
- **Depends on**: T-21 (arena)
- **S.U.P.E.R**: S (connection count only), E (/proc/net/tcp), U (simple counter)
- **Acceptance**: (a) Count non-header lines in `/proc/net/tcp` + `/proc/net/tcp6` → TCP count. (b) UDP count from `/proc/net/udp` + `/proc/net/udp6`. (c) Empty files → count 0.
- **Tests**: Fixture-based; empty files; Go agent comparison.

### T-29: src/monitor/process/ — Process count (Linux)
- **Phase**: P2
- **Files**: `src/monitor/process/mod.rs`, `src/monitor/process/linux.rs`
- **Lines**: ~50 (10 + 40)
- **Priority**: P1
- **Effort**: S
- **Depends on**: T-21 (arena)
- **S.U.P.E.R**: S (process count only), E (/proc filesystem), U (simple counter)
- **Acceptance**: (a) Count `/proc/<pid>/` where PID is numeric. (b) Count running processes via `/proc/<pid>/status` State=R. (c) Non-numeric entries (self, thread-self, fs) excluded. (d) Process that exits between readdir and read → skip gracefully.
- **Tests**: Fixture-based with mock /proc structure; Go agent comparison.

### T-2A: src/monitor/uptime/ — Uptime (Linux)
- **Phase**: P2
- **Files**: `src/monitor/uptime/mod.rs`, `src/monitor/uptime/linux.rs`
- **Lines**: ~35 (10 + 25)
- **Priority**: P1
- **Effort**: S
- **Depends on**: T-21 (arena)
- **S.U.P.E.R**: S (uptime only), E (/proc/uptime), U (trivial)
- **Acceptance**: (a) Parse `/proc/uptime` first field → u64 seconds. (b) Example: `"86400.00 12345.67\n"` → uptime=86400. (c) Missing file → error handled.
- **Tests**: Fixture-based; Go agent comparison (within 1s drift).

### T-2B: src/monitor/mod.rs — Monitor struct + tick() orchestrator
- **Phase**: P2
- **Files**: `src/monitor/mod.rs` (expand)
- **Lines**: ~55
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-23 through T-2A (all Linux collectors), T-21 (arena), T-14 (json)
- **S.U.P.E.R**: S (tick orchestration only, capped at 60 lines), U (calls collectors in fixed order, registry pattern if grows), E (platform dispatch via cfg)
- **Acceptance**: (a) `Monitor::new()` initializes with zero state (prev_net_rx/tx, total_up/down, boot_time_secs). (b) `tick(&mut self, config: &Config) -> &[u8]`: arena.reset() → collect CPU/mem/swap/load/disk/net/connections/uptime/process → encode into JsonBuf → return &[u8]. (c) **Zero-alloc proof**: `dhat` shows zero heap allocations in tick call tree. (d) **Tick timing**: complete tick <10ms on idle 4-core system. (e) **Arena**: after tick, cursor <4096 (8 KB capacity). (f) EncodeJson used for all monitoring types.
- **Tests**: Two consecutive ticks (second has non-zero cpu_usage + net speed); zero-alloc verification via dhat; tick timing benchmark; arena overflow test.

### T-2C: src/server/mod.rs (expand) — Integrate monitor tick loop
- **Phase**: P2
- **Files**: `src/server/mod.rs` (expand from T-19)
- **Lines**: ~40 (adds to T-19)
- **Priority**: P0
- **Effort**: M
- **Depends on**: T-2B (monitor), T-19 (server base)
- **S.U.P.E.R**: S (main loop integration), U (delegates to monitor + protocol)
- **Acceptance**: (a) After WS connect: `loop { monitor.tick(); encode to JsonBuf; wrap in v2 notification; send WS text frame; sleep 1s; }`. (b) RSS <3 MB after 60s running (VmRSS). (c) 1-second tick jitter <50ms under idle system. (d) `cargo test` passes all collector unit tests.
- **Tests**: 60-second run test measuring RSS + tick jitter; `cargo test` all Phase 2 tests.

### P3 — Protocol FSM + Fallback

### T-31: src/protocol/fsm.rs — FallbackFsm + ConnectionFsm
- **Phase**: P3
- **Files**: `src/protocol/fsm.rs`
- **Lines**: ~120
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-18 (protocol types)
- **S.U.P.E.R**: S (pure state machine), U (zero deps beyond protocol types), E (pure data, no I/O), R (replaceable: 1 file, 2 consumers)
- **Acceptance**: (a) All 12 FSM transitions from Appendix B.5 tested. (b) 3-strike counter: 3 consecutive v2 failures → v1; 1 success resets. (c) Network errors (`record_failure(false)`) do NOT increment counter. (d) `new(1)` starts in V1HttpPost; `new(2)` starts in V2WebSocket. (e) `record_success()` from V1HttpPost returns to V2WebSocket. (f) `reset()` → V2WebSocket, counter=0. (g) All enums `Copy`, all transitions are pure match arms, zero allocations.
- **Tests**: All 12 transition edges; 3-strike cycle; success-reset; network vs protocol error distinction; config-driven initial state.

### T-32: src/server/backoff.rs — Backoff with jitter
- **Phase**: P3
- **Files**: `src/server/backoff.rs`
- **Lines**: ~50
- **Priority**: P0
- **Effort**: M
- **Depends on**: —
- **S.U.P.E.R**: S (exponential backoff only), U (self-contained PRNG), E (pure math, no I/O), R (replaceable: 50 lines)
- **Acceptance**: (a) `Backoff::new(1s, 300s)`: `next_delay()` ≈1s (±25% jitter) → ≈2s → ≈4s → ... caps at 300s. (b) Jitter bounds: all delays within ±25% of base. (c) `reset()` → next delay ≈1s again. (d) Same seed produces deterministic sequence. (e) XorShift PRNG (no `std::rand`, no external crate). (f) Jitter logic matches Go agent's statistical distribution.
- **Tests**: Base sequence test; jitter bounds (100 calls, deterministic seed); reset test; seed determinism; min/max bounds.

### T-33: src/server/reconnection.rs — Reconnection loop
- **Phase**: P3
- **Files**: `src/server/reconnection.rs`
- **Lines**: ~70
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-31 (fsm), T-32 (backoff), T-16 (ws)
- **S.U.P.E.R**: S (reconnection logic only), U (select-style polling), E (platform poller wrapper needed for Windows)
- **Acceptance**: (a) Select-style polling: data tick fires at interval, heartbeat fires at 30s, read timeout returns. (b) On WS error → `backoff.next_delay()` → reconnect. (c) After `max_retries` → log fatal, exit. (d) Timer precision: data tick interval within 50ms of configured. (e) Non-blocking socket read with timeout (no busy-wait). (f) On Windows: `select()` instead of `poll()` via thin `Poller` abstraction.
- **Tests**: Select-style poll; reconnect trigger; max retries exit; heartbeat interval; Go agent behavior match (30s heartbeat, configurable tick, 5min backoff cap).

### T-34: src/http.rs (expand) — Full HTTP POST fallback
- **Phase**: P3
- **Files**: `src/http.rs` (expand from T-17)
- **Lines**: ~70 (replaces stub)
- **Priority**: P0
- **Effort**: M
- **Depends on**: T-17 (http stub), T-15 (tls), T-14 (json)
- **S.U.P.E.R**: S (HTTP POST only), U (manual HTTP/1.1), E (no reqwest), R (self-contained)
- **Acceptance**: (a) `post_json(url, body, timeout)`: builds correct HTTP/1.1 POST. (b) All required headers: `Host`, `Content-Type: application/json`, optional `Content-Encoding: gzip`, optional `CF-Access-*`. (c) Status 200 → Ok; 401 → auth error; 500 → server error; timeout → timeout error. (d) POST body reuses arena buffer (copy to heap Vec for send). (e) Wire compatibility: POST body byte-identical to WS text frame body for same monitoring data.
- **Tests**: POST request builder; status code parsing; timeout handling; wire comparison vs WS frame body.

### T-35: src/server/task.rs — Stub exec + ping handlers
- **Phase**: P3
- **Files**: `src/server/task.rs` (new, stub)
- **Lines**: ~75
- **Priority**: P1
- **Effort**: M
- **Depends on**: T-18 (protocol types)
- **S.U.P.E.R**: S (task dispatch only), R (stub → real impl in P5)
- **Acceptance**: (a) `handle_exec(task_id, cmd)` returns `{status: "ok", output: "stub"}`. (b) `handle_ping(task_id, ping_type, target)` returns `{status: "ok", results: []}`. (c) Task ID parsed from incoming JSON-RPC event. (d) Stub responses parse correctly by Komari server.
- **Tests**: Exec stub; ping stub; task_id parsing; JSON format validation.

### T-36: src/protocol/v2.rs (expand) — BuildReportPayload et al.
- **Phase**: P3
- **Files**: `src/protocol/v2.rs` (expand from T-18)
- **Lines**: ~65 (adds to T-18)
- **Priority**: P0
- **Effort**: M
- **Depends on**: T-18 (protocol base), T-14 (json)
- **S.U.P.E.R**: S (protocol builders), U (depends only on json.rs), E (pure data)
- **Acceptance**: (a) `build_report_payload(json)` wraps in `{"report":<json>}` → v2 notification. (b) `build_report_request(id, json, ack_ids)` includes `ack_event_ids` array. (c) `build_basic_info_payload(json)` wraps in `{"info":<json>}`. (d) `build_ping_result(task_id, type, value, finished_at)` formatted correctly. (e) Wire compatibility: JSON byte-identical to Go agent for same inputs.
- **Tests**: All builders with known inputs; ack_event_ids with 0/1/multiple IDs; Go agent comparison.

### T-37: src/server/mod.rs (expand) — Integrate FSM + reconnection + backoff
- **Phase**: P3
- **Files**: `src/server/mod.rs` (expand from T-2C)
- **Lines**: ~60 (adds to T-2C)
- **Priority**: P0
- **Effort**: XL
- **Depends on**: T-31 (fsm), T-32 (backoff), T-33 (reconnection), T-34 (http), T-35 (task), T-36 (protocol builders), T-2C (server + monitor)
- **S.U.P.E.R**: S (main loop integration), U (facade over subsystems), E (no async, sync loop)
- **Acceptance**: (a) Full lifecycle: connect v2 WS → send heartbeat → inject v2 protocol error → verify strike 1 → inject 2 more → verify v1 HTTP fallback → inject success → return to v2 WS. (b) **Recorded session replay**: Go agent's full connect→heartbeat→disconnect replayed against Rust agent. JSON outputs compared frame-by-frame, byte-identical. (c) End-to-end tick: collect + encode + send <10ms (excluding network). (d) Phase 1 + Phase 2 regression: all P1/P2 tests still pass.
- **Tests**: FSM integration test; recorded session replay; regression re-run of P1/P2 tests.

### P4 — Cross-Platform Metrics

P4 is organized into **7 parallel lanes** (LL1-LL7). Each lane is a task.

### T-41 [Lane 1]: Linux IP + missing collectors
- **Phase**: P4
- **Files**: `src/monitor/ip/mod.rs`, `src/monitor/ip/linux.rs`, expand load/connections/process/uptime mod.rs for platform dispatch
- **Lines**: ~120 (55 + 55 + 10)
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-2B (monitor), T-22 (platform)
- **S.U.P.E.R**: S (IP detection), U (platform-isolated), E (netlink + HTTP fallback)
- **Acceptance**: (a) IP detection: Netlink RTM_GETADDR or getifaddrs. (b) HTTP fallback to ipify-like service. (c) Custom IP override from config. (d) Expand load/connections/process/uptime mod.rs for platform dispatch (prep for lanes 2-4).
- **Tests**: Fixture-based; Go agent comparison.

### T-42 [Lane 2]: Windows metrics (all 9 collectors)
- **Phase**: P4
- **Files**: `src/platform/windows.rs`, `src/monitor/cpu/windows.rs`, `src/monitor/mem/windows.rs`, `src/monitor/disk/windows.rs`, `src/monitor/net/windows.rs`, `src/monitor/load/windows.rs`, `src/monitor/connections/windows.rs`, `src/monitor/process/windows.rs`, `src/monitor/uptime/windows.rs`, `src/monitor/ip/windows.rs`
- **Lines**: ~570 (25 + 70 + 75 + 60 + 70 + 25 + 30 + 45 + 20 + 55)
- **Priority**: P1
- **Effort**: XL
- **Depends on**: T-2B (monitor), T-22 (platform)
- **S.U.P.E.R**: S (per-collector files), U (platform-isolated via cfg), P (Win32 API via windows-rs), E (Registry + Win32), R (windows-rs crate)
- **Acceptance**: (a) CPU: `GetSystemInfo` + Registry for name + `QueryPerformanceCounter`. (b) Memory: `GlobalMemoryStatusEx` + `GetPerformanceInfo`. 3-mode dispatch identical to Linux. (c) Disk: `GetLogicalDrives` + `GetDiskFreeSpaceExW`. Filter `DRIVE_FIXED`. (d) Network: `GetIfTable2` + `GetTcpTable2`/`GetTcp6Table2`. (e) Process: `K32EnumProcesses`. (f) Uptime: `GetTickCount64`. (g) Load: Performance counter. (h) Connections: `GetTcpTable2` row count. (i) IP: `GetAdaptersAddresses`. (j) All `unsafe` FFI blocks have `// SAFETY:` comments. (k) Cross-validate against Go agent on same Windows host: all values within 1%.
- **Tests**: Fixture-based where possible (mock Win32 calls); Go agent comparison; 3-mode memory validation on Windows.

### T-43 [Lane 3]: macOS metrics (all 9 collectors)
- **Phase**: P4
- **Files**: `src/platform/macos.rs`, `src/monitor/cpu/macos.rs`, `src/monitor/mem/macos.rs`, `src/monitor/disk/macos.rs`, `src/monitor/net/macos.rs`, `src/monitor/load/macos.rs`, `src/monitor/connections/macos.rs`, `src/monitor/process/macos.rs`, `src/monitor/uptime/macos.rs`, `src/monitor/ip/macos.rs`
- **Lines**: ~390 (20 + 50 + 60 + 40 + 55 + 15 + 25 + 35 + 20 + 40)
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-2B (monitor), T-22 (platform)
- **S.U.P.E.R**: S (per-collector files), U (platform-isolated via cfg), P (sysctl + Mach APIs), E (sysctl + host_statistics64), R (libc crate)
- **Acceptance**: (a) CPU: `sysctl hw.logicalcpu` / `machdep.cpu.brand_string`. (b) Memory: `sysctl hw.memsize` + `host_statistics64` + `sysctl vm.swapusage`. (c) Disk: `getmntinfo` + `statfs`. Exclude devfs, autofs. (d) Network: `sysctl net` + routing socket. (e) Load: `getloadavg()`. (f) Connections: `sysctl net.inet.tcp`. (g) Process: `sysctl kern.proc.all`. (h) Uptime: `sysctl kern.boottime`. (i) IP: `getifaddrs`. (j) All `unsafe` FFI blocks have `// SAFETY:` comments.
- **Tests**: Fixture-based with mock sysctl results; Go agent comparison where darwin support exists.

### T-44 [Lane 4]: FreeBSD metrics (all 9 collectors)
- **Phase**: P4
- **Files**: `src/platform/freebsd.rs`, `src/monitor/cpu/freebsd.rs`, `src/monitor/mem/freebsd.rs`, `src/monitor/disk/freebsd.rs`, `src/monitor/net/freebsd.rs`, `src/monitor/load/freebsd.rs`, `src/monitor/connections/freebsd.rs`, `src/monitor/process/freebsd.rs`, `src/monitor/uptime/freebsd.rs`, `src/monitor/ip/freebsd.rs`
- **Lines**: ~390 (20 + 50 + 60 + 40 + 55 + 15 + 25 + 35 + 20 + 40)
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-2B (monitor), T-22 (platform)
- **S.U.P.E.R**: S (per-collector files), U (platform-isolated via cfg), P (sysctl + KVM), E (sysctl + kvm_getswapinfo), R (libc crate)
- **Acceptance**: (a) CPU: `sysctl hw.model` / `hw.ncpu` / `kern.cp_times`. (b) Memory: `sysctl hw.physmem` / `hw.usermem` + `kvm_getswapinfo`. (c) Disk: `getmntinfo` + `statfs`. (d) Network: `sysctl net` + kvm. (e) Load: `getloadavg()`. (f) Connections: `sysctl net.inet.tcp`. (g) Process: `sysctl kern.proc.all`. (h) Uptime: `sysctl kern.boottime`. (i) IP: `getifaddrs`.
- **Tests**: Fixture-based with mock sysctl/kvm results; cross-compilation CI ensures build correctness; runtime via Cirrus CI or QEMU.

### T-45 [Lane 5]: GPU — Linux (nvidia-smi + rocm-smi + DRM)
- **Phase**: P4
- **Files**: `src/monitor/gpu/mod.rs`, `src/monitor/gpu/linux.rs`
- **Lines**: ~190 (55 + 135)
- **Priority**: P2 (feature-gated)
- **Effort**: L
- **Depends on**: T-2B (monitor), T-21 (arena)
- **S.U.P.E.R**: S (GPU detection only), U (platform-isolated), E (exec nvidia-smi/rocm-smi), R (feature-gated: `gpu-detection`)
- **Acceptance**: (a) NVIDIA: exec `nvidia-smi --query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total --format=csv,noheader`. Parse CSV output. (b) AMD: exec `rocm-smi --showproductname --showtemp --showuse --showmeminfo vram`. Key scanner approach. (c) Fallback: `/sys/class/drm/card*/device/vendor` check (0x10de=NVIDIA, 0x1002=AMD). (d) 3-tier search: `which nvidia-smi` → `/usr/bin/nvidia-smi` → DRM fallback. (e) Feature-gated: `#[cfg(feature = "gpu-detection")]`. (f) GPU names match Go agent on same hardware.
- **Tests**: Parse nvidia-smi CSV; rocm-smi key scanner; DRM vendor check; multi-GPU (2/4/8); temperatures absent ([Not Supported]); Go agent comparison.

### T-46 [Lane 6]: GPU — Windows (DXGI)
- **Phase**: P4
- **Files**: `src/monitor/gpu/windows.rs`
- **Lines**: ~115
- **Priority**: P2 (feature-gated)
- **Effort**: L
- **Depends on**: T-2B (monitor), T-42 (Windows platform)
- **S.U.P.E.R**: S (GPU detection only), U (platform-isolated), E (DXGI via windows-rs), R (feature-gated)
- **Acceptance**: (a) DXGI via `windows-rs` crate: enumerate adapters, get desc for name + VRAM (`DedicatedVideoMemory`). (b) Utilization via `GetPerformanceData` WMI fallback if DXGI returns 0. (c) Cross-validate VRAM values against Go agent on same Windows host. If different, add `gpu_mem_mode` config flag. (d) Feature-gated.
- **Tests**: DXGI adapter enumeration mock; VRAM extraction; WMI fallback; Go agent comparison.

### T-47 [Lane 7]: GPU macOS/FreeBSD + OS detection + Virtualization + CI
- **Phase**: P4
- **Files**: `src/monitor/gpu/macos.rs`, `src/monitor/gpu/freebsd.rs`, `src/monitor/os.rs`, `src/monitor/virtualization.rs`, `.github/workflows/ci.yml` (expand)
- **Lines**: ~375 (55 + 65 + 115 + 85 + 75)
- **Priority**: P1
- **Effort**: XL
- **Depends on**: T-2B (monitor), T-43 (macOS), T-44 (FreeBSD)
- **S.U.P.E.R**: S (GPU + OS + virt + CI — four sub-tasks), U (per-platform isolation), E (system_profiler, pciconf, /etc/os-release, CPUID), R (feature-gated GPU)
- **Acceptance**: (a) macOS GPU: exec `system_profiler SPDisplaysDataType -json` (10.15+). Parse JSON for `spdisplays_vram`, `sppci_model`. Fallback: plain text parse. (b) FreeBSD GPU: exec `pciconf -lv | grep -B3 -A15 VGA`. Limited VRAM info. (c) OS detection: Linux=`/etc/os-release` + heuristics (Android, Synology, PVE, fnOS); Windows=Registry `CurrentVersion`; macOS=`sw_vers`; FreeBSD=`uname -r` + `freebsd-version`. Unknown → `"Linux"` fallback. (d) Virtualization: Container=`/proc/self/cgroup` + `/.dockerenv`; VM=CPUID hypervisor bit + DMI product_name (KVM, VMware, VirtualBox, Hyper-V); Windows=CPUID `__cpuid`; macOS=`sysctl kern.hv_support`. (e) CI: full 4-OS matrix with `cargo test`, `cargo build --release`, `cargo clippy -- -D warnings`, `cargo fmt --check`. (f) No platform-specific code in shared modules (all behind `cfg(target_os)`).
- **Tests**: OS detection per platform fixtures; virtualization detection mock; CI green on all 4 platforms; GPU macOS system_profiler parse; GPU FreeBSD pciconf parse.

### P5 — Terminal + Ping + Gzip + DNS + Update

### T-51: src/gzip.rs — Fixed-Huffman DEFLATE encoder
- **Phase**: P5
- **Files**: `src/gzip.rs`
- **Lines**: ~200 (per DD9)
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-14 (json)
- **S.U.P.E.R**: S (gzip encoding only, no decoding), U (self-implemented, zero external deps), E (pure Rust, stack-based BitWriter), R (no flate2 lock-in)
- **Acceptance**: (a) CRC32 vectors: `""`→0x00000000, `"123456789"`→0xCBF43926, `"\x00"`→0xD202EF8D, `"\xFF"`→0xFF000000, `"hello world"`→0x0D4A1185. (b) Gzip round-trip: `gzip_bytes(b'{"cpu":{"usage":12.5}}')` → gunzip produces identical JSON. (c) Fixed Huffman tables match RFC 1951 Section 3.2.6. (d) Payload <200 bytes → stored blocks (BTYPE=00). (e) Encode 2 KB JSON <500 microseconds. (f) Gzip output accepted by Komari server's Go `compress/gzip` reader.
- **Tests**: CRC32 vectors; gzip round-trip; fixed Huffman code length verification; stored-block fallback; boundary cases (empty, 1 byte, 64KB, all byte values, repetitive input); server acceptance.

### T-52: src/dns.rs — Full custom DNS resolver
- **Phase**: P5
- **Files**: `src/dns.rs`
- **Lines**: ~165
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-12 (config)
- **S.U.P.E.R**: S (DNS resolution only), U (configurable server list), E (UDP socket, DNS wire format), R (self-implemented)
- **Acceptance**: (a) DNS query build: A query (QTYPE=1) + AAAA query (QTYPE=28), correct QNAME encoding. (b) Response parse: A, AAAA, CNAME chain, NXDOMAIN, truncated → TCP fallback. (c) Cache: TTL-bounded, max 5 min cap, hard cap at 50 entries with LRU eviction. (d) 10 built-in DNS servers (matching Go Appendix D). (e) Prefer IPv4 by default; `--prefer-ip-version=6` → AAAA records first. (f) `--custom-dns` flag adds server. (g) Timeout: 2s per server.
- **Tests**: Query build correctness; response parse all types; cache hit/miss/expiry; server list iteration; IPv4/IPv6 preference; timeout handling.

### T-53: src/terminal/ — PTY (Unix) + ConPTY (Windows)
- **Phase**: P5
- **Files**: `src/terminal/mod.rs`, `src/terminal/unix.rs`, `src/terminal/windows.rs`
- **Lines**: ~365 (70 + 140 + 155)
- **Priority**: P2 (feature-gated)
- **Effort**: XL
- **Depends on**: —
- **S.U.P.E.R**: S (terminal PTY only), U (trait-based: `Terminal` trait + platform impls), P (Unix vs Windows completely separate), E (posix_openpt / CreatePseudoConsole), R (feature-gated: `terminal`)
- **Acceptance**: (a) Unix: `posix_openpt()` + `grantpt()` + `unlockpt()` + `fork()` + `execvp()`. Slave: `setsid()`, `ioctl(TIOCSCTTY)`, `dup2` stdin/stdout/stderr. Resize: `ioctl(TIOCSWINSZ)`. Signal: SIGCHLD. Fallback: `open("/dev/ptmx")` + `ptsname()` if `posix_openpt` unavailable. (b) Windows: `CreatePseudoConsole()` (Win10 1809+). `STARTUPINFOEX` with `PPROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`. Overlapped pipe I/O. Resize: `ResizePseudoConsole()`. Runtime detection: if `CreatePseudoConsole` absent → error "ConPTY requires Windows 10 1809+". (c) Feature-gated: `#[cfg(feature = "terminal")]`. (d) `exec ls -la` returns directory listing on Linux; `dir` works on Windows. (e) Graceful shutdown: closing WS sends SIGHUP (Unix) / `TerminateProcess` (Windows), `waitpid` confirms exit.
- **Tests**: Unix PTY open/read/write/resize/close; Windows ConPTY create/read/write/resize/close; executable not found; command fails (exit code !=0); long output >64KB.

### T-54: src/server/ping_*.rs — ICMP + TCP + HTTP ping
- **Phase**: P5
- **Files**: `src/server/ping_icmp.rs`, `src/server/ping_tcp.rs`, `src/server/ping_http.rs`
- **Lines**: ~190 (85 + 45 + 60)
- **Priority**: P2 (feature-gated)
- **Effort**: L
- **Depends on**: T-52 (dns)
- **S.U.P.E.R**: S (one ping protocol per file), U (directory of ping files), E (CAP_NET_RAW for ICMP, graceful degradation), R (feature-gated: `ping`)
- **Acceptance**: (a) ICMP: raw socket (`SOCK_RAW`, `IPPROTO_ICMP`). Build ICMP echo request (type=8, code=0, checksum). Windows: `IcmpSendEcho2` via iphlpapi. Timeout + RTT. Without CAP_NET_RAW → return error "permission denied, use TCP/HTTP ping". (b) TCP: `TcpStream::connect_timeout()` → measure connect time as RTT. Closed port → error. (c) HTTP: manual HTTP/1.1 GET/HEAD with timeout. Status 2xx/3xx → RTT measured. (d) 3-tier dispatch: ICMP fails → TCP → HTTP. (e) Ping result format: `{"task_id":N,"ping_type":"icmp","value":23,"finished_at":"..."}`. Value = -1 on failure, finished_at = RFC 3339 nano.
- **Tests**: ICMP checksum; ICMP echo reply parse; TCP connect timeout; HTTP status handling; ping type dispatch; permission error handling.

### T-55: src/server/cf_access.rs — Cloudflare Access headers
- **Phase**: P5
- **Files**: `src/server/cf_access.rs`
- **Lines**: ~55
- **Priority**: P2
- **Effort**: S
- **Depends on**: T-12 (config)
- **S.U.P.E.R**: S (CF header injection only), U (single concern), E (header manipulation, no I/O)
- **Acceptance**: (a) When `cf_access_client_id` and `cf_access_client_secret` configured → WS upgrade + HTTP POST include `CF-Access-Client-Id` + `CF-Access-Client-Secret` headers. (b) When not configured → headers absent. (c) Header names + values match Go agent exactly.
- **Tests**: Header injection with config set; header absence without config; header value correctness.

### T-56: src/task.rs — ExecTask + PingTask types
- **Phase**: P5
- **Files**: `src/task.rs`
- **Lines**: ~40
- **Priority**: P1
- **Effort**: S
- **Depends on**: —
- **S.U.P.E.R**: S (task types only), U (pure data types), E (Copy where possible), R (no lock-in)
- **Acceptance**: (a) `ExecTask`: `task_id`, `command` fields. (b) `PingTask`: `ping_task_id`, `ping_type`, `ping_target` fields. (c) Task result upload format: `agent.taskResult` JSON. (d) Field names match Go agent: `task_id` (not `taskId`), `ping_task_id`, `ping_type`, `ping_target`.
- **Tests**: Deserialization from JSON-RPC event; task result format validation.

### T-57: src/server/task.rs (expand) — Real exec + ping implementations
- **Phase**: P5
- **Files**: `src/server/task.rs` (expand from T-35)
- **Lines**: ~130 (replaces stub, adds to T-35)
- **Priority**: P1
- **Effort**: L
- **Depends on**: T-35 (task stub), T-53 (terminal), T-54 (ping), T-56 (task types)
- **S.U.P.E.R**: S (task dispatch), U (dispatches to exec or ping), E (platform shell selection), R (TaskHandler trait if grows beyond 3 types)
- **Acceptance**: (a) Exec: Windows=`powershell -Command ...`, Unix=`sh -s`. `std::process::Command::output()` with timeout. (b) `handle_exec("echo hello")` → output="hello\n", exit_code=0. (c) Non-existent command → error, exit_code=-1. (d) `sleep 30` with timeout=1s → killed, exit_code=-1. (e) Ping: dispatches to ICMP/TCP/HTTP based on `ping_type`. (f) Task result uploaded via WS or HTTP POST. Wire format: `{"task_id":"...","result":"...","exit_code":...,"finished_at":"..."}`.
- **Tests**: Exec success; exec failure; exec timeout; shell selection per platform; ping dispatch; result upload format.

### T-58: src/update.rs — Self-update via GitHub releases
- **Phase**: P5
- **Files**: `src/update.rs`
- **Lines**: ~85
- **Priority**: P2 (feature-gated)
- **Effort**: M
- **Depends on**: T-12 (config), T-52 (dns)
- **S.U.P.E.R**: S (self-update only), U (depends on config + dns), E (platform-specific binary replace), R (feature-gated: `self-update`)
- **Acceptance**: (a) Version comparison: semver check via GitHub releases API. (b) Download asset matching current platform (`komari-agent-linux-amd64`, etc.). (c) SHA256 verification. (d) Unix: write to `.new`, `chmod +x`, `rename` (atomic). (e) Windows: `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)` + self-terminate. (f) Exit code 42 (matching Go agent per DD12). (g) Feature-gated.
- **Tests**: Version comparison; releases API response parse; platform asset name matching; SHA256 valid + invalid; binary replace simulation.

### T-59: tests/integration.rs — Full integration test
- **Phase**: P5
- **Files**: `tests/integration.rs`
- **Lines**: ~105
- **Priority**: P0
- **Effort**: L
- **Depends on**: T-37 (server + FSM), T-2B (monitor), T-57 (real task), all P5 tasks
- **S.U.P.E.R**: S (integration test only), U (tests full system), E (dummy server in-process), R (validates all subsystems together)
- **Acceptance**: (a) Dummy Komari server: `TcpListener`, WS upgrade (101 + accept key), receive heartbeat, send `agent.exec` event, receive `agent.taskResult`, send ping, receive pong, close connection, verify agent reconnects. (b) Full lifecycle: connect → heartbeat → task exec → disconnect → reconnect. (c) Runs in <30 seconds. (d) Dummy server and agent in same process (separate threads). (e) Must pass on all 4 platforms.
- **Tests**: This IS the integration test — the final gate for P1-P5.

### P6 — Polish + Packaging

### T-61: src/monitor/netstatic.rs — Persistent traffic history
- **Phase**: P6
- **Files**: `src/monitor/netstatic.rs`
- **Lines**: ~85
- **Priority**: P1
- **Effort**: M
- **Depends on**: T-26 (network types), T-14 (json)
- **S.U.P.E.R**: S (traffic history persistence only), U (uses network collector types), E (file I/O), R (self-contained)
- **Acceptance**: (a) `TrafficData` entries persisted to `net_static.json`. `VecDeque<TrafficData>` with max 720 entries (12h at 1/min). Oldest ejected on overflow. (b) Atomic write: write to `.tmp`, `rename` to final. On crash: tmp exists → use if valid + newer. (c) Startup: valid file → load; corrupted → discard + start fresh; missing → start fresh. (d) Month rotation per `month_rotate` config. (e) `get_total_traffic()` returns cumulative. (f) File format matches Go's `net_static.json` exactly.
- **Tests**: Serialize/deserialize; VecDeque overflow; atomic write/read; corruption recovery; month rotation; Go format compatibility.

### T-62: src/autodiscovery.rs — Auto-registration
- **Phase**: P6
- **Files**: `src/autodiscovery.rs`
- **Lines**: ~60
- **Priority**: P1
- **Effort**: M
- **Depends on**: T-12 (config), T-47 (os detection)
- **S.U.P.E.R**: S (auto-registration only), U (depends on config + os), E (HTTP POST + file I/O), R (self-contained)
- **Acceptance**: (a) `POST /api/agent/discover` with host info (hostname, OS, IP, agent_version). (b) Response 200 with agent ID → save to `auto-discovery.json`. (c) 4xx/5xx → retry after 60s. (d) Startup: `auto-discovery.json` exists → skip discovery. (e) Match Go agent: same endpoint, same JSON fields, same retry behavior.
- **Tests**: Payload format; response handling (200/4xx/5xx); config file persistence; restart skip logic.

### T-63: Windows toast notification
- **Phase**: P6
- **Files**: `src/platform/windows_toast.rs` (or integrated into `platform/windows.rs`)
- **Lines**: ~75
- **Priority**: P2
- **Effort**: M
- **Depends on**: T-12 (config), T-42 (Windows platform)
- **S.U.P.E.R**: S (toast notification only), P (Windows-only via cfg), E (Win32 toast API), R (MessageBoxW fallback)
- **Acceptance**: (a) Windows toast via `windows-rs` `ToastNotificationManager`. (b) Template: `ToastTemplateType_ToastText02`. (c) Fallback: `MessageBoxW` if toast API unavailable. (d) Content matches Go agent's Windows toast.
- **Tests**: Toast notification on Windows 10/11; MessageBoxW fallback; non-Windows compiles empty.

### T-64: Install scripts
- **Phase**: P6
- **Files**: `scripts/install.sh`, `scripts/install.ps1`
- **Lines**: ~85 (45 + 40)
- **Priority**: P1
- **Effort**: M
- **Depends on**: All prior phases (needs release binary)
- **S.U.P.E.R**: S (install only), E (platform-specific install), R (standalone scripts)
- **Acceptance**: (a) `install.sh`: Linux/macOS. Detect OS, download binary from GitHub releases, install to `/usr/local/bin/komari-agent`, create systemd/launchd service, start. (b) `install.ps1`: Windows. Download, extract to `$env:ProgramFiles\komari-agent`, create scheduled task or nssm service, start. (c) `set -euo pipefail` (bash), `Set-StrictMode -Version Latest` (PowerShell). (d) Script runtime <60s. (e) Agent heartbeats after scripted install. (f) Service management matches Go agent: same service name, same restart policy, exit code 42 = restart after update.
- **Tests**: End-to-end on fresh VM per platform.

### T-65: Documentation
- **Phase**: P6
- **Files**: `README.md`, `docs/protocol.md`, `docs/building.md`
- **Lines**: ~180 (80 + 60 + 40)
- **Priority**: P1
- **Effort**: M
- **Depends on**: All prior phases (documents implementation)
- **S.U.P.E.R**: S (documentation only), E (platform-agnostic knowledge)
- **Acceptance**: (a) README: overview, features, platform support, installation, configuration (all 34 flags with env vars), building from source, architecture overview, troubleshooting. (b) `docs/protocol.md`: v2 JSON-RPC methods, v1 fallback format, heartbeat schema, task schema, ping schema. (c) `docs/building.md`: Rust toolchain requirements, platform toolchains, cross-compilation, CI. (d) All links valid. All code examples correct. (e) Covers all 4 platforms.
- **Tests**: Link validation; code example execution.

### T-66: Final verification — All gates re-checked
- **Phase**: P6
- **Files**: (no new files)
- **Lines**: —
- **Priority**: P0
- **Effort**: L
- **Depends on**: All tasks T-11 through T-65
- **S.U.P.E.R**: S (verification only), all principles
- **Acceptance**: (a) All P1-P6 tests pass. (b) Real Komari server integration: connect, heartbeat, monitor, basicInfo, exec, ping, terminal, disconnect, reconnect. (c) Binary: <1 MB stripped linux-amd64, <1.2 MB windows-amd64. (d) RSS <3 MB after 60s. (e) Tick jitter <50ms. (f) Zero alloc verified by dhat. (g) `cargo fmt --check` clean. `cargo clippy -- -D warnings` clean. (h) No `unsafe` except FFI modules with `// SAFETY:` comments. (i) All 4 CI platforms green. (j) Git tag `v0.1.0` created.
- **Tests**: Full regression suite; binary size check; RSS measurement; zero-alloc validation; recorded session replay.

---

## 2. Parallel Lanes Per Phase

### Phase 1 — 4 Parallel Lanes

```
              ┌─────────────────────────────────┐
              │          P1: Foundation          │
              └─────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
   Lane 1A (config+crypto)  Lane 1B (json+tls)  Lane 1C (project config)
   ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐
   │ T-12 config.rs    │  │ T-14 json.rs     │  │ T-11 Cargo.toml + CI │
   │ T-13 crypto.rs    │  │ T-15 tls.rs      │  │                      │
   └────────┬─────────┘  └────────┬─────────┘  └──────────────────────┘
            │                     │
            └──────────┬──────────┘
                       │
                  Lane 1D (protocol types)
                  ┌──────────────────────┐
                  │ T-18 protocol/v2+v1  │
                  └──────────┬───────────┘
                             │
                  ┌──────────┴───────────┐
                  │ Lane 1E (transport)  │
                  │ T-16 ws.rs           │
                  │ T-17 http.rs         │
                  └──────────┬───────────┘
                             │
                  ┌──────────┴───────────┐
                  │ Lane 1F (integration)│
                  │ T-19 server/mod.rs   │
                  │ T-1A app+main.rs     │
                  └──────────────────────┘

Lanes 1A, 1B, 1C can all run simultaneously.
Lane 1D can run simultaneously with 1A+1B (only needs json.rs).
Lane 1E waits for 1A (crypto) + 1B (tls). Lane 1F waits for 1E + 1D.
```

### Phase 2 — 4 Parallel Lanes

```
              ┌─────────────────────────────────┐
              │     P2: Linux Metrics + Arena    │
              └─────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┬─────────────────────┐
        │                     │                     │                     │
   Lane 2A (foundation)  Lane 2B (cpu+mem)   Lane 2C (disk+net)   Lane 2D (small collectors)
   ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐
   │ T-21 arena.rs    │  │ T-23 cpu/linux   │  │ T-25 disk/linux  │  │ T-27 load/linux          │
   │ T-22 platform/   │  │ T-24 mem/linux   │  │ T-26 net/linux   │  │ T-28 connections/linux   │
   └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘  │ T-29 process/linux       │
            │                     │                     │             │ T-2A uptime/linux        │
            └─────────────────────┼─────────────────────┘             └────────────┬─────────────┘
                                  │                                              │
                                  └──────────────┬───────────────────────────────┘
                                                 │
                                         Lane 2E (integration)
                                         ┌──────────────────────────┐
                                         │ T-2B monitor/mod.rs      │
                                         │ T-2C server/mod expand   │
                                         └──────────────────────────┘

Lanes 2A, 2B, 2C, 2D can ALL run simultaneously (all depend on T-21 arena.rs which Lane 2A produces).
Lane 2E waits for all 4 collector lanes.
```

### Phase 3 — 3 Parallel Lanes

```
              ┌─────────────────────────────────┐
              │      P3: Protocol FSM + Fallback │
              └─────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
   Lane 3A (FSM+backoff)  Lane 3B (HTTP+task)  Lane 3C (protocol builders)
   ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐
   │ T-31 protocol/   │  │ T-34 http expand │  │ T-36 protocol/v2 expand  │
   │     fsm.rs       │  │ T-35 task stub   │  │                          │
   │ T-32 server/     │  │                  │  │                          │
   │     backoff.rs   │  │                  │  │                          │
   └────────┬─────────┘  └────────┬─────────┘  └──────────┬───────────────┘
            │                     │                        │
            └──────────┬──────────┘                        │
                       │                                   │
                  Lane 3D (reconnection)                   │
                  ┌──────────────────┐                     │
                  │ T-33 server/     │◄────────────────────┘
                  │     reconnection │  (needs T-31 fsm)
                  └────────┬─────────┘
                           │
                  ┌────────┴─────────┐
                  │ Lane 3E (server  │
                  │ T-37 integration)│
                  └──────────────────┘

Lanes 3A, 3B, 3C can all run simultaneously.
Lane 3D waits for 3A (needs FSM + backoff). Lane 3E waits for 3A+3B+3C+3D.
```

### Phase 4 — 7 Parallel Lanes (Embarrassingly Parallel)

```
              ┌─────────────────────────────────────────────┐
              │          P4: Cross-Platform Metrics           │
              └─────────────────────────────────────────────┘
                                    │
    ┌───────┬───────┬───────┬───────┼───────┬───────┬───────┐
    │       │       │       │       │       │       │       │
   LL1    LL2     LL3     LL4     LL5     LL6     LL7    (all simultaneous)
    │       │       │       │       │       │       │       │
   Linux  Windows macOS  FreeBSD  GPU     GPU    GPU+OS+
   IP+    ALL 9   ALL 9   ALL 9  Linux   Windows  Virt+CI
   dispatch
    │       │       │       │       │       │       │       │
   T-41   T-42    T-43    T-44    T-45    T-46    T-47
  ~120L  ~570L   ~390L   ~390L   ~190L   ~115L   ~375L

Lane 1: Linux IP detection + platform dispatch expansion
Lane 2: Windows CPU + Memory + Disk + Network + Load + Connections + Process + Uptime + IP
Lane 3: macOS CPU + Memory + Disk + Network + Load + Connections + Process + Uptime + IP
Lane 4: FreeBSD CPU + Memory + Disk + Network + Load + Connections + Process + Uptime + IP
Lane 5: GPU Linux (nvidia-smi + rocm-smi + DRM fallback)
Lane 6: GPU Windows (DXGI via windows-rs)
Lane 7: GPU macOS + GPU FreeBSD + OS detection + Virtualization detection + CI expansion

ALL 7 LANES CAN RUN SIMULTANEOUSLY.
No cross-lane dependencies — each platform is fully independent.
```

### Phase 5 — 5 Parallel Lanes

```
              ┌─────────────────────────────────────────────┐
              │   P5: Terminal + Ping + Gzip + DNS + Update  │
              └─────────────────────────────────────────────┘
                                    │
    ┌───────────┬───────────┬───────────┬───────────┬───────────┐
    │           │           │           │           │           │
  Lane 5A    Lane 5B    Lane 5C    Lane 5D    Lane 5E    (all simultaneous)
    │           │           │           │           │           │
  Gzip + DNS  Terminal   Terminal   Ping all   CF + Update
              Unix       Windows   3 variants
    │           │           │           │           │           │
  T-51       T-53       T-53       T-54       T-55
  T-52       (unix.rs)  (win.rs)              T-56
  T-56                                          T-58
    │           │           │           │           │           │
    └───────────┴───────────┴───────────┴───────────┴───────────┘
                                    │
                            Lane 5F (integration)
                            ┌──────────────────┐
                            │ T-57 task expand │
                            │ T-59 integration │
                            └──────────────────┘

Lanes 5A, 5B, 5C, 5D, 5E can all run simultaneously.
Lane 5F waits for all 5 parallel lanes.
```

### Phase 6 — 4 Parallel Lanes (Fully Parallel)

```
              ┌─────────────────────────────────┐
              │        P6: Polish + Packaging    │
              └─────────────────────────────────┘
                              │
    ┌─────────────┬─────────────┬─────────────┬─────────────┐
    │             │             │             │             │
  Lane 6A      Lane 6B      Lane 6C      Lane 6D      Lane 6E
    │             │             │             │             │
  Netstatic   Autodiscovery Windows    Install    Documentation
                          Toast       Scripts
    │             │             │             │             │
  T-61         T-62         T-63         T-64        T-65
    │             │             │             │             │
    └─────────────┴─────────────┴─────────────┴─────────────┘
                              │
                      Lane 6F (final verify)
                      ┌──────────────────┐
                      │ T-66 all gates   │
                      └──────────────────┘

ALL 5 P6 lanes can run simultaneously. Lane 6F waits for all.
```

---

## 3. Dependency Chain — Critical Path

The longest chain of sequentially-dependent files from task 1 to task N. This is the **minimum delivery time** regardless of parallelization.

### 3.1 File-Level Critical Path

```
Step    Task    File                          Phase    Lines    Depends On
────    ────    ────                          ─────    ─────    ──────────
  1     T-11    Cargo.toml                     P1       35     —
  2     T-12    src/config.rs                  P1      150     —
  3     T-14    src/json.rs                    P1      105     —           ← JsonBuf API locked here
  4     T-18    src/protocol/v2.rs             P1       65     json.rs
  5     T-16    src/ws.rs                      P1       95     config + crypto + tls
  6     T-19    src/server/mod.rs              P1       95     ws + protocol
  7     T-21    src/arena.rs                   P2      140     —
  8     T-23    src/monitor/cpu/linux.rs       P2      100     arena + json
  9     T-24    src/monitor/mem/linux.rs       P2      190     arena + json + config
 10     T-2B    src/monitor/mod.rs             P2       55     all Linux collectors
 11     T-2C    src/server/mod.rs (expand)     P2       40     monitor/mod
 12     T-31    src/protocol/fsm.rs            P3      120     protocol/v2, protocol/v1
 13     T-32    src/server/backoff.rs          P3       50     —
 14     T-33    src/server/reconnection.rs     P3       70     fsm + backoff
 15     T-37    src/server/mod.rs (expand)     P3       60     reconnection + http + task
 16     T-41    src/monitor/ip/linux.rs        P4       55     monitor/mod + platform
 17     T-51    src/gzip.rs                    P5      200     json.rs
 18     T-52    src/dns.rs                     P5      165     config.rs
 19     T-53    src/terminal/unix.rs           P5      140     — (feature-gated)
 20     T-54    src/server/ping_icmp.rs        P5       85     dns.rs
 21     T-57    src/server/task.rs (expand)    P5      130     ping_* + terminal + task_types
 22     T-59    tests/integration.rs           P5      105     all of above
 23     T-62    src/autodiscovery.rs           P6       60     config + os
 24     T-61    src/monitor/netstatic.rs       P6       85     network + json
 25     T-66    Final verification             P6       —      all tasks
```

**Critical path**: 25 sequential steps. **Chain**: json.rs → protocol/v2.rs → ws.rs → server/mod.rs → monitor/mod.rs → platform files → terminal/ping/gzip.

**Critical path duration** (single developer): 17-23 working days.

### 3.2 Phase-Level Critical Path

```
P1 (600 lines, 2-3d) → P2 (750 lines, 4-5d) → P3 (500 lines, 3-4d) → P5 (1,045 lines, 6-8d) → P6 (510 lines, 2-3d)
                                                                     ↗
                                                          P4 (1,535 lines, 6-8d) can parallelize with P3
```

**With P3/P4 parallelization**: 15-19 elapsed days (2 developers).

---

## 4. S.U.P.E.R Checkpoints

S = Single Responsibility | U = Uncoupled | P = Portable | E = Environment-Independent | R = Replaceable

### After Phase 1 — S + P verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **S** (Single) | Each file has one responsibility: config.rs (config only), crypto.rs (SHA-1+base64 only), json.rs (JSON encode only), ws.rs (WS handshake only) | 10 files, each <200 lines, single named concern |
| **P** (Protocol contracts) | JSON-RPC 2.0 wire format, WebSocket handshake RFC 6455, SHA-1 RFC 3174, base64 RFC 4648 | All test vectors pass. `Sec-WebSocket-Accept` computed correctly. |

### After Phase 2 — S + E verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **S** (Single) | Each collector file handles one metric only (cpu/*.rs, mem/*.rs, etc.) | 9 collector files, each <150 lines |
| **E** (Env-Agnostic) | Zero heap allocation in tick() hot path; arena-backed reads; SmallVec inline storage. No global state. | `dhat` shows zero allocs. Arena 8KB sufficient. RSS <3 MB after 60s. |

### After Phase 3 — U verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **U** (Unidirectional flow) | FSM is a pure state machine (no I/O). Backoff is pure math (no I/O). Reconnection loop has single-direction event dispatch. Metrics flow: collect → encode → send (no back-pressure loops). | 12 FSM transitions tested. Recorded session replay passes byte-for-byte. No circular dependencies. |

### After Phase 4 — E + R verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **E** (Env-Agnostic) | Per-platform collectors behind `cfg(target_os)`. OS native TLS roots (no bundled certs). Platform-specific exec tools isolated in feature-gated modules. | CI green on all 4 platforms. No platform-specific code in shared modules. GPU detection gracefully degrades when tools missing. |
| **R** (Replaceable) | Each platform file is replaceable independently. GPU detection behind feature gate — can swap nvidia-smi approach for direct NVML FFI without touching other files. OS detection heuristics can be updated per-platform. | All platform files are `cfg`-gated. Removing a platform = delete one directory. |

### After Phase 5 — R verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **R** (Replaceable parts) | Feature gates (`gpu-detection`, `terminal`, `ping`, `self-update`). Self-implemented gzip + DNS + crypto (zero external deps in hot path). Trait-based Terminal (swap PTY impls without touching server). Ping 3-tier (ICMP→TCP→HTTP) — any tier replaceable. | All feature combinations compile. Terminal trait abstracts platform PTY. Ping dispatch abstracts protocol. Gzip encode-only (no decompression dep). |

### After Phase 6 — ALL principles verified

| Principle | Verified By | Evidence |
|-----------|------------|----------|
| **S** (Single) | 58% of modules Grade A. No module >200 lines except gzip (200, per DD9), terminal (370, 2 platforms). | S.U.P.E.R aggregate: 17.9/20 (Grade A). |
| **U** (Uncoupled) | Explicit `&Config` passing (no global). No circular dependencies (Go had 1: monitoring→netstatic→monitoring). | `cargo tree` shows tree-shaped dep graph. |
| **P** (Portable) | cfg-gated platform dispatch. Feature-gated optional capabilities. 4-OS CI matrix all green. | `--no-default-features`: 600 KB. `--features full`: ~876 KB. All <1 MB. |
| **E** (Environment-Independent) | OS native roots (no bundled certs). Arena + SmallVec (no global allocator coupling). No async runtime. Hand-written JSON + gzip (no serde/flate2). | Binary <1 MB stripped. RSS <3 MB. Zero alloc in hot path. |
| **R** (Replaceable) | Self-implemented crypto + JSON + gzip. Trait-based terminal + task dispatch. Feature gates for everything non-core. tungstenite thin wrapper (95 lines). | Core agent (~600 KB) has zero external deps in hot path. All optional features can be compiled out. |

---

## 5. Resolved Conflicts

All conflicts found during cross-review synthesis of the 4 decompositions and 3 cross-reviews.

### Conflict #1: CLI Parsing (clap vs hand-written)

| Source A | Source B | Conflict |
|----------|----------|----------|
| `architecture-reference.md` §8.2 + `dependency-graph.md` §7.2 (DD-010: use clap, ~25 KB overhead) | `spec.md` DD1 (hand-written ~150 lines, saves ~15 KB + unicode deps) | clap adds binary size; spec mandates hand-written for <1 MB target |

**Resolution**: **Hand-written CLI wins** (spec DD1 takes priority per `acceptance-criteria.md` §0.3). Manual `std::env::args()` iteration + `std::env::var()` for env override. No clap. T-12 updated to 150 lines.

**Impact**: T-12 effort changes from S (clap derive) to L (manual parser). Binary saves ~15 KB.

### Conflict #2: TLS Certificate Store (webpki-roots vs OS native)

| Source A | Source B | Conflict |
|----------|----------|----------|
| `architecture-reference.md` App.A + `dependency-graph.md` (webpki-roots, ~100 KB blob) | `spec.md` DD5 (OS native roots: Linux=`/etc/ssl/certs`, Windows=CryptoAPI, macOS=Security.framework) | webpki-roots adds ~100 KB; spec mandates OS native for <1 MB target |

**Resolution**: **OS native roots win** (spec DD5). Use `rustls-platform-verifier` crate or manual FFI per platform. No `webpki-roots`. T-15 updated to reflect platform-specific cert store access.

**Impact**: T-15 complexity increases slightly (platform FFI for cert store). Binary saves ~80 KB.

### Conflict #3: Gzip Encoder Size (450 vs 200 lines)

| Source A | Source B | Conflict |
|----------|----------|----------|
| `architecture-reference.md` §2.2 (gzip.rs: 450 lines, CRC32 table 1 KB, LZ77 hash chain, BitWriter) | `spec.md` DD9 (fixed-Huffman encode-only ~200 lines, no full DEFLATE) | 450 lines vs 200 lines — scope disagreement |

**Resolution**: **~200 lines max** (spec DD9). Fixed Huffman with pre-computed code tables (RFC 1951 BTYPE=01). CRC32 as small const array (256×u32 = 1 KB). No LZ77 hash chain — use stored blocks (BTYPE=00) for payloads <200 bytes and fixed Huffman for larger. Accept 3-5% worse compression ratio.

**Impact**: Saves ~250 lines. Gzip module simpler. Compression ratio 3-5% worse than what architecture-reference planned, but still valid gzip accepted by server.

### Conflict #4: WebSocket Implementation (tungstenite vs manual frames)

| Source A | Source B | Conflict |
|----------|----------|----------|
| `architecture-reference.md` DD-009 (tungstenite sync mode, ~30 KB) | `spec.md` DD3 + `acceptance-criteria.md` T-1.6 (manual frame codec ~350 lines) | tungstenite is simpler and correct; spec implies manual for binary size |

**Resolution**: **tungstenite wins** (per architecture DD-009 + R5 resolution). Tungstenite handles frame masking, fragmentation, ping/pong, and close handshake correctly per RFC 6455. A manual frame codec would be ~350 lines with higher bug risk. The ~30 KB binary cost is acceptable (total still <1 MB). Per `acceptance-criteria.md` §0.3, DD3's manual frame codec intent was superseded by DD-009.

**Impact**: T-16 uses tungstenite. Saves ~250 lines of manual frame codec. Adds ~30 KB binary (net of tungstenite overhead) but with proven RFC 6455 compliance.

### Conflict #5: Feature Gate Default

| Source A | Source B | Conflict |
|----------|----------|----------|
| `architecture-reference.md` App.A (no feature gates mentioned in Cargo.toml — all features compiled) | `spec.md` DD13 (`default=[]`, core ~600 KB, `full` enables all ~876 KB) | Architecture ref compiles everything; spec mandates feature gates |

**Resolution**: **`default=[]` wins** (spec DD13). Core agent compiles only monitoring + v1/v2 protocol + HTTP fallback. `gpu-detection`, `terminal`, `ping`, `self-update` behind feature gates. Binary budget: ~600 KB core, ~876 KB full.

**Impact**: Cargo.toml must define features. Every platform and optional module must be feature-gated. CI must test all feature combinations.

### Conflict #6: Phase 1 Line Count (500 vs 600)

| Source A | Source B | Conflict |
|----------|----------|----------|
| `phased-implementation-plan.md` (P1 "~600") | File breakdown sums to ~500 (35+150+195+105+40+95+70+115+95+95 = ~995? No, that's too many...) | Line count discrepancy |

Recalculation: P1 files: Cargo.toml(35) + config.rs(150) + crypto.rs(195) + json.rs(105) + tls.rs(40) + ws.rs(95) + http.rs(70) + protocol/(65+45+5) + server/mod.rs(95) + app.rs(80) + main.rs(15) + ci.yml(42) = **1,037 lines**.

But phased-plan says "~600" for P1. The phased-plan line estimates are conservative — the 600 figure likely counts only code lines (excluding comments, blanks, test code) and the architecture reference files add structure (mod.rs files, trait definitions).

**Resolution**: P1 tasks = ~600 lines of implementation code (excluding comments, blanks, inline tests). File-level estimates in the task table include those overheads. Total P1 lines for budget = 600.

### Conflict #7: Missing Dependency — platform/mod.rs depends on T-22 before T-41 can proceed

| Found by | Issue |
|----------|-------|
| Cross-review #2 (dependency-graph.md §2.2 step 7) | `platform/mod.rs` must exist before any P4 platform module can compile |

**Resolution**: T-22 (`src/platform/mod.rs` + `src/platform/linux.rs`) is already scheduled in P2 (Lane 2A). P4 lanes 2-7 all depend on T-22 being complete. This dependency is explicit in the task table.

### Conflict #8: Missing Dependency — monitor/mod.rs must have platform dispatch before P4 collectors

| Found by | Issue |
|----------|-------|
| Cross-review #1 (bottleneck analysis §4.1) | `monitor/mod.rs` has 15 dependents. P4 collectors (Windows/macOS/FreeBSD) must plug into the tick() orchestrator |

**Resolution**: T-2B (monitor/mod.rs) defines the tick loop contract in P2. P4 collectors follow the same interface (function signature: `collect(&arena, &config) -> MetricStruct`). T-41 (Lane 1) includes expanding the platform dispatch in `monitor/mod.rs` and child `mod.rs` files (`monitor/load/mod.rs`, etc.) to route to the correct platform.

### Conflict #9: Missing Dependency — gzip.rs must exist before http.rs can send compressed POST

| Found by | Issue |
|----------|-------|
| Cross-review #1 (dependency graph, P5→P3 back-edge) | `http.rs` uses gzip for `Content-Encoding: gzip` in HTTP POST fallback. But gzip.rs is Phase 5, and HTTP POST fallback is Phase 3. |

**Resolution**: In Phase 3, HTTP POST fallback sends uncompressed JSON (no `Content-Encoding` header). In Phase 5, T-51 (gzip) + T-34 (http.rs expand) add gzip compression. The architecture reference already notes this: "HTTP POST without gzip in P3; add gzip in P5." Accept this two-phase approach.

---

## 6. Total Estimates

### 6.1 Line Count Summary

| Phase | Task IDs | Files | Lines (code) | Lines (total incl. comments/tests) |
|-------|----------|-------|:------------:|:----------------------------------:|
| P1 | T-11 to T-1A (10 tasks) | 20 | ~600 | ~1,037 |
| P2 | T-21 to T-2C (12 tasks) | 18 | ~750 | ~1,070 |
| P3 | T-31 to T-37 (7 tasks) | 8 | ~500 | ~665 |
| P4 | T-41 to T-47 (7 tasks) | 38 | ~1,535 | ~1,920 |
| P5 | T-51 to T-59 (9 tasks) | 18 | ~1,045 | ~1,415 |
| P6 | T-61 to T-66 (6 tasks) | 10 | ~510 | ~680 |
| **Total** | **51 tasks** | **~112 files** | **~4,940** | **~6,787** |

### 6.2 Effort Summary

| Effort | Count | Tasks |
|--------|:-----:|-------|
| S (Small, <2h) | 10 | T-11, T-1A, T-22, T-27, T-28, T-29, T-2A, T-55, T-56, T-63 |
| M (Medium, 2-4h) | 16 | T-15, T-17, T-18, T-2C, T-32, T-34, T-35, T-36, T-58, T-61, T-62, T-64, T-65, T-41, T-52, T-54 |
| L (Large, 4-8h) | 19 | T-12, T-13, T-14, T-16, T-19, T-21, T-23, T-25, T-26, T-2B, T-31, T-33, T-43, T-44, T-45, T-46, T-51, T-57, T-59, T-66 |
| XL (Extra Large, 1-2d) | 6 | T-24, T-37, T-42, T-47, T-53 |
| **Total effort** | **51 tasks** | **~178 hours (~23 working days serial)** |

### 6.3 Calendar Time Estimates

| Scenario | Developers | Elapsed Time | How |
|----------|:---------:|:-----------:|-----|
| **Serial** (single developer) | 1 | **23-31 days** | P1(3d) → P2(5d) → P3(4d) → P4(8d) → P5(8d) → P6(3d) |
| **Phase-level parallel** (P3∥P4) | 2 | **18-25 days** | Dev A: P1→P2→P3→P5→P6. Dev B: P4 (parallel with P3). Join on P5. |
| **Aggressive intra-phase parallel** | 3-4 | **12-16 days** | P1(2d with 3 lanes) → P2(3d with 4 lanes) → P3(2d) + P4(3d with 7 lanes) → P5(3d with 5 lanes) → P6(2d) |
| **Theoretical maximum** (all lanes staffed) | 6+ | **8-10 days** | P1(1.5d) → P2(2d) → P3∥P4(2.5d) → P5(2d) → P6(1d) |

### 6.4 Per-Phase Calendar Breakdown (Single Developer, Serial)

| Phase | Tasks | Lines | Serial Days | With Max Parallel | Speedup |
|-------|:-----:|:-----:|:-----------:|:-----------------:|:-------:|
| P1 | 10 | 600 | 2-3 | 1.5 | 1.7× |
| P2 | 12 | 750 | 4-5 | 2 | 2.3× |
| P3 | 7 | 500 | 3-4 | 2 | 1.8× |
| P4 | 7 | 1,535 | 6-8 | 3 (7 lanes simultaneous) | 2.3× |
| P5 | 9 | 1,045 | 6-8 | 3 | 2.3× |
| P6 | 6 | 510 | 2-3 | 1 | 2.5× |
| **Total** | **51** | **4,940** | **23-31** | **12.5** | **~2.0×** |

### 6.5 Binary Size Budget Per Phase

| Phase | linux-amd64 | windows-amd64 | macos-amd64 | freebsd-amd64 |
|-------|:-----------:|:-------------:|:-----------:|:-------------:|
| P1 (Foundation) | <2.0 MB | <2.5 MB | <2.5 MB | <2.5 MB |
| P2 (Linux Metrics) | <1.5 MB | <2.0 MB | <2.0 MB | <2.0 MB |
| P3 (Protocol FSM) | <1.3 MB | <1.8 MB | <1.8 MB | <1.8 MB |
| P4 (Cross-Platform) | <1.5 MB | <2.0 MB | <2.0 MB | <2.0 MB |
| P5 (Terminal+Ping+Tools) | <1.2 MB | <1.5 MB | <1.5 MB | <1.5 MB |
| **P6 (Final, --features full)** | **<1.0 MB** | **<1.2 MB** | **<1.2 MB** | **<1.2 MB** |
| P6 (`--no-default-features`) | **<700 KB** | **<850 KB** | **<850 KB** | **<850 KB** |

### 6.6 Cumulative Size Reduction

At P6 final, binary size is **<1 MB** (linux-amd64 stripped, `--features full`). Core (`--no-default-features`): **<700 KB**. This is achieved through:

- No async runtime (saves ~1 MB vs tokio)
- Hand-written JSON encoder (saves ~300 KB vs serde)
- Custom gzip encoder (saves ~20 KB vs flate2)
- Self-implemented SHA-1 + base64 (saves ~5 KB vs crypto crates)
- OS native roots (saves ~80 KB vs webpki-roots)
- Hand-written CLI (saves ~15 KB vs clap)
- `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`

---

## Appendix A: Task Priority Legend

| Priority | Meaning | Gate |
|----------|---------|------|
| **P0** | Blocking. Must pass before next task can start. | Fails CI/build if not met. |
| **P1** | High. Must pass within current phase. | Phase gate checklist item. |
| **P2** | Medium. Should pass within current phase. Feature-gated or non-blocking. | Phase gate checklist item (deferred if feature-gated). |

## Appendix B: Feature Gate Cross-Reference

| Feature | Cargo flag | Tasks requiring it |
|---------|-----------|-------------------|
| Core monitoring | (always compiled) | T-11 through T-37, T-41 through T-44, T-47 (OS+virt+CI), T-61, T-62, T-65, T-66 |
| `gpu-detection` | `--features gpu-detection` | T-45, T-46, T-47 (GPU macOS/FreeBSD) |
| `terminal` | `--features terminal` | T-53 |
| `ping` | `--features ping` | T-54 |
| `self-update` | `--features self-update` | T-58 |
| `full` | `--features full` (enables all) | All tasks |

## Appendix C: Wire Compatibility Checkpoints

These JSON fields must be byte-identical between Go agent and Rust agent outputs:

| Field Path | Go Value (example) | Tolerance |
|-----------|-------------------|-----------|
| `cpu.usage` | 12.50 | Exact (2 decimal places) |
| `ram.total` | 17179869184 | Exact |
| `ram.used` | varies | Exact (same mem_mode) |
| `swap.total` | 2147483648 | Exact |
| `load.load1` | 0.25 | Exact (2 decimal places) |
| `network.up` | 125000.0 | Exact (1 decimal place) |
| `connections.tcp` | 42 | Exact |
| `uptime` | 86400 | Within 1s |
| `process` | 234 | Within 5 |
| `gpu.name` | "NVIDIA GeForce RTX 4090" | Exact string match |
| `os_name` | "Ubuntu 22.04.3 LTS" | Exact string match |
| `ping_type` | "icmp" | Exact string match |
| `value` | 23 | Exact (ms, -1 on failure) |
| `exit_code` | 0 | Exact |
| `finished_at` | "2024-06-20T12:00:05.123456789Z" | RFC 3339 nano format |

---

**Document end.** This task-breakdown.md synthesizes all 4 decompositions (P1+P2, P3, P4, P5+P6 from `phased-implementation-plan.md` and `acceptance-criteria.md`) and 3 cross-reviews (`dependency-graph.md`, `architecture-reference.md`, `spec.md`). It resolves 9 conflicts found during synthesis and provides the definitive task list, parallel lanes, critical path, S.U.P.E.R checkpoints, and total estimates for komari-agent-rs implementation.
