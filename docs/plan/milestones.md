# Komari-Agent-RS: Milestones, Acceptance Criteria & Validation Report

**Version**: 1.0.0
**Date**: 2026-06-20
**Status**: Merged SSOT -- supersedes individual `spec.md`, `phased-implementation-plan.md`, `acceptance-criteria.md`, `dependency-graph.md`, `architecture-reference.md` for milestone tracking.

**Hard Constraints (from spec.md)**:

| Constraint | Target |
|-----------|:------:|
| Binary (Linux stripped) | < 1 MB |
| Steady-state RSS | < 3 MB |
| Hot-path heap allocations | 0 |
| External deps | rustls + ring crypto (+ webpki-roots ~80KB exempt) |
| Concurrency model | Sync single-threaded |
| License | MIT |

---

## 1. MILESTONE TABLE

Each milestone maps to one implementation phase. All gate criteria are quantified and testable.

### M1: Foundation + Handshake

| Attribute | Detail |
|-----------|--------|
| **Objective** | Binary compiles on 4 OS targets. Establishes TLS WebSocket connection to Komari server. Sends static heartbeat JSON. All crypto + encoding fully unit-tested. CI pipeline green on empty project. |
| **Depends on** | Nothing (M1 is the root) |
| **Estimated days** | 2-3 |
| **Deliverables** | `Cargo.toml` + `.cargo/config.toml` + `.github/workflows/ci.yml`, `src/main.rs`, `src/app.rs`, `src/config.rs`, `src/crypto.rs`, `src/json.rs`, `src/tls.rs`, `src/ws.rs`, `src/http.rs` (stub), `src/protocol/v2.rs`, `src/protocol/v1.rs`, `src/protocol/mod.rs`, `src/server/mod.rs` (initial) |
| **Code budget** | ~600 lines |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G1.1 | `cargo build --release` succeeds on linux-amd64, windows-amd64, macos-amd64, freebsd-amd64 | CI matrix green |
| G1.2 | Binary size < 2.0 MB stripped on all platforms (Phase 1 threshold) | `ls -lh target/release/komari-agent` (strip symbols) |
| G1.3 | Agent connects to real Komari server, completes WS handshake (HTTP 101) | Manual integration test |
| G1.4 | Server receives valid static heartbeat JSON (`{"jsonrpc":"2.0","method":"report",...}`) | Server logs |
| G1.5 | SHA-1 RFC 3174 vectors pass: `"abc"` -> `a9993e36...`, `""` -> `da39a3ee...`, `"abcdbcde..."` -> `84983e44...` | `cargo test` |
| G1.6 | WebSocket accept key vector: `sha1(base64_decode("dGhlIHNhbXBsZSBub25jZQ==") + GUID)` -> `"s3pPLMBiTxaQ9kYGzzhZRbK+xOo="` | `cargo test` |
| G1.7 | Base64 RFC 4648 vectors (all 7) pass | `cargo test` |
| G1.8 | `cargo fmt --check` zero diffs | CI step |
| G1.9 | `cargo clippy -- -D clippy::all -D clippy::pedantic` clean (with allowed lints: `module_name_repetitions`, `cast_precision_loss`, `cast_sign_loss`, `cast_possible_truncation`, `must_use_candidate`) | CI step |
| G1.10 | No `unsafe` except in platform FFI / syscall wrappers | `grep -r "unsafe" src/` |
| G1.11 | Git commit at M1 boundary | `git log` |

**S.U.P.E.R Check**: Modules from Phase 1 score minimum B (13/20). Key: `json.rs` (20/A), `crypto.rs` (19/A), `config.rs` (19/A), `ws.rs` (16/B), `http.rs` (16/B). Average: >= 17 (A).

---

### M2: Linux Metrics + Zero-Alloc Loop

| Attribute | Detail |
|-----------|--------|
| **Objective** | Full Linux monitoring suite operational. ScratchArena + SmallVec allocators proven. Zero heap allocation in 1-second tick loop verified via `dhat`. RSS measured < 3 MB after 60s steady state. All /proc parsers behave identically to Go agent. |
| **Depends on** | M1 (Foundation) |
| **Estimated days** | 4-5 |
| **Deliverables** | `src/arena.rs`, `src/platform/mod.rs`, `src/platform/linux.rs`, `src/monitor/mod.rs`, `src/monitor/cpu/linux.rs`, `src/monitor/mem/linux.rs`, `src/monitor/disk/linux.rs`, `src/monitor/net/linux.rs`, `src/monitor/load/linux.rs`, `src/monitor/connections/linux.rs`, `src/monitor/process/linux.rs`, `src/monitor/uptime/linux.rs`, `src/server/mod.rs` (expand with tick loop) |
| **Code budget** | ~750 lines (cumulative 1,350) |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G2.1 | All Linux collectors produce JSON byte-identical in structure to Go agent (field names, types, nesting) | Diff against Go agent output on same host |
| G2.2 | 3-mode RAM calculation matches Go for modes 0, 1, 2 on same host (values within 1%) | Side-by-side run; compare `ram.used`, `ram.total` |
| G2.3 | `is_physical_disk()` produces identical mount list as Go agent on same host | Compare mount list; must match all 30+ exclude patterns |
| G2.4 | Zero heap allocation in `monitor.tick()` hot path -- verified via `dhat` or custom `#[global_allocator]` that panics/promotes on alloc | `cargo test zero_alloc::tick_has_zero_heap_allocations` |
| G2.5 | RSS < 3 MB after 60s steady state (measured via `/proc/<pid>/status VmRSS`) | `cat /proc/$PID/status \| grep VmRSS`; must be < 3072 KB |
| G2.6 | 1-second tick jitter < 50ms under idle system (100 consecutive ticks, measure max/mean/p99) | Internal tick timing instrumentation |
| G2.7 | All collector unit tests pass with fixture /proc data | `cargo test` |
| G2.8 | Binary size < 1.5 MB stripped linux-amd64 (Phase 2 threshold) | `ls -lh` |
| G2.9 | `cargo fmt --check` + `cargo clippy` clean | CI |
| G2.10 | Git commit at M2 boundary | `git log` |

**S.U.P.E.R Check**: `monitor/mod.rs` (14/B), `arena.rs` (20/A), all collectors average 18 (A). New modules minimum B.

---

### M3: Protocol FSM + Fallback

| Attribute | Detail |
|-----------|--------|
| **Objective** | Full v2/v1 protocol negotiation with 3-strike fallback. HTTP POST fallback operational. Exponential backoff with jitter (1s -> 5 min cap). Reconnection loop survives server restart. **Recorded session replay passes**: Rust agent output matches Go agent frame-by-frame when replaying identical server responses. |
| **Depends on** | M2 (Linux Metrics) |
| **Estimated days** | 3-4 |
| **Deliverables** | `src/protocol/fsm.rs`, `src/server/backoff.rs`, `src/server/reconnection.rs`, `src/http.rs` (expand to full POST), `src/server/task.rs` (stubs), `src/protocol/v2.rs` (expand builders), `src/server/mod.rs` (integrate FSM + backoff + reconnection) |
| **Code budget** | ~500 lines (cumulative 1,850) |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G3.1 | All 12 FSM transitions pass unit tests | `cargo test fsm::` |
| G3.2 | 3-strike counter: 3 consecutive v2 protocol errors triggers v1 fallback; 1 success resets counter to 0 | FSM unit test |
| G3.3 | HTTP POST fallback sends identical JSON body to WS mode (same `EncodeJson` path) | Byte diff WS frame vs POST body |
| G3.4 | Backoff: ~1s delay after 3 failures (with +/- 25% jitter); caps at 5 min after 10 failures | Backoff unit test with fixed PRNG seed |
| G3.5 | Reconnection loop survives server restart: agent detects disconnect within 30s (heartbeat timeout), reconnects within backoff window | Integration test with server kill/restart |
| G3.6 | Recorded session replay: Go agent session captured via tcpdump, replayed against Rust agent via dummy server; JSON output matches byte-for-byte (timestamps within 1s tolerance) | `tests/integration.rs` |
| G3.7 | Network errors (e.g., TCP RST) do NOT increment FSM protocol failure counter | FSM unit test `test_network_error_does_not_count` |
| G3.8 | Binary size < 1.3 MB stripped linux-amd64 (Phase 3 threshold) | `ls -lh` |
| G3.9 | `cargo fmt --check` + `cargo clippy` clean | CI |
| G3.10 | Git commit at M3 boundary | `git log` |

**S.U.P.E.R Check**: `protocol/fsm.rs` (20/A), `server/backoff.rs` (20/A), `server/reconnection.rs` (16/B). Average >= 16 (A-).

---

### M4: Cross-Platform Metrics

| Attribute | Detail |
|-----------|--------|
| **Objective** | Windows, macOS, FreeBSD metrics fully operational. GPU detection across all 4 platforms. OS name + kernel version detection. Virtualization/container detection. CI matrix fully green on all 4 platforms (build + test + clippy + fmt). All platform-specific code isolated behind `cfg(target_os)`. |
| **Depends on** | M2 (Linux Metrics). Can run **in parallel with M3** if two developers. |
| **Estimated days** | 6-8 (serial); 3-4 (with 3-4 developers on parallel fan-out) |
| **Deliverables** | `src/platform/{windows,macos,freebsd}.rs`, `src/monitor/{cpu,mem,disk,net,load,connections,process,uptime,ip}/{windows,macos,freebsd}.rs`, `src/monitor/gpu/{mod,linux,windows,macos,freebsd}.rs`, `src/monitor/os.rs`, `src/monitor/virtualization.rs`, `.github/workflows/ci.yml` (expand to full matrix) |
| **Code budget** | ~1,535 lines (cumulative 3,385) |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G4.1 | All 4 platform binaries pass `cargo test` in CI | CI matrix green |
| G4.2 | Windows collectors: CPU, memory (3-mode), disk, network, process, uptime values match Go agent on same host within 1% | Manual integration test on Windows |
| G4.3 | macOS collectors: CPU, memory, disk, load, process, uptime values within valid ranges | Manual integration test on macOS |
| G4.4 | FreeBSD collectors: compile and produce valid JSON output | CI build + QEMU/Cirrus CI test |
| G4.5 | GPU detection: NVIDIA (nvidia-smi path) produces same GPU names as Go agent on same host | Manual integration test |
| G4.6 | GPU detection: AMD (rocm-smi path) produces same GPU names as Go agent | Manual integration test |
| G4.7 | GPU detection: Intel/Apple Silicon (DRM/system_profiler fallback) correctly identifies GPU | Manual integration test |
| G4.8 | OS detection correctly identifies: Ubuntu 22.04, Debian 12, Windows 11, macOS 15, FreeBSD 14 | Unit tests with fixture data |
| G4.9 | Virtualization detection: Docker, KVM, VMware, Hyper-V all correctly identified | Unit tests with fixture data |
| G4.10 | No platform-specific code in shared modules (all behind `cfg(target_os)` or platform module re-exports) | `grep -r "cfg(target_os)" src/monitor/mod.rs` must be empty |
| G4.11 | Binary size < 1.5 MB stripped linux-amd64 (accommodates cross-platform code increase) | `ls -lh` |
| G4.12 | CI matrix: 4 OS x (build + test + clippy + fmt) all green | `.github/workflows/ci.yml` |
| G4.13 | All M1-M3 gate criteria still pass (no regressions) | Re-run M1-M3 tests |
| G4.14 | Git commit at M4 boundary | `git log` |

**S.U.P.E.R Check**: All platform collectors minimum B (13/20). `monitor/gpu/` (13/B due to P/E dependencies on external tools). `monitor/os.rs` (15/B). `monitor/virtualization.rs` (16/B). Average >= 16 (A-).

---

### M5: Terminal, Ping, Gzip, DNS, Self-Update

| Attribute | Detail |
|-----------|--------|
| **Objective** | Full PTY/ConPTY terminal for remote shell. ICMP/TCP/HTTP ping with 3-tier fallback. Fixed-Huffman gzip compression. Custom DNS resolver with TTL cache. Cloudflare Access header support. Self-update via GitHub releases. Full integration test with dummy Komari server (connect -> heartbeat -> task exec -> disconnect -> reconnect). |
| **Depends on** | M3 (Protocol FSM) AND M4 (Cross-Platform Metrics) |
| **Estimated days** | 6-8 |
| **Deliverables** | `src/gzip.rs`, `src/dns.rs` (full), `src/terminal/{mod,unix,windows}.rs`, `src/server/{ping_icmp,ping_tcp,ping_http}.rs`, `src/server/cf_access.rs`, `src/task.rs`, `src/server/task.rs` (expand real impls), `src/update.rs`, `tests/integration.rs` |
| **Code budget** | ~1,045 lines (cumulative 4,430) |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G5.1 | Terminal: `exec ls -la` returns correct directory listing via PTY on Linux | Unit test with fork/exec |
| G5.2 | Terminal: `dir` returns correct output via ConPTY on Windows 10 1809+ | Unit test with ConPTY |
| G5.3 | Terminal graceful shutdown: closing WS sends SIGHUP (Unix) / TerminateProcess (Windows); `waitpid` confirms exit | Unit test |
| G5.4 | ICMP ping to 8.8.8.8 returns RTT < 500ms (with CAP_NET_RAW on Linux) | Manual integration test |
| G5.5 | TCP ping to `google.com:443` returns RTT | Manual integration test |
| G5.6 | HTTP ping to `https://google.com` returns HTTP 200 | Manual integration test |
| G5.7 | Gzip: encoding 2 KB JSON produces valid gzip; `gunzip` decompresses to byte-identical original | Unit test with `gunzip` |
| G5.8 | Gzip: CRC32 test vectors pass: `""` -> 0x0, `"123456789"` -> 0xCBF43926 | Unit test |
| G5.9 | DNS: resolve `dns.google` with each configured DNS server; IPv4 preference works; IPv6 flag routes to AAAA | Unit test with UDP mock |
| G5.10 | DNS cache: TTL honored; cache hit avoids UDP send; max 50-entry LRU cap | Unit test |
| G5.11 | Self-update: detects newer GitHub release, downloads correct platform asset, verifies SHA256, replaces binary, exits 42 | Integration test against test GitHub repo |
| G5.12 | CF Access: when `cf_access_client_id` + `cf_access_client_secret` configured, WS upgrade and HTTP POST include `CF-Access-Client-Id` + `CF-Access-Client-Secret` headers | Unit test |
| G5.13 | Full integration test: dummy server lifecycle (connect -> heartbeat -> task exec -> disconnect -> reconnect) passes | `cargo test integration::full_lifecycle` |
| G5.14 | Binary size: < 1.2 MB stripped linux-amd64 | `ls -lh` |
| G5.15 | RSS: remains < 3 MB (terminal session peak < 3.2 MB) | `VmRSS` measurement |
| G5.16 | `cargo fmt --check` + `cargo clippy` clean on all platforms | CI |
| G5.17 | All M1-M4 gate criteria still pass (no regressions) | Re-run all tests |
| G5.18 | Git commit at M5 boundary | `git log` |

**S.U.P.E.R Check**: `gzip.rs` (18/A), `dns.rs` (15/B), `terminal/` (15-20/A-B), `ping_*` (14-20/A-B), `cf_access.rs` (20/A), `update.rs` (16/B). Average >= 16 (A-).

---

### M6: Polish + Packaging

| Attribute | Detail |
|-----------|--------|
| **Objective** | Auto-discovery. Persistent network traffic history. Windows toast notifications. Install scripts for all platforms. Full documentation. Final binary < 1 MB linux-amd64 stripped. Tag `v0.1.0`. |
| **Depends on** | M5 (Terminal + Ping + Tools) |
| **Estimated days** | 2-3 |
| **Deliverables** | `src/monitor/netstatic.rs`, `src/autodiscovery.rs`, `src/platform/windows_toast.rs`, `scripts/install.sh`, `scripts/install.ps1`, `README.md` (expand), `docs/protocol.md`, `docs/building.md` |
| **Code budget** | ~510 lines (cumulative 4,940) |

**Gate Criteria (all must be YES)**:

| # | Criterion | Check Method |
|---|-----------|-------------|
| G6.1 | Auto-discovery: fresh agent POSTs to `/api/agent/discover`, receives agent ID, persists to `auto-discovery.json`; restart picks up existing config and skips discovery | Integration test against real server |
| G6.2 | Network stats: `net_static.json` persists across agent restarts; `VecDeque` max 720 entries (12h at 1/min); oldest ejected when full; month rotation works | Unit test + 5-min run |
| G6.3 | Network stats: atomic write via `.tmp` + `rename` -- no corruption on crash | Unit test (simulated crash) |
| G6.4 | Windows toast: notification appears on Windows 10/11; falls back to `MessageBoxW` if toast API unavailable | Manual test on Windows |
| G6.5 | Install script `install.sh`: completes on Ubuntu 22.04 and macOS 15; binary installed to `/usr/local/bin/komari-agent`; systemd/launchd service created; agent starts and heartbeats | Manual test on fresh VM |
| G6.6 | Install script `install.ps1`: completes on Windows 11; binary installed to `$env:ProgramFiles\komari-agent`; service created; agent starts and heartbeats | Manual test on fresh Windows |
| G6.7 | README.md covers all sections: overview, features, platform support, installation, configuration (all 34 flags), building from source, architecture, troubleshooting | Manual review |
| G6.8 | `docs/protocol.md` documents all v2 JSON-RPC methods, v1 format, heartbeat/task/ping schemas | Manual review |
| G6.9 | `docs/building.md` documents prerequisites, `cargo build --release`, cross-compilation, CI | Manual review |
| G6.10 | All M1-M5 gate criteria still pass (no regressions) | Full test suite |
| G6.11 | Binary size: **< 1.0 MB** stripped linux-amd64, < 1.2 MB windows-amd64, < 1.2 MB macos-amd64, < 1.2 MB freebsd-amd64 | `ls -lh` per platform |
| G6.12 | `--no-default-features` (core only) binary < 700 KB on all platforms | `ls -lh` with `--no-default-features` |
| G6.13 | RSS < 3 MB after 60s steady state (re-verified) | `VmRSS` measurement |
| G6.14 | `cargo fmt --check` + `cargo clippy` clean on all platforms | CI |
| G6.15 | Git tag `v0.1.0` created, pushed to `DeliciousBuding/komari-agent-rs` | `git tag -l` |
| G6.16 | Git commit at M6 boundary | `git log` |

**S.U.P.E.R Check**: `netstatic.rs` (18/A), `autodiscovery.rs` (18/A), `windows_toast.rs` (16/B). Average >= 17 (A).

---

## 2. ACCEPTANCE CRITERIA PER MODULE

Each criterion is phrased as a yes/no question. "YES" = criterion met. "NO" = blocker. Module ordering follows the dependency graph.

### Phase 1 Modules

#### M1: `Cargo.toml` + `.cargo/config.toml` + CI skeleton

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.1 | Does `cargo metadata` resolve without errors? | |
| AC-1.2 | Is `[profile.release]` set to `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`? | |
| AC-1.3 | Does `cargo build --release` on empty `main.rs` produce < 400 KB stripped? | |
| AC-1.4 | Does `.github/workflows/ci.yml` have a 4-OS matrix (ubuntu, windows, macos, freebsd) all green? | |
| AC-1.5 | Does `Cargo.toml` list only approved deps with no wildcard versions? | |

#### M1: `src/config.rs` -- CLI + env config

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.6 | Do all 34 config fields parse from CLI args correctly? | |
| AC-1.7 | Do all 34 env var overrides work? (`AGENT_TOKEN`, `AGENT_ENDPOINT`, etc.) | |
| AC-1.8 | Does `--config-file` load and merge with CLI/env correctly? | |
| AC-1.9 | Does `mem_mode()` return 0 by default, 1 when `memory_include_cache=true`, 2 when `memory_report_raw_used=true`? | |
| AC-1.10 | Is hand-written CLI parsing < 150 lines (per spec DD1)? | |
| AC-1.11 | Does config compile and pass tests on all 4 platforms? | |
| AC-1.12 | Do all field names match Go agent's 34-field Config struct exactly? | |

#### M1: `src/crypto.rs` -- SHA-1 + base64

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.13 | Does SHA-1 produce `a9993e36...` for `"abc"`? | |
| AC-1.14 | Does SHA-1 produce `da39a3ee...` for `""`? | |
| AC-1.15 | Does SHA-1 produce `84983e44...` for the 56-byte test vector? | |
| AC-1.16 | Does WebSocket accept key computation produce `"s3pPLMBiTxaQ9kYGzzhZRbK+xOo="`? | |
| AC-1.17 | Do all 7 base64 RFC 4648 vectors pass? | |
| AC-1.18 | Does `base64_encode(sha1(input))` round-trip deterministically for 256 random inputs? | |
| AC-1.19 | Is SHA-1 < 100 lines? Base64 < 60 lines? | |
| AC-1.20 | Are there zero external crate deps (no `sha1`, `base64`, `digest`)? | |
| AC-1.21 | Is everything stack-allocated (no `Box`, no `Vec`)? | |
| AC-1.22 | Does SHA-1 output match Go `gorilla/websocket`'s `Sec-WebSocket-Accept` computation bit-for-bit? | |

#### M1: `src/json.rs` -- JsonBuf + Field + EncodeJson

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.23 | Does `push_u64(0)` -> `"0"`, `push_u64(42)` -> `"42"`, `push_u64(18446744073709551615)` -> correct string? | |
| AC-1.24 | Does `push_i64(-1)` -> `"-1"`, `push_i64(-9223372036854775808)` -> correct? | |
| AC-1.25 | Does `push_f64_prec2(0.0)` -> `"0.00"`, `push_f64_prec2(12.5)` -> `"12.50"`? | |
| AC-1.26 | Does `push_f64_prec2(f64::NAN)` -> `"null"`? | |
| AC-1.27 | Is JSON string escaping correct for `"`, `\`, `\n`, `\r`, `\t`, control chars? | |
| AC-1.28 | Does `arena.reset()` clear buffer; does `JsonBuf::new()` write to beginning? | |
| AC-1.29 | Is `JsonBuf::new()` zero heap allocation? | |
| AC-1.30 | Does encoding a 2 KB monitoring report take < 50 microseconds? | |
| AC-1.31 | Is JSON output byte-identical to Go agent for the same metric values? | |

#### M1: `src/tls.rs` -- TLS configuration

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.32 | Does `build_client_config(insecure=false)` use OS native root certificates (no webpki-roots)? | |
| AC-1.33 | Does insecure mode (`ignore_unsafe_cert=true`) skip certificate verification? | |
| AC-1.34 | Does TLS handshake succeed against `https://httpbin.org`? | |
| AC-1.35 | Does TLS handshake fail against `https://expired.badssl.com` in secure mode? | |

#### M1: `src/ws.rs` -- WebSocket connect + handshake

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.36 | Does WS upgrade request have: GET method, correct path `/api/clients/v2/rpc?token=<token>`, `Upgrade: websocket`, `Connection: Upgrade`, `Sec-WebSocket-Version: 13`? | |
| AC-1.37 | Is `Sec-WebSocket-Key` 16 random bytes, base64 encoded, 24 chars long? | |
| AC-1.38 | Does `verify_handshake_response()` accept valid 101 + correct accept key? | |
| AC-1.39 | Does it reject wrong accept key? Non-101 status? | |
| AC-1.40 | Is WebSocket frame codec hand-implemented (not tungstenite)? | |
| AC-1.41 | Are WS upgrade headers byte-identical to Go `gorilla/websocket` (same order, same casing)? | |

#### M1: `src/protocol/v2.rs` + `v1.rs` -- JSON-RPC types

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.42 | Does `build_notification("agent.report", ...)` produce correct v2 notification envelope? | |
| AC-1.43 | Does `build_request("id-1", "agent.pull", ...)` produce correct v2 request with id? | |
| AC-1.44 | Does `build_report_payload(...)` wrap in `{"report":...}` then v2 envelope? | |
| AC-1.45 | Does V1 payload format match Go agent flat JSON byte-for-byte? | |
| AC-1.46 | Are all 10 method constants present with correct string values? | |

#### M1: `src/server/mod.rs` -- initial run() with static heartbeat

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.47 | Does `run()` connect to real Komari server, complete WS handshake? | |
| AC-1.48 | Does server receive valid heartbeat JSON? | |
| AC-1.49 | Is first connect (TCP + TLS + WS upgrade) < 5 seconds? | |
| AC-1.50 | Is the agent single-threaded (no `spawn`, no `tokio`)? | |

#### M1: `src/app.rs` + `src/main.rs` -- entry point

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-1.51 | Does `main()` return `i32` exit code (not `process::exit()` except for self-update exit 42)? | |
| AC-1.52 | Is `main.rs` < 15 lines? `app.rs` < 80 lines? | |
| AC-1.53 | Are CLI flag names identical to Go agent (long form only, same env var names)? | |

---

### Phase 2 Modules

#### M2: `src/arena.rs` -- ScratchArena + SmallVec

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.1 | Does `arena.alloc(1)` return Some? `arena.alloc(8192)` return Some? `arena.alloc(8193)` return None? | |
| AC-2.2 | Does `arena.reset()` rewind cursor to 0 and allow new allocations that overwrite old data? | |
| AC-2.3 | Does `SmallVec::push(0..N)` keep elements inline; `push(N+1)` spill to heap Vec? | |
| AC-2.4 | Is `arena.alloc()` < 5 CPU instructions? `arena.reset()` single assignment? | |
| AC-2.5 | Does `// SAFETY:` comment exist on every `UnsafeCell` access? | |

#### M2: `src/monitor/cpu/linux.rs` -- CPU collection

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.6 | Does CPU usage computation produce correct delta from previous sample? | |
| AC-2.7 | Does it handle `/proc/stat` with guest/guest_nice fields? | |
| AC-2.8 | Does first tick return `usage = 0.0` (no previous sample)? | |
| AC-2.9 | Does CPU model name extraction handle special chars (e.g., `"Intel(R) Core(TM) i7-13700K"`)? | |
| AC-2.10 | Do CPU values match Go agent on same host within 0.5 percentage points? | |

#### M2: `src/monitor/mem/linux.rs` -- Memory collection (3-mode)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.11 | Does mode 0: `used = total - MemAvailable` match Go agent on same host? | |
| AC-2.12 | Does mode 1: `used = total - MemFree - Buffers - Cached` match Go agent? | |
| AC-2.13 | Does mode 2: `used = total - MemFree` match Go agent? | |
| AC-2.14 | Does MemAvailable missing (kernel < 3.14) fall back to `MemFree + Buffers + Cached`? | |
| AC-2.15 | Does swap disabled (SwapTotal=0) produce `total=0, used=0`? | |
| AC-2.16 | Does `free -b` fallback work when /proc/meminfo parse fails? | |

#### M2: `src/monitor/disk/linux.rs` -- Disk collection

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.17 | Does `is_physical_disk()` exclude all 30+ virtual filesystem types? | |
| AC-2.18 | Does mount list match Go agent on same host exactly (same devices, same total/used)? | |
| AC-2.19 | Is the exclude list a `const &[&str]` slice (no dynamic allocation)? | |
| AC-2.20 | Does it handle zero physical disks (all virtual FS)? | |

#### M2: `src/monitor/net/linux.rs` -- Network collection

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.21 | Does first tick return `speed = 0.0` (no previous sample)? | |
| AC-2.22 | Do subsequent ticks compute correct speed delta = `(rx - prev_rx) / interval`? | |
| AC-2.23 | Does NIC include/exclude filter support wildcard matching? | |
| AC-2.24 | Are loopback and virtual interfaces (docker, tun, veth) excluded by default? | |
| AC-2.25 | Do network values match Go agent on same host within 10%? | |

#### M2: `src/monitor/load/linux.rs` -- Load average

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.26 | Does parsing `"0.25 0.32 0.28 1/456 12345\n"` yield `load1=0.25, load5=0.32, load15=0.28`? | |
| AC-2.27 | Do values match Go agent on same host to 2 decimal places? | |

#### M2: `src/monitor/connections/linux.rs` -- Connection count

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.28 | Does TCP count match Go agent on same host? | |
| AC-2.29 | Does it handle empty `/proc/net/tcp` (header only)? | |

#### M2: `src/monitor/process/linux.rs` -- Process count

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.30 | Does PID counting exclude non-numeric /proc entries (self, thread-self, fs)? | |
| AC-2.31 | Does running process count (`State=R` in `/proc/<pid>/status`) match Go agent? | |

#### M2: `src/monitor/uptime/linux.rs` -- Uptime

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.32 | Does uptime value match Go agent on same host within 1s? | |
| AC-2.33 | Does it handle very large uptime values (years)? | |

#### M2: `src/monitor/mod.rs` -- Monitor struct + tick()

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-2.34 | Does `Monitor::new()` initialize with zero previous state? | |
| AC-2.35 | Does second `tick()` call produce non-zero cpu_usage and net speed (has previous sample)? | |
| AC-2.36 | Does `tick()` complete with zero heap allocations (verified via `dhat` or custom allocator)? | |
| AC-2.37 | Does a single tick complete in < 10 ms on idle 4-core system? | |
| AC-2.38 | Is arena cursor < 4096 after each tick (well within 8192 capacity)? | |
| AC-2.39 | Is JSON output from `tick()` valid JSON with correct field names? | |

---

### Phase 3 Modules

#### M3: `src/protocol/fsm.rs` -- FallbackFsm + ConnectionFsm

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.1 | Do all 12 FSM transitions pass unit tests? | |
| AC-3.2 | Does `record_failure(true)` increment counter; 3 consecutive -> `V1HttpPost`? | |
| AC-3.3 | Does `record_success()` reset counter to 0 and return to `V2WebSocket`? | |
| AC-3.4 | Does `record_failure(false)` (network error) NOT increment counter? | |
| AC-3.5 | Does `new(0)` or `new(1)` start in `V1HttpPost`? | |
| AC-3.6 | Are FSM transitions zero-allocation (pure match arms on Copy enums)? | |
| AC-3.7 | Is file size < 120 lines? | |

#### M3: `src/server/backoff.rs` -- Backoff with jitter

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.8 | Does `next_delay()` sequence approximate: 1s, 2s, 4s, 8s, ... capping at 300s? | |
| AC-3.9 | Are all delays within +/- 25% of base for 100 calls with deterministic seed? | |
| AC-3.10 | Does `reset()` restore initial delay and zero attempts? | |
| AC-3.11 | Does matching Go agent's jitter logic produce the same statistical distribution? | |
| AC-3.12 | Is the PRNG self-contained (no external rand crate)? | |

#### M3: `src/server/reconnection.rs` -- Reconnection loop

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.13 | Does data tick fire at configured interval (default 1s) within 50ms precision? | |
| AC-3.14 | Does heartbeat ping fire at 30s interval? | |
| AC-3.15 | On WS error, does backoff trigger and reconnect happen after delay? | |
| AC-3.16 | After `max_retries` attempts, does agent log fatal and exit? | |
| AC-3.17 | Does reconnect survive server restart (kill server, agent detects, reconnects when server returns)? | |
| AC-3.18 | On Windows, does the `Poller` abstraction use `select()` instead of `poll()`? | |

#### M3: `src/http.rs` (expand) -- Full HTTP POST fallback

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.19 | Does `post_json()` include headers: `Host`, `Content-Type: application/json`, optional `Content-Encoding: gzip`, optional `CF-Access-*`? | |
| AC-3.20 | Does HTTP POST to real Komari server `/api/clients/v2/rpc` succeed? | |
| AC-3.21 | Is POST body byte-identical to WS text frame body for the same monitoring data? | |
| AC-3.22 | Is the implementation < 70 lines (manual HTTP/1.1 per DD4)? | |

#### M3: `src/server/task.rs` (stubs)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.23 | Does `handle_exec("ls -la")` return well-formed stub with task_id? | |
| AC-3.24 | Does `handle_ping("icmp", "8.8.8.8")` return well-formed stub with ping_type and task_id? | |
| AC-3.25 | Do stub responses parse correctly by Komari server? | |

#### M3: `src/server/mod.rs` (FSM integration)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-3.26 | Does recorded Go agent session replay produce byte-for-byte matching JSON output? | |
| AC-3.27 | Does full lifecycle unit test pass: connect v2 WS -> heartbeat -> inject 3 errors -> v1 fallback -> success -> return to v2 WS? | |
| AC-3.28 | Do M1 heartbeat and M2 monitor correctness still pass (regression)? | |

---

### Phase 4 Modules

#### M4: Windows collectors (cpu, mem, disk, net, load, connections, process, uptime)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.1 | Does Windows CPU collector match Go agent's CPU name and core count on same host? | |
| AC-4.2 | Does Windows memory 3-mode match Go agent on same host? | |
| AC-4.3 | Does Windows disk collector match Go agent's mount/drive list? | |
| AC-4.4 | Does Windows network delta calculation match Go agent? | |
| AC-4.5 | Does Windows process count match Go agent? | |
| AC-4.6 | Does each `unsafe` Win32 FFI call have a `// SAFETY:` comment? | |

#### M4: macOS collectors

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.7 | Does macOS CPU collector report correct model name and core count? | |
| AC-4.8 | Does macOS memory collector report valid total/used values? | |
| AC-4.9 | Does macOS disk collector exclude devfs, autofs? | |
| AC-4.10 | Does macOS uptime via `sysctl kern.boottime` produce correct value? | |
| AC-4.11 | Are all sysctl/Mach API calls behind `unsafe` with `// SAFETY:` comments? | |

#### M4: FreeBSD collectors

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.12 | Do FreeBSD collectors compile and produce valid (non-panicking) output in CI? | |
| AC-4.13 | Does freebsd memory use `kvm_getswapinfo` correctly? | |
| AC-4.14 | Does cross-compilation CI (or Cirrus CI) confirm build correctness? | |

#### M4: `src/monitor/gpu/` -- GPU detection (all 4 platforms)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.15 | Does NVIDIA detection (nvidia-smi CSV) produce same GPU name and VRAM as Go agent? | |
| AC-4.16 | Does AMD detection (rocm-smi key scanner) produce same GPU name? | |
| AC-4.17 | Does DRM fallback (`/sys/class/drm`) detect GPU presence when nvidia-smi missing? | |
| AC-4.18 | Does Windows DXGI produce same GPU name as Go agent? | |
| AC-4.19 | Does macOS `system_profiler SPDisplaysDataType` produce correct GPU info? | |
| AC-4.20 | Does FreeBSD `pciconf -lv` detect GPU? | |
| AC-4.21 | Does empty GPU report (`count=0`) not cause panic or protocol error? | |

#### M4: `src/monitor/os.rs` -- OS name + kernel version

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.22 | Does it correctly identify: Ubuntu 22.04, Debian 12, Windows 11, macOS 15, FreeBSD 14? | |
| AC-4.23 | Does it handle Android (build.prop), Synology (/etc.defaults/VERSION), PVE (pveversion), fnOS? | |
| AC-4.24 | Does unknown Linux fall back to `"Linux"` with raw os-release dump in message field? | |

#### M4: `src/monitor/virtualization.rs` -- VM/container detection

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.25 | Does it detect: Docker, Kubernetes (kubepods), LXC containers? | |
| AC-4.26 | Does it detect: KVM, VMware, VirtualBox, Hyper-V VMs? | |
| AC-4.27 | Does it detect container-inside-VM (both indicators present)? | |
| AC-4.28 | Does bare metal produce empty virtualization string? | |
| AC-4.29 | Is CPUID detection behind `#[cfg(target_arch = "x86_64")]`? | |

#### M4: CI expansion

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-4.30 | Is CI matrix 4 OS x (build + test + clippy + fmt) all green? | |
| AC-4.31 | Is CI runtime < 15 minutes per platform? | |
| AC-4.32 | Does FreeBSD CI use cross-compilation or Cirrus CI runner? | |

---

### Phase 5 Modules

#### M5: `src/gzip.rs` -- Fixed-Huffman DEFLATE encoder

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.1 | Does CRC32 pass: `""` -> 0x0, `"123456789"` -> 0xCBF43926? | |
| AC-5.2 | Does gzip round-trip: encode JSON -> gunzip -> byte-identical original? | |
| AC-5.3 | Does fixed Huffman table match RFC 1951 Section 3.2.6 exactly? | |
| AC-5.4 | Does encoding 2 KB JSON produce valid gzip accepted by `gunzip`? | |
| AC-5.5 | Does Komari server accept gzip-compressed POST body? | |
| AC-5.6 | Is encoding of 2 KB JSON < 500 microseconds? | |
| AC-5.7 | Is total module < 200 lines (per DD9)? | |

#### M5: `src/dns.rs` -- Custom DNS resolver

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.8 | Does A query build produce correct QNAME encoding, QTYPE=1, QCLASS=1? | |
| AC-5.9 | Does AAAA query produce QTYPE=28? | |
| AC-5.10 | Does response parse handle A, AAAA, CNAME chain, NXDOMAIN, truncation? | |
| AC-5.11 | Does cache hit avoid UDP send; TTL held; max 50-entry LRU cap? | |
| AC-5.12 | Does each of 10 built-in DNS servers resolve? | |
| AC-5.13 | Does `prefer_ipv4=true` return A records first; `prefer_ipv6=true` return AAAA first? | |
| AC-5.14 | Is DNS query built from raw bytes (no external DNS library)? | |
| AC-5.15 | Does resolution produce same IP as Go agent for same hostname + DNS server? | |

#### M5: `src/terminal/` -- PTY (Unix) + ConPTY (Windows)

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.16 | Does Unix PTY: open -> grantpt/unlockpt -> fork/exec "echo hello" -> read "hello\n"? | |
| AC-5.17 | Does Unix PTY resize via `TIOCSWINSZ` ioctl work? | |
| AC-5.18 | Does close master -> SIGHUP child -> `waitpid` confirm exit? | |
| AC-5.19 | Does Windows ConPTY: `CreatePseudoConsole` + `CreateProcess` with `STARTUPINFOEX` -> write "dir\r\n" -> read output? | |
| AC-5.20 | Does `ResizePseudoConsole` work on Windows? | |
| AC-5.21 | Does ConPTY fail gracefully on Windows < 10 1809 with clear error message? | |
| AC-5.22 | Does terminal WebSocket sub-protocol match Go agent? | |
| AC-5.23 | Does graceful shutdown (WS close -> process exit) send SIGHUP/TerminateProcess? | |
| AC-5.24 | Does each `unsafe` FFI block in terminal code have `// SAFETY:` comment? | |

#### M5: `src/server/ping_icmp.rs` + `ping_tcp.rs` + `ping_http.rs` -- Ping

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.25 | Does ICMP echo request have correct type=8, code=0, checksum? | |
| AC-5.26 | Does ICMP RTT measurement match expected latency? | |
| AC-5.27 | Does Linux ICMP gracefully return permission error when no CAP_NET_RAW? | |
| AC-5.28 | Does TCP ping to open port measure RTT; closed port return "connection refused"? | |
| AC-5.29 | Does HTTP ping return correct status code; timeout return error? | |
| AC-5.30 | Does `handle_ping("icmp", target)` dispatch correctly across all 3 ping types? | |
| AC-5.31 | Does `agent.pingResult` JSON match Go agent format exactly? | |

#### M5: `src/server/cf_access.rs` -- Cloudflare Access headers

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.32 | When config fields set, do WS upgrade and HTTP POST include `CF-Access-Client-Id` + `CF-Access-Client-Secret`? | |
| AC-5.33 | When config fields empty, are headers absent? | |
| AC-5.34 | Does agent connect through CF Access-protected Komari server successfully? | |

#### M5: `src/server/task.rs` (expand) -- Real exec + ping

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.35 | Does `handle_exec("echo hello")` output "hello\n" with exit_code=0? | |
| AC-5.36 | Does exec with non-existent command return exit_code=-1? | |
| AC-5.37 | Does exec with timeout kill the process and return exit_code=-1? | |
| AC-5.38 | Does Windows exec use `powershell -Command`; Unix use `sh -s`? | |
| AC-5.39 | Does `agent.taskResult` JSON format match Go agent exactly? | |

#### M5: `src/update.rs` -- Self-update

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.40 | Does version comparison: current < latest -> update needed; current >= latest -> up to date? | |
| AC-5.41 | Does it download correct platform asset from GitHub releases? | |
| AC-5.42 | Does SHA256 verification reject invalid checksum? | |
| AC-5.43 | Does Unix binary replace use `chmod +x` + `rename` (atomic)? | |
| AC-5.44 | Does Windows binary replace use `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`? | |
| AC-5.45 | Does agent exit with code 42 after successful update (matching Go agent)? | |

#### M5: `tests/integration.rs` -- Full integration test

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-5.46 | Does dummy server complete WebSocket handshake correctly? | |
| AC-5.47 | Does agent send heartbeat and dummy server receive it? | |
| AC-5.48 | Does dummy server send `agent.exec` event and agent respond with result? | |
| AC-5.49 | Does dummy server close connection and agent reconnect? | |
| AC-5.50 | Does full lifecycle test pass on all 4 platforms in CI? | |
| AC-5.51 | Does integration test run in < 30 seconds? | |

---

### Phase 6 Modules

#### M6: `src/monitor/netstatic.rs` -- Persistent traffic history

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-6.1 | Does `TrafficData` serialize/deserialize to/from JSON correctly? | |
| AC-6.2 | Does `VecDeque` cap at 720 entries, ejecting oldest on overflow? | |
| AC-6.3 | Does atomic file write (`.tmp` + `rename`) prevent corruption on crash? | |
| AC-6.4 | Does restart load existing `net_static.json` correctly? | |
| AC-6.5 | Does month rotation config zero cumulative counters? | |
| AC-6.6 | Does file format match Go agent's `net_static.json` exactly? | |

#### M6: `src/autodiscovery.rs` -- Auto-registration

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-6.7 | Does `POST /api/agent/discover` include hostname, OS, IP, agent_version? | |
| AC-6.8 | Does HTTP 200 with agent ID -> save to `auto-discovery.json`? | |
| AC-6.9 | Does HTTP 4xx/5xx -> retry after 60s? | |
| AC-6.10 | Does restart skip discovery if `auto-discovery.json` exists? | |

#### M6: Install scripts + Documentation

| ID | Criterion | YES/NO |
|----|-----------|:------:|
| AC-6.11 | Does `install.sh` complete on Ubuntu 22.04 and macOS 15? | |
| AC-6.12 | Does `install.ps1` complete on Windows 11? | |
| AC-6.13 | Does agent start and heartbeat after scripted install? | |
| AC-6.14 | Does README.md cover all required sections? | |
| AC-6.15 | Do `docs/protocol.md` and `docs/building.md` exist and are accurate? | |

---

## 3. BINARY SIZE TRACKING

### 3.1 Expected Size at Each Milestone

| Milestone | linux-amd64 stripped | windows-amd64 stripped | macos-amd64 stripped | freebsd-amd64 stripped |
|-----------|:---:|:---:|:---:|:---:|
| M1 (Foundation, no features) | < 2.0 MB | < 2.5 MB | < 2.5 MB | < 2.5 MB |
| M2 (Linux Metrics) | < 1.5 MB | < 2.0 MB | < 2.0 MB | < 2.0 MB |
| M3 (Protocol FSM) | < 1.3 MB | < 1.8 MB | < 1.8 MB | < 1.8 MB |
| M4 (Cross-Platform) | < 1.5 MB | < 2.0 MB | < 2.0 MB | < 2.0 MB |
| M5 (Terminal+Ping+Gzip+DNS) | < 1.2 MB | < 1.5 MB | < 1.5 MB | < 1.5 MB |
| **M6 (Final, `--features full`)** | **< 1.0 MB** | **< 1.2 MB** | **< 1.2 MB** | **< 1.2 MB** |
| M6 (`--no-default-features`) | < 700 KB | < 700 KB | < 700 KB | < 700 KB |

### 3.2 How to Measure

**Method 1 -- `ls -lh` (primary, always available)**:
```bash
# Build
cargo build --release

# Strip (on Unix)
strip target/release/komari-agent

# Measure
ls -lh target/release/komari-agent
# Output example: -rwxr-xr-x 1 user user 876K Jun 20 12:00 komari-agent
```

**Method 2 -- `cargo-bloat` (diagnostic, for investigating what takes space)**:
```bash
cargo install cargo-bloat
cargo bloat --release --crates          # Per-crate breakdown
cargo bloat --release -n 20             # Top 20 largest symbols
cargo bloat --release --filter komari   # Our code only
```

**Method 3 -- CI automated check**:
```yaml
- name: Binary size check
  run: |
    SIZE=$(stat -c%s target/release/komari-agent)
    if [ $SIZE -gt 1048576 ]; then
      echo "Binary too large: $SIZE bytes (max 1 MB)"
      exit 1
    fi
```

### 3.3 Regression Alert Threshold

| Threshold | Action |
|-----------|--------|
| +5% within a milestone | Warning: log the growth, identify source via `cargo bloat` |
| +10% within a milestone | Blocker: must be resolved before milestone gate passes |
| +25 KB or more from any single commit | Immediate investigation: `cargo bloat --release -n 50` to find culprit |
| Crosses 1.0 MB on linux-amd64 stripped | **Hard blocker** -- binary fails spec constraint |

### 3.4 Binary Budget Allocation (linux-amd64, stripped, `--features full`)

| Component | Expected | Max Allowed |
|-----------|:-------:|:-----------:|
| Application code (all rust modules) | ~144 KB | 200 KB |
| rustls (TLS library) | ~250 KB | 300 KB |
| webpki-roots / OS cert store | ~100 KB | 120 KB (exempt from budget per spec) |
| tungstenite (WebSocket) | ~30 KB | 50 KB |
| clap (CLI parser) | ~25 KB | 35 KB |
| log + env_logger | ~10 KB | 20 KB |
| windows-rs (Windows only) | ~40 KB | 60 KB |
| Rust std + alloc + core + panic | ~60 KB | 80 KB |
| Miscellaneous / padding | ~40 KB | 50 KB |
| **Total** | **~659 KB** | **< 1.0 MB** |

### 3.5 Feature Gate Size Impact

| Feature | Binary increment | Cumulative (`full`) |
|---------|:---------------:|:-------------------:|
| (none, core only) | ~600 KB | 600 KB |
| `gpu-detection` | +80 KB | 680 KB |
| `terminal` | +60 KB | 740 KB |
| `ping` | +30 KB | 770 KB |
| `self-update` | +15 KB | 785 KB |
| `full` (all above) | **~785 KB** | **~785 KB** |

---

## 4. RSS TRACKING

### 4.1 Expected RSS at Steady State

| State | RSS | Notes |
|-------|:---:|-------|
| Startup (first connect) | ~2.5 MB | DNS resolution, TLS handshake |
| **Steady-state (monitoring, 60s+)** | **< 2.97 MB** | Target < 3 MB |
| Self-update active | ~8 MB | Transient, freed after update |
| Terminal session active | ~3.1 MB | PTY I/O buffers, temporary |
| Graceful shutdown | ~1 MB | OS reclaims memory |

### 4.2 RSS Component Breakdown (Steady State)

| Component | Size |
|-----------|:---:|
| Rust runtime (allocator, panic handler, TLS) | ~500 KB |
| Main thread stack | ~2 MB |
| ScratchArena (8 KB stack) | 8 KB |
| Monitor state (prev_net, counters) | ~200 B |
| Config struct | ~400 B |
| WebSocket buffers (tungstenite) | ~65 KB |
| rustls TLS session | ~200 KB |
| DNS cache | ~10 KB |
| Netstatic history (VecDeque, heap) | ~50 KB |
| JSON payload buffer | ~8 KB |
| Gzip encoder scratch (LZ77 tables) | ~80 KB |
| Log buffer | ~32 KB |
| Backoff + FSM state | ~200 B |
| **Total** | **~2.97 MB** |

### 4.3 Measurement Procedure

**Environment**: Container (Docker) with memory limit > 256 MB, or bare metal.

**Step-by-step**:

```bash
# 1. Build release binary
cargo build --release
strip target/release/komari-agent

# 2. Start agent
./target/release/komari-agent \
    --token=<test_token> \
    --endpoint=<test_endpoint> \
    --interval=1 &
AGENT_PID=$!

# 3. Wait 60 seconds for steady state
sleep 60

# 4. Sample RSS every 10 seconds for 2 minutes (12 samples)
for i in $(seq 1 12); do
    # Linux
    RSS_KB=$(cat /proc/$AGENT_PID/status | grep VmRSS | awk '{print $2}')
    echo "$(date +%H:%M:%S) RSS: ${RSS_KB} KB"
    sleep 10
done

# 5. Kill agent
kill $AGENT_PID

# 6. Report: max RSS across all samples
```

**macOS**:
```bash
RSS_KB=$(ps -o rss= -p $AGENT_PID)
echo "RSS: ${RSS_KB} KB"
```

**Windows (PowerShell)**:
```powershell
$p = Get-Process -Id $AGENT_PID
$RSS_BYTES = $p.WorkingSet64
Write-Host "RSS: $([math]::Round($RSS_BYTES / 1024)) KB"
```

### 4.4 RSS Acceptance Criteria

| Criterion | Threshold | Check |
|-----------|:---------:|-------|
| Steady-state RSS (60s+) | < 3,072 KB | Max of 12 samples over 2 minutes |
| Peak RSS (startup) | < 4,096 KB | Single highest sample |
| RSS trend | Flat or decreasing after 60s | Linear regression slope <= 0 |
| Terminal session peak | < 3,276 KB (3.2 MB) | During active PTY session |
| Self-update peak | < 10 MB | Transient, must free after update |

### 4.5 Regression Alert Threshold

| Threshold | Action |
|-----------|--------|
| RSS > 2.8 MB in CI | Warning: log for human review |
| RSS > 3.0 MB | Blocker for M2 gate; investigate with `dhat` or custom allocator |
| RSS > 3.5 MB | Hard blocker for any milestone; memory leak investigation required |
| RSS growth > 10 KB per subsequent milestone | Investigate cumulative leak |

---

## 5. INTEGRATION TEST PLAN

### 5.1 Real Komari Server Verification

**Prerequisites**:
1. Running Komari server instance (e.g., `https://monitor.example.com`)
2. Valid agent token
3. Go agent binary (for comparison) AND Rust agent binary (under test)
4. Network access from test machine to server

**Step 1: Recorded Fixture Collection (Go Agent)**:
```bash
# Capture WebSocket traffic
sudo tcpdump -i any -w /tmp/go-agent-ws.pcap \
    'host monitor.example.com and port 443' &
TCPDUMP_PID=$!

# Run Go agent for 30 seconds
./komari-agent-go --token=<token> --endpoint=https://monitor.example.com &
GO_PID=$!
sleep 30

# Optionally send exec event from server UI

# Stop
kill $GO_PID; sleep 2; sudo kill $TCPDUMP_PID

# Extract WebSocket frames
tshark -r /tmp/go-agent-ws.pcap -Y 'websocket' \
    -T fields -e websocket.payload > /tmp/go-frames.jsonl
```

**Step 2: JSON Wire Compatibility Test**:
```bash
# For each tick frame, diff Rust vs Go output
for frame in $(seq 1 30); do
    diff <(go_frame_$frame.json | jq -S .) \
         <(rust_frame_$frame.json | jq -S .)
done
# All diffs must be empty or only contain expected timestamp differences
```

**Step 3: Byte-for-Byte Compatibility Checkpoints**:

| Checkpoint | Go output | Rust output | Tolerance |
|-----------|-----------|-------------|-----------|
| `cpu.usage` | 12.5 | 12.50 | Exactly match (2 decimal places) |
| `ram.total` | 17179869184 | 17179869184 | Exact |
| `ram.used` | varies | varies | Exact (same mem_mode) |
| `load.load1` | 0.25 | 0.25 | Exact (2 decimal places) |
| `network.up` | 125000.0 | 125000.0 | Exact (1 decimal place) |
| `connections.tcp` | 42 | 42 | Exact |
| `uptime` | 86400 | 86400 | Within 1s |
| `gpu.name` | "NVIDIA GeForce RTX 4090" | "NVIDIA GeForce RTX 4090" | Exact string match |
| `os_name` | "Ubuntu 22.04.3 LTS" | "Ubuntu 22.04.3 LTS" | Exact string match |

**Step 4: Protocol FSM Verification**:
```bash
# Test 1: Normal operation (v2 WS)
curl -s https://monitor.example.com/api/admin/agents | \
    jq '.[] | select(.version=="komari-agent-rs")'
# Verify: protocol version = 2

# Test 2: Force v2 WS failure -> v1 fallback (block WS port)
sudo iptables -A OUTPUT -p tcp --dport 443 -j DROP
sleep 30  # Agent should fall back
sudo iptables -D OUTPUT -p tcp --dport 443 -j DROP
# Verify server logs show v1 fallback

# Test 3: Recovery
# Agent must detect v2 WS available and return to v2
# Check server logs: protocol version should change from v1 to v2
```

**Step 5: GPU Detection Verification**:
```bash
# Linux: compare agents
nvidia-smi --query-gpu=name,memory.total,memory.used --format=csv,noheader
diff <(go_agent_gpu_info.json | jq -S .) <(rust_agent_gpu_info.json | jq -S .)
```

### 5.2 Mock Server Design for CI

The mock Komari server is a self-contained TCP server that speaks the Komari WebSocket protocol. It runs in the same test process as the agent.

**Design** (`tests/mock_server.rs` or inline in `tests/integration.rs`):

```rust
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

/// Mock Komari server for integration testing.
/// Speaks minimal WebSocket protocol: upgrades, accepts text frames,
/// sends ping/pong, sends task events, closes connection.
pub struct MockKomariServer {
    listener: TcpListener,
    port: u16,
}

impl MockKomariServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        Self { listener, port }
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/api/clients/v2/rpc?token=test-token", self.port)
    }

    /// Handle one agent connection lifecycle.
    /// Blocks until agent disconnects or test timeout.
    pub fn handle_one_connection(&self) {
        let (mut stream, _) = self.listener.accept().unwrap();

        // 1. Read HTTP upgrade request
        let mut buf = [0u8; 4096];
        let n = stream_read(&mut stream, &mut buf);

        // 2. Parse Sec-WebSocket-Key
        let request = std::str::from_utf8(&buf[..n]).unwrap();
        let key = extract_header(request, "Sec-WebSocket-Key").unwrap();

        // 3. Compute accept key
        let accept = compute_accept_key(key);

        // 4. Send 101 Switching Protocols
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            accept
        );
        stream_write(&mut stream, response.as_bytes());

        // 5. Receive agent heartbeat (text frame)
        let frame = read_ws_frame(&mut stream);
        assert!(frame.opcode == 1, "Expected text frame, got opcode {}", frame.opcode);
        let heartbeat: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(heartbeat["jsonrpc"], "2.0");
        assert_eq!(heartbeat["method"], "agent.report");
        println!("[MockServer] Received heartbeat: {} bytes", frame.payload.len());

        // 6. Optionally send agent.exec event
        // ...

        // 7. Close connection (simulate server restart)
        drop(stream);
        thread::sleep(Duration::from_millis(500));
    }
}
```

**CI Integration Test**:

```rust
#[cfg(test)]
mod integration {
    use super::*;
    use std::process::{Command, Child};
    use std::time::Duration;

    #[test]
    fn full_lifecycle_connect_heartbeat_disconnect_reconnect() {
        let mock = MockKomariServer::start();

        // Start agent as subprocess (or in-thread for simpler testing)
        let mut agent = Command::new("target/release/komari-agent")
            .args(&[
                "--token", "test-token",
                "--endpoint", &mock.endpoint(),
                "--interval", "0.1",  // fast tick for test
                "--ignore-unsafe-cert",
            ])
            .spawn()
            .expect("Failed to start agent");

        // Connection 1: agent connects, sends heartbeat
        let handle = thread::spawn(move || {
            mock.handle_one_connection();
        });

        // Wait for first connection to complete
        thread::sleep(Duration::from_secs(5));

        // Simulate server restart: start new mock server
        let mock2 = MockKomariServer::start();
        let handle2 = thread::spawn(move || {
            mock2.handle_one_connection();
        });

        // Wait for reconnection
        thread::sleep(Duration::from_secs(10));

        // Kill agent
        agent.kill().unwrap();
        agent.wait().unwrap();

        handle.join().unwrap();
        handle2.join().unwrap();
    }
}
```

**Pass criteria for CI**:
1. Agent connects to mock server within 5 seconds
2. Agent sends valid JSON-RPC 2.0 heartbeat
3. Mock server receives and validates heartbeat
4. Agent detects disconnection
5. Agent reconnects within backoff window
6. Test completes in < 30 seconds
7. No panics, no hangs, clean exit

---

## 6. VALIDATION RESOLUTIONS

This section documents issues discovered during cross-validation of the source documents (spec vs architecture-reference vs acceptance-criteria vs dependency-graph) and how each was resolved.

### 6.1 Resolved: CLI Parsing Library Conflict

**Discovered**: `phased-implementation-plan.md` (Phase 1, config.rs) originally listed `clap (derive)` as the CLI parser. `spec.md` (DD1) explicitly overrode this: "CLI parsing: hand-written ~150 lines. NOT clap."

**Resolution**: Spec wins. `config.rs` uses hand-written CLI parsing per DD1. Estimated savings: ~15 KB binary + unicode dependency chain.

**Impact**: `acceptance-criteria.md` AC-1.10 updated to reflect hand-written requirement. `architecture-reference.md` Section 4 type definitions updated.

---

### 6.2 Resolved: TLS Certificate Store

**Discovered**: `architecture-reference.md` initially specified `webpki-roots` for TLS cert store (Section 1.2: "All TLS via rustls with webpki-roots"). `spec.md` (DD5) overrode: "TLS certificate store: OS native roots. NOT webpki-roots."

**Resolution**: Spec wins. TLS uses OS native root certificates. On Linux read from `/etc/ssl/certs`; Windows uses CryptoAPI; macOS uses Security.framework. Estimated savings: ~80 KB binary.

**Impact**: `acceptance-criteria.md` AC-1.32 updated. `architecture-reference.md` Section 1.2 and Sections 2 (tls.rs), 7 (binary budget) updated.

---

### 6.3 Resolved: Gzip Compression Approach

**Discovered**: Initial design considered full DEFLATE implementation or flate2/miniz_oxide crate. `spec.md` (DD9) specified: "Gzip: fixed-Huffman encode-only ~200 lines. NOT full DEFLATE. NOT flate2/miniz_oxide."

**Resolution**: Spec wins. Implement fixed-Huffman DEFLATE encoder only (send side). Pre-computed Huffman tables per RFC 1951 Section 3.2.6. No full decode capability. 20-30% larger than optimal DEFLATE but saves ~250 lines and avoids 20-50 KB crate.

**Impact**: `acceptance-criteria.md` AC-5.7 updated with 200-line constraint. Binary budget adjusted.

---

### 6.4 Resolved: Feature Gate Default

**Discovered**: `spec.md` (DD13) specifies `default=[]` (core ~600 KB), `full` enables all (~876 KB). Initial phased plan did not explicitly document feature gate structure.

**Resolution**: All optional capabilities (`gpu-detection`, `terminal`, `ping`, `self-update`) are behind Cargo features. `default = []`. CI must test both `--no-default-features` and `--features full`.

**Impact**: `acceptance-criteria.md` Appendix C added. Binary budget split into feature-gated increments.

---

### 6.5 Resolved: Platform Module Architecture

**Discovered**: `dependency-graph.md` treated `platform/{linux,windows,macos,freebsd}.rs` as module gate files. `architecture-reference.md` Section 2.2 described them as per-platform FFI declaration files.

**Resolution**: Both correct. Platform files serve dual role: (a) `#[cfg(target_os)]` compile gate, (b) FFI function declarations for OS-specific APIs. No conflict.

---

### 6.6 Resolved: Line Count Estimates

**Discovered**: `phased-implementation-plan.md` estimated total ~4,940 lines. `architecture-reference.md` estimated ~4,900. `acceptance-criteria.md` per-task line counts sum to ~4,940. Minor discrepancies in individual module counts.

**Resolution**: Use `phased-implementation-plan.md` numbers as canonical (more granular breakdown). Accept +/- 10% variance per module. Line counts are estimates, not rigid constraints.

---

### 6.7 Resolved: RSS Budget -- Main Thread Stack

**Discovered**: `architecture-reference.md` Section 6.1 RSS budget includes "Stack (main thread): ~2 MB". This is OS-default thread stack size, not actively used memory. Some readings may show lower RSS (1-1.5 MB) because the stack is allocated but not committed.

**Resolution**: RSS measurement uses `VmRSS` (`/proc/<pid>/status`), which counts committed pages. The 2 MB stack is a virtual allocation but only committed pages (~100-200 KB in practice) count toward RSS. The budget of 2.97 MB is conservative; actual RSS expected closer to 1.5-2.5 MB.

---

### 6.8 Resolved: CI FreeBSD Target

**Discovered**: Multiple documents mention FreeBSD as a CI target. FreeBSD has tier-3 Rust support (requires `x86_64-unknown-freebsd` target via rustup). Cirrus CI offers FreeBSD runners. Cross-compilation from Linux is unreliable.

**Resolution**: Phase 1-3: FreeBSD handled via cross-compilation in CI (build only, no run). Phase 4+: Use Cirrus CI for FreeBSD `cargo test`. If Cirrus CI unavailable, accept build-only for FreeBSD with manual testing on FreeBSD VM.

---

### 6.9 Resolved: S.U.P.E.R Scores Between Documents

**Discovered**: `dependency-graph.md` Section 5 provides full S.U.P.E.R scoring (48 modules). `architecture-reference.md` Section 2 provides module descriptions but no S.U.P.E.R scores. No conflict; `dependency-graph.md` scores are canonical.

**Resolution**: This document uses `dependency-graph.md` Section 5 S.U.P.E.R scores as SSOT. Each milestone's S.U.P.E.R check references the applicable modules' scores.

---

## 7. DEFINITION OF DONE

### 7.1 "Phase Complete" Definition

A milestone (M1-M6) is **complete** when ALL of the following are true:

1. **All tasks in the phase are implemented**: Every file listed in the milestone's deliverables exists and compiles.
2. **All gate criteria are met**: Every criterion marked `[ ]` in the milestone's gate checklist is checked `[x]` with verified evidence.
3. **All tests pass**: `cargo test` returns zero failures for the current phase and all prior phases (no regressions).
4. **Code quality gates pass**: `cargo fmt --check` zero diffs. `cargo clippy -- -D warnings` clean. No `unsafe` outside approved FFI modules. All `// SAFETY:` comments present and correct.
5. **CI is green**: All platforms in the CI matrix (build + test + clippy + fmt) pass.
6. **Binary size within budget**: Measured binary size meets the milestone's threshold.
7. **RSS within budget** (from M2 onward): Measured RSS meets the milestone's threshold.
8. **Git commit made**: At least one commit pushed at the phase boundary.
9. **Peer review completed** (from M3 onward): At least one other developer has reviewed the phase's diff and approved.
10. **No open blocking issues**: All issues discovered during the phase are either resolved or explicitly deferred to a later phase with documented rationale.

### 7.2 "Project Complete" Definition (M6)

The project is **complete** when:

1. **All 6 milestones are complete** per Section 7.1.
2. **Wire compatibility proven**: Recorded Go agent session replays produce byte-for-byte matching JSON output.
3. **All 4 platforms validated**: Linux, Windows, macOS, and FreeBSD each pass full integration test.
4. **Hard constraints met**: Binary < 1 MB (Linux stripped), RSS < 3 MB, zero heap alloc in hot path, rustls-only.
5. **Documentation complete**: README.md, docs/protocol.md, docs/building.md all accurate and complete.
6. **Install scripts verified**: `install.sh` and `install.ps1` tested on fresh VMs.
7. **Git tag `v0.1.0` created and pushed** to `DeliciousBuding/komari-agent-rs`.
8. **Decision log finalized**: Any deviations from spec documented in `docs/plan/decisions.md`.

### 7.3 "Acceptance Criteria Met" Definition

An individual acceptance criterion (Section 2) is met when:

1. The criterion has been explicitly tested (not assumed).
2. The test result is recorded (CI log, manual test notes, `cargo test` output).
3. The result is `YES` (pass).
4. If the result is `NO`, a documented issue exists with an owner and target milestone for resolution.

### 7.4 Gates Summary Table

| Gate | M1 | M2 | M3 | M4 | M5 | M6 |
|------|:--:|:--:|:--:|:--:|:--:|:--:|
| Unit tests pass | X | X | X | X | X | X |
| Integration tests pass | - | - | X | X | X | X |
| Binary size threshold met | X | X | X | X | X | X |
| RSS threshold met | - | X | X | X | X | X |
| Zero-alloc verified | - | X | X | X | X | X |
| Cross-platform CI green | X | X | X | X | X | X |
| Wire compat (Go agent) | Partial | X | X | X | X | X |
| fmt + clippy clean | X | X | X | X | X | X |
| unsafe audit clean | X | X | X | X | X | X |
| Git commit | X | X | X | X | X | X |
| Peer review | - | - | X | X | X | X |
| Documentation | - | - | - | - | - | X |

---

## Appendix A: Quick Reference -- Per-Milestone Commands

```bash
# === M1: Foundation ===
cargo build --release
strip target/release/komari-agent
ls -lh target/release/komari-agent
cargo test
cargo fmt --check
cargo clippy -- -D clippy::all -D clippy::pedantic
./target/release/komari-agent --token=<token> --endpoint=<endpoint>

# === M2: Linux Metrics ===
# (same as M1, plus:)
cargo test zero_alloc::tick_has_zero_heap_allocations
./target/release/komari-agent --token=<token> --endpoint=<endpoint> &
AGENT_PID=$!; sleep 60
cat /proc/$AGENT_PID/status | grep VmRSS
kill $AGENT_PID

# === M3: Protocol FSM ===
cargo test fsm::
cargo test integration::full_lifecycle

# === M4: Cross-Platform ===
# CI handles per-platform testing
cargo test --target x86_64-pc-windows-msvc  # on Windows host
cargo test --target aarch64-apple-darwin     # on macOS host

# === M5: Terminal + Ping + Tools ===
cargo test integration::full_lifecycle
# Manual: ICMP ping test (requires CAP_NET_RAW or root)
sudo setcap cap_net_raw+ep target/release/komari-agent

# === M6: Polish ===
# Binary size final check
SIZE=$(stat -c%s target/release/komari-agent)
echo "Binary: ${SIZE} bytes ($(echo "scale=1; $SIZE/1024" | bc) KB)"
# Install script test
sudo bash scripts/install.sh
# Create git tag
git tag -a v0.1.0 -m "komari-agent-rs v0.1.0"
git push origin v0.1.0
```

---

## Appendix B: Cross-Reference -- Source Document Mapping

| Topic | This Document Section | Source |
|-------|----------------------|--------|
| Hard constraints | Header | `spec.md` SS1 |
| Confirmed design decisions | Header | `spec.md` SS2 |
| Feature matrix | SS3.5 | `spec.md` SS3 |
| Phase descriptions | SS1 milestone table | `phased-implementation-plan.md` SSPhases 1-6 |
| Gate criteria | SS1 per-milestone | `acceptance-criteria.md` SSPhases 1-6 |
| Acceptance criteria per module | SS2 | `acceptance-criteria.md` SStasks 1.1-6.6 |
| S.U.P.E.R scores | SS1 per-milestone | `dependency-graph.md` SS5 |
| Binary budget | SS3, SS3.4 | `architecture-reference.md` SS7 |
| RSS budget | SS4, SS4.2 | `architecture-reference.md` SS6 |
| Integration test plan | SS5 | `acceptance-criteria.md` Appendix A |
| Mock server design | SS5.2 | New (synthesized from integration test requirements) |
| Validation resolutions | SS6 | Cross-document diff |
| Definition of done | SS7 | New (synthesized from all source docs) |
| Protocol wire format | (architecture-reference.md SS5) | `architecture-reference.md` SS5 |

---

**Document end**. This is the single source of truth for milestones, acceptance criteria, and validation for komari-agent-rs. All other plan documents are reference material; discrepancies are resolved in Section 6 (Validation Resolutions).
