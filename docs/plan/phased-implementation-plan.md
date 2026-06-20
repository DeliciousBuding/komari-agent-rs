# Komari-Agent-RS Phased Implementation Plan

**Generated**: 2026-06-20
**Source analysis**: `docs/analysis/module-inventory.md` + `docs/analysis/chatgpt-architecture-advice.md`
**Target codebase**: Go `komari-agent-go` (~6,555 lines) -> Rust `komari-agent-rs` (~4,500 lines estimated)
**Core principle**: Sync single-threaded, zero-allocation hot path, explicit config passing (no globals).

---

## Architectural Pillars (from analysis docs)

| Pillar | Source | Rust Approach |
|---|---|---|
| Custom JSON | ChatGPT S1 | `JsonBuf` + `Field` enum + `EncodeJson` trait -- no serde in hot path |
| Event loop | ChatGPT S2 | Non-blocking socket + `poll()`/`select()` -- no epoll/kqueue, no async |
| Memory budget | ChatGPT S3 | Scratch arena + fixed `SmallVec<u8, N>` -- zero alloc in 1s tick |
| GPU detection | ChatGPT S4 | nvidia-smi CSV + rocm-smi key scanner + /sys/class/drm fallback |
| Protocol FSM | ChatGPT S5 | Two enums: `FallbackFsm` + `ConnectionFsm` -- heap-free state machine |
| Cross-platform | ChatGPT S6 | `cfg`-gated type aliases, static dispatch -- no trait objects |
| Binary size | ChatGPT S7 | `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"` |
| WebSocket crypto | ChatGPT S8 | Self-implemented SHA-1 + base64 (~200 lines, no external crate) |
| Gzip | ChatGPT S9 | Fixed Huffman encoder only (send side), no full deflate |
| Config | module-inventory S3 | clap derive, explicit `&Config` parameter passing, no `GlobalConfig` |

---

## PHASE 1: Foundation + Handshake (500 lines)

**Goal**: Binary compiles on 4 targets, establishes TLS WebSocket, sends static heartbeat.
**Duration estimate**: 2-3 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `Cargo.toml` | 35 | Deps: clap (derive), rustls (no_std-friendly), webpki-roots, zeroize. No async runtime. |
| `src/main.rs` | 15 | Parse config, call `server::run()`, exit with code. |
| `src/config.rs` | 105 | `Config` struct with clap derive. Fields: `server_url`, `agent_secret`, `agent_id`, `dns_servers` (Option), `ipv6_prefer` (bool). Env override via `#[arg(env)]`. No global. |
| `src/crypto.rs` | 195 | **SHA-1**: 80-round implementation per RFC 3174. **Base64**: encode-only per RFC 4648 (WebSocket accept key). Zero-alloc stack buffers. Combined ~195 lines per ChatGPT S8 estimate. |
| `src/encoding.rs` | 105 | `JsonBuf` struct (stack `[u8; 4096]` + cursor), `Field` enum (Str, I64, U64, F64, Bool, Null, Nested), `EncodeJson` trait with `fn encode_json(&self, buf: &mut JsonBuf)`. No Display/format! -- field-by-field push. |
| `src/server.rs` | 95 | `run(config: &Config)` entry. TCP connect via `std::net::TcpStream`, wrap in `rustls::StreamOwned`. HTTP upgrade request (minimal headers), verify 101 response + `Sec-WebSocket-Accept`, enter send-receive loop. Send one static heartbeat JSON on connect. |
| `.cargo/config.toml` | 8 | Release profile: `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`. |
| `.github/workflows/ci.yml` | 42 | Matrix: `ubuntu-latest` (x86_64-unknown-linux-gnu), `windows-latest` (x86_64-pc-windows-msvc), `macos-latest` (x86_64-apple-darwin), `ubuntu-latest` cross to `x86_64-unknown-freebsd`. Steps: checkout, install target, `cargo build --release`, archive artifact. |
| **Phase 1 subtotal** | **~600** | Slightly over 500 nominal; crypto and encoding are load-bearing for all later phases. |

### Internal Dependencies
```
main.rs -> config.rs
main.rs -> server.rs -> crypto.rs, encoding.rs
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| rustls API churn (0.23 vs 0.22) | Medium | Pin exact rustls version in Cargo.toml; the API surface we need (ClientConnection, StreamOwned) is stable across 0.22+. |
| SHA-1 correctness (WebSocket accept key) | Medium | Validate against RFC 6455 test vector: `dGhlIHNhbXBsZSBub25jZQ==` + `258EAFA5-E914-47DA-95CA-C5AB0DC85B11` -> `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`. |
| FreeBSD cross-compile toolchain | Low | Use `cross` tool or rustup target; FreeBSD has tier-2 Rust support. |
| jsonbuf buffer overflow | Medium | `JsonBuf` capacity 4096; heartbeat JSON is ~200 bytes. Add debug_assert on overflow; Phase 3 adds dynamic fallback to Vec for large payloads. |

### Success Criteria
- [ ] `cargo build --release` succeeds on all 4 targets in CI.
- [ ] Binary connects to a real Komari server endpoint, completes WS handshake (101 response).
- [ ] Server receives valid heartbeat JSON (`{"jsonrpc":"2.0","method":"report","params":{...}}`).
- [ ] Binary size < 2 MB stripped per target.
- [ ] `cargo test` passes (crypto test vectors, encoding round-trip).

---

## PHASE 2: Linux Metrics + Zero-Alloc Loop (+800 lines, cumulative 1,300)

**Goal**: Full Linux monitoring suite, scratch allocator, zero-allocation 1-second loop. Prove RSS < 3 MB.
**Duration estimate**: 4-5 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `src/memory.rs` | 140 | **ScratchArena**: bump allocator backed by `[u8; 8192]` stack buffer. `alloc(len) -> &mut [u8]`, `reset()` on each tick. **SmallVec**: `SmallVec<T, N>` with inline storage, falls back to Vec only if N exceeded (should never happen in hot path). Lifetime tied to arena. |
| `src/platform.rs` | 15 | `#[cfg(target_os)]` re-exports. `type CurrentCpu = linux::Cpu;` etc. |
| `src/platform/linux.rs` | 5 | Module gate. |
| `src/monitor/mod.rs` | 55 | `Monitor` struct holding arena + SmallVec buffers. `tick(&mut self, config: &Config) -> JsonBuf` orchestrator. 1-second `thread::sleep` loop in `server.rs`. Collectors called in fixed order, each pushes fields into `JsonBuf`. Arena reset after each tick. |
| `src/monitor/cpu.rs` | 85 | Parse `/proc/stat` (first line for total, per-core lines). Parse `/proc/cpuinfo` (model name, cores, MHz). Compute delta from previous sample for usage %. No allocation: read into arena buffer, line-scan with `&[u8]` slices. |
| `src/monitor/memory.rs` | 140 | Parse `/proc/meminfo` for MemTotal/MemAvailable/Buffers/Cached/SwapTotal/SwapFree. Implement 3-mode RAM calculation matching Go `Ram()` dispatch exactly: mode 0 (used = total - available), mode 1 (used = total - free - buffers - cached), mode 2 (used = total - free). Config field `mem_mode: u8` drives dispatch. |
| `src/monitor/disk.rs` | 80 | Read `/proc/mounts`, filter with `is_physical_disk()` (30+ excludes matching Go: /dev/loop, /sys, /proc, /run, /snap, overlay, tmpfs, devtmpfs, cgroup, etc.). For each physical mount: `statvfs` for total/used/free. Collect all into arena-borrowed slices. |
| `src/monitor/load.rs` | 35 | Parse `/proc/loadavg` (3 floats). Trivial. |
| `src/monitor/process.rs` | 40 | Count `/proc` entries where name is numeric (PID). Filter `/proc/<pid>/status` for state=R (running). |
| `src/monitor/uptime.rs` | 25 | Parse `/proc/uptime` first field. |
| `src/monitor/network.rs` | 90 | Parse `/proc/net/dev` for interface RX/TX bytes/packets. Compute speed by delta from previous sample / 1s. Count connections via `/proc/net/tcp` + `/proc/net/tcp6` (count entries, not full parse). |
| `src/server.rs` (expand) | 40 | Integrate monitor tick loop. After WS connect: loop { monitor.tick(); encode to JsonBuf; send WS text frame; sleep 1s; }. |
| **Phase 2 subtotal** | **~750** | |

### Internal Dependencies
```
memory.rs  <-- monitor/mod.rs, monitor/cpu.rs, monitor/memory.rs, ...
platform/linux.rs <-- monitor/*
monitor/mod.rs -> monitor/{cpu,memory,disk,load,process,uptime,network}.rs
server.rs -> monitor/mod.rs
encoding.rs -> memory.rs (EncodeJson uses arena for temporaries)
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| /proc format differs across kernel versions | Medium | Test on kernel 4.19+ (CentOS 7), 5.15 (Ubuntu 22.04), 6.x (latest). MemAvailable absent on <3.14 -- fallback to MemFree+Buffers+Cached. |
| 3-mode RAM compat mismatch with Go | High | Port Go unit test vectors verbatim. Cross-validate against Go agent on same host. |
| is_physical_disk filter divergence | Medium | Port the 30-entry exclude list exactly. Add integration test: Rust vs Go disk list on same Linux host. |
| Arena overflow (8 KB too small) | Low | 8 KB covers all /proc reads + JSON output. Add `debug_assert!` on bump pointer; Phase 2 max per-tick JSON is ~2 KB. |
| Network speed first-tick delta is 0 | Low | Skip sending speed on first tick (delta unavailable), matching Go behavior. |

### Success Criteria
- [ ] All Linux collectors produce JSON identical in structure to Go agent (field names, types, nesting).
- [ ] 3-mode RAM calculation matches Go output for modes 0, 1, 2 on same host.
- [ ] `is_physical_disk()` produces same mount list as Go on same host.
- [ ] Zero heap allocation in `monitor.tick()` hot path (verify via `dhat` or custom allocator hook).
- [ ] RSS < 3 MB after 60 seconds of running (measure via `/proc/<pid>/status` VmRSS).
- [ ] 1-second tick jitter < 50ms under idle system.
- [ ] `cargo test` passes all collector unit tests with fixture /proc data.

---

## PHASE 3: Protocol FSM + Fallback (+400 lines, cumulative 1,700)

**Goal**: v2/v1 protocol negotiation, HTTP POST fallback, exponential backoff, exec/ping stubs. Prove recorded session replay.
**Duration estimate**: 3-4 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `src/rpc/mod.rs` | 5 | Module gate. |
| `src/rpc/v2.rs` | 65 | JSON-RPC 2.0 types: `Request { jsonrpc, method, params, id }`, `Response { jsonrpc, result/error, id }`. Method constants: `report`, `task`, `ping`, `basicInfo`. EncodeJson impl for each. |
| `src/rpc/v1.rs` | 45 | V1 compatibility types. `V1Report` struct with flat fields (no jsonrpc envelope). Unify gzip here (removing Go duplication). |
| `src/server/protocol.rs` | 120 | `FallbackFsm` enum: `V2Ws` -> `V2WsFail1` -> `V2WsFail2` -> `V2HttpPost` -> `V1HttpPost`. `ConnectionFsm` enum: `Disconnected`, `Connecting`, `Connected`, `Degraded`. 3-strike counter resets on successful v2 WS message. State transitions are pure match arms, no allocation. |
| `src/server/backoff.rs` | 50 | `Backoff { initial: Duration, max: Duration, current: Duration, attempts: u32 }`. `next_delay() -> Duration`, `reset()`. Jitter: +/- 25% random. Caps at 5 minutes. |
| `src/server/http_fallback.rs` | 70 | `post_json(url, body, timeout)` using `std::net::TcpStream` + manual HTTP/1.1 POST with `Content-Type: application/json`. No reqwest -- keep binary small. TLS via rustls for HTTPS endpoints. |
| `src/server/task.rs` | 75 | Stub handlers: `handle_exec(cmd, timeout) -> ExecResult` (returns `{status: "ok", output: "stub"}`), `handle_ping(target, count) -> PingResult` (returns `{status: "ok", results: []}`). Real implementations in Phase 5. |
| `src/server/reconnection.rs` | 70 | Reconnection loop: select-style poll over 3 concerns -- dataTicker (1s monitor), heartbeatTicker (30s ping), readDone (WS close). On disconnect: backoff.sleep(), reconnect. Exact match of Go's `select {}` pattern with `time.Ticker`. In Rust: `std::thread::sleep` + non-blocking socket read with timeout. |
| **Phase 3 subtotal** | **~500** | |

### Internal Dependencies
```
rpc/v2.rs, rpc/v1.rs -> encoding.rs (EncodeJson)
server/protocol.rs -> rpc/v2.rs, rpc/v1.rs, server/backoff.rs
server/http_fallback.rs -> encoding.rs
server/task.rs -> rpc/v2.rs
server/reconnection.rs -> server/protocol.rs, server/http_fallback.rs, server/backoff.rs
server.rs -> server/reconnection.rs, server/task.rs
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| FSM edge case: network drop during fallback | Medium | Record Go agent behavior with `tcpdump` + Wireshark on: connect->WS fail->HTTP POST fail->v1 POST. Replay exact sequences. |
| HTTP POST body larger than arena | Low | Phase 3 payloads (basicInfo, report) fit in 4 KB. Add `JsonBuf::reserve_or_extend()` that falls back to heap Vec only when arena exhausted. |
| Backoff jitter causes test flakiness | Low | Seed PRNG with fixed value in tests. |
| reconnection loop starvation (tick takes >1s) | Low | Add tick timeout: if `monitor.tick()` > 950ms, skip one cycle and log warning. Monitor is designed to complete in <10ms. |

### Success Criteria
- [ ] FSM transitions verified with a recorded session: capture Go agent's full connect->heartbeat->disconnect cycle, replay against Rust agent, compare JSON outputs frame-by-frame.
- [ ] 3-strike counter: 3 consecutive v2 failures triggers v1 fallback; 1 v2 success resets counter.
- [ ] HTTP POST fallback sends identical JSON body to WS mode (same `EncodeJson` path).
- [ ] Backoff: after 3 failures, delay is ~1s; after 10 failures, delay caps at 5 min.
- [ ] Reconnection loop survives server restart (server goes down, agent reconnects within backoff window).
- [ ] `cargo test` includes FSM state transition table test (all 12 edges).

---

## PHASE 4: Cross-Platform Metrics (+1,200 lines, cumulative 2,900)

**Goal**: Windows/macOS/FreeBSD metrics, GPU across 4 platforms, OS & virtualization detection. CI matrix fully green on all 4 OS targets.
**Duration estimate**: 6-8 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `src/platform/linux.rs` (expand) | 5 | Re-export Linux collectors. |
| `src/platform/windows.rs` | 25 | Windows platform module. `mod windows_cpu; mod windows_memory; ...` |
| `src/platform/macos.rs` | 20 | macOS platform module. |
| `src/platform/freebsd.rs` | 20 | FreeBSD platform module. |
| `src/monitor/cpu_windows.rs` | 70 | CPU: `GetSystemInfo` (core count), registry `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0` for name, performance counters via `QueryPerformanceCounter` (no WMI). |
| `src/monitor/memory_windows.rs` | 75 | Memory: `GlobalMemoryStatusEx` for total/avail/swap. `GetPerformanceInfo` for detailed breakdown. 3-mode dispatch matching Go (using same field names). |
| `src/monitor/disk_windows.rs` | 60 | Disk: `GetLogicalDrives` + `GetDiskFreeSpaceExW` per drive. Filter type DRIVE_FIXED. |
| `src/monitor/process_windows.rs` | 45 | Process: `K32EnumProcesses` (PSAPI) for count. No per-process detail (matching Go). |
| `src/monitor/network_windows.rs` | 70 | Network: `GetIfTable2` (iphlpapi) for interface list + RX/TX bytes. Compute speed by 1s delta. Connection count via `GetTcpTable2`/`GetTcp6Table2`. |
| `src/monitor/cpu_macos.rs` | 50 | CPU: `sysctl kern.hostname`/`machdep.cpu.brand_string`/`hw.logicalcpu`. Load via `getloadavg()`. |
| `src/monitor/memory_macos.rs` | 60 | Memory: `sysctl hw.memsize` + `host_statistics64` (VM page counts). Swap via `sysctl vm.swapusage`. |
| `src/monitor/disk_macos.rs` | 40 | Disk: `getmntinfo` + `statfs`. Filter physical (exclude devfs, autofs, etc.). |
| `src/monitor/process_macos.rs` | 35 | Process: `sysctl kern.proc.all` count. |
| `src/monitor/cpu_freebsd.rs` | 50 | CPU: `sysctl hw.model`/`hw.ncpu`. Load via `getloadavg()`. |
| `src/monitor/memory_freebsd.rs` | 60 | Memory: `sysctl hw.physmem`/`hw.usermem` + `kvm_getswapinfo`. |
| `src/monitor/disk_freebsd.rs` | 40 | Disk: `getmntinfo` + `statfs`. |
| `src/monitor/process_freebsd.rs` | 35 | Process: `sysctl kern.proc.all` count. |
| `src/monitor/load.rs` (expand) | 20 | Load: platform dispatch -- Linux /proc/loadavg, macOS/FreeBSD getloadavg(), Windows performance counter. |
| `src/monitor/gpu/mod.rs` | 55 | `GpuDetector` trait (or cfg-gated type). `detect() -> Vec<GpuInfo>`. Dispatches per platform. |
| `src/monitor/gpu/linux.rs` | 135 | **NVIDIA**: exec `nvidia-smi --query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total --format=csv,noheader` (CSV, not XML -- per ChatGPT S4). **AMD**: exec `rocm-smi --showproductname --showtemp --showuse --showmeminfo vram` or parse `/opt/rocm/bin/rocm-smi`. **Fallback**: parse `/sys/class/drm/card*/device/vendor` (0x10de=NVIDIA, 0x1002=AMD) + read model from `/sys/class/drm/card*/device/product_name` if nvidia-smi missing (container). |
| `src/monitor/gpu/windows.rs` | 115 | DXGI via `windows-rs` crate (`Windows::Win32::Graphics::Dxgi`). Enumerate adapters, get desc for name + VRAM. No COM vtables manually (unlike Go's raw `syscall.SyscallN`). For utilization: `GetPerformanceData` via WMI fallback only if DXGI returns 0. |
| `src/monitor/gpu/macos.rs` | 55 | Exec `system_profiler SPDisplaysDataType -json` (macOS 10.15+), parse JSON for `spdisplays_vram`, `sppci_model`. Fallback: `system_profiler SPDisplaysDataType` plain text parse. |
| `src/monitor/gpu/freebsd.rs` | 65 | Exec `pciconf -lv | grep -B3 -A15 VGA` for GPU name + vendor. Limited VRAM info (FreeBSD has no standard GPU VRAM API). |
| `src/monitor/os.rs` | 115 | **Linux**: parse `/etc/os-release` (ID, VERSION_ID, PRETTY_NAME). Heuristic for Android (build.prop), Synology (/etc.defaults/VERSION), PVE (pveversion), fnOS. **Windows**: registry `CurrentVersion` -> ProductName + CurrentBuild + UBR. **macOS**: exec `sw_vers` -> ProductName + ProductVersion. **FreeBSD**: `uname -r` + `freebsd-version`. |
| `src/monitor/virtualization.rs` | 85 | **Container**: `/proc/self/cgroup` (docker, kubepods, lxc), `/.dockerenv` existence. **VM**: CPUID hypervisor bit, DMI `/sys/class/dmi/id/product_name` (KVM, VMware, VirtualBox, Hyper-V). **Windows**: CPUID via `__cpuid` intrinsic. **macOS**: `sysctl kern.hv_support`. |
| `src/monitor/uptime.rs` (expand) | 20 | Platform dispatch: Linux /proc/uptime, Windows `GetTickCount64`, macOS/FreeBSD `sysctl kern.boottime`. |
| `.github/workflows/ci.yml` (expand) | 75 | Full matrix: 4 OS x {test, build}. Add `cargo test` per platform. Add `cargo clippy -- -D warnings`. Add `cargo fmt --check`. Artifact upload for release builds. |
| **Phase 4 subtotal** | **~1,535** | |

### Internal Dependencies
```
platform/{linux,windows,macos,freebsd}.rs -> monitor/{cpu,memory,disk,network,process,...}_{platform}.rs
monitor/gpu/mod.rs -> monitor/gpu/{linux,windows,macos,freebsd}.rs
monitor/mod.rs -> platform::Current* type aliases (static dispatch)
monitor/os.rs -> encoding.rs
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| Windows-rs DXGI API: VRAM reporting differs from Go's COM vtables | High | Go does manual COM vtable calls. windows-rs may report different VRAM values (dedicated vs shared). Cross-validate against Go on same Windows host; add config flag `gpu_mem_mode` if needed. |
| nvidia-smi not in PATH (container, minimal install) | Medium | Primary: `nvidia-smi` in PATH. Fallback 1: `/usr/bin/nvidia-smi`. Fallback 2: `/sys/class/drm` vendor check (works without nvidia-smi). |
| rocm-smi output format changes across ROCm versions | Medium | Test with ROCm 5.x and 6.x. The key scanner approach (per ChatGPT S4) avoids full JSON parse; scan for known keys like "GPU\[" and "Temperature". |
| macOS SystemProfiler JSON requires 10.15+ | Low | Fallback to plain text parse for 10.14 and below (EOL, low priority). |
| FreeBSD test hardware availability | Medium | Use Cirrus CI (freebsd runner) or QEMU VM for testing. Tier-2 Rust target may have issues. |
| OS detection false positive on niche distros | Low | Port Go's heuristics exactly. Add unknown -> "Linux" fallback with raw os-release dump. |

### Success Criteria
- [ ] All 4 platform binaries pass `cargo test` in CI (unit tests with fixture data; integration tests where platform matches).
- [ ] GPU detection: NVIDIA (nvidia-smi path), AMD (rocm-smi path), Intel (DRM fallback), Apple Silicon (system_profiler).
- [ ] Memory 3-mode: validated against Go on Windows (mode 0/1/2 match).
- [ ] OS detection: correctly identifies Ubuntu 22.04, Debian 12, Windows 11, macOS 15, FreeBSD 14.
- [ ] Virtualization: detects Docker, KVM, VMware, Hyper-V.
- [ ] CI matrix: 4 OS x (build + test + clippy + fmt) all green.
- [ ] No platform-specific code in shared modules (all behind `cfg(target_os)` or platform module re-exports).

---

## PHASE 5: Terminal, Ping, Gzip, DNS, Self-Update (+800 lines, cumulative 3,700)

**Goal**: Full terminal PTY/ConPTY, ICMP/TCP/HTTP ping, gzip compression, Cloudflare Access, DNS resolver, self-update. Full integration test passing.
**Duration estimate**: 6-8 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `src/terminal/mod.rs` | 70 | `Terminal` trait: `close()`, `read(buf) -> Result<usize>`, `write(data)`, `resize(cols, rows)`, `wait() -> ExitStatus`. `start_terminal(cmd: &str, cols: u16, rows: u16) -> Terminal`. cfg-gated implementation selection. |
| `src/terminal/unix.rs` | 140 | Unix PTY via `posix_openpt()` + `grantpt()` + `unlockpt()` + `fork()` + `execvp()`. Slave: `setsid()`, `ioctl(TIOCSCTTY)`, `dup2` stdin/stdout/stderr. Signal handling: SIGCHLD for exit detection. Resize: `ioctl(TIOCSWINSZ)`. |
| `src/terminal/windows.rs` | 155 | Windows ConPTY via `CreatePseudoConsole()` (Windows 10 1809+). Startup info with `STARTUPINFOEX`, `InitializeProcThreadAttributeList`, `UpdateProcThreadAttribute(PPROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE)`. I/O via overlapped pipes. Resize: `ResizePseudoConsole()`. Signal via `TerminateProcess()`. |
| `src/server/ping_icmp.rs` | 85 | ICMP echo via raw socket (`socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` on Linux -- needs CAP_NET_RAW or setuid). Build ICMP header manually (type 8, code 0, checksum). Windows: `IcmpSendEcho2` via iphlpapi. Timeout + RTT calculation. |
| `src/server/ping_tcp.rs` | 45 | TCP connect to `host:port` with `TcpStream::connect_timeout()`. Success = port open. Measure connect time as RTT. |
| `src/server/ping_http.rs` | 60 | HTTP GET/HEAD to URL with timeout. Check status 2xx/3xx. Measure total round-trip. Add optional `Host` header override. |
| `src/compress.rs` | 80 | Gzip encode-only: fixed Huffman tree (pre-computed code lengths per RFC 1951 S3.2.6). Deflate block with BTYPE=01. Minimal gzip header (ID1, ID2, CM=8, FLG=0, MTIME=0, XFL=0, OS=255). Adler32 checksum. ~400 bytes of pre-computed tables. Per ChatGPT S9: avoid full deflate. |
| `src/dns.rs` | 165 | Full custom DNS resolver. `resolve(host: &str, prefer_ipv6: bool) -> Vec<IpAddr>`. UDP send to DNS server (configurable list, default `[8.8.8.8, 1.1.1.1, 114.114.114.114, 223.5.5.5]`). Build DNS query (A + AAAA), parse response. Cache with TTL. Dialer factory: create TCP stream with resolved IP. Match Go behavior: prefer IPv4 by default, config flag for IPv6 preference. |
| `src/server/cf_access.rs` | 55 | Cloudflare Access: add `CF-Access-Client-Id` and `CF-Access-Client-Secret` headers to WS upgrade and HTTP POST requests when `cf_client_id` config is set. |
| `src/update.rs` | 85 | Self-update: check GitHub releases API for latest version (semver compare). Download asset matching current platform (linux-amd64, windows-amd64, etc.). Verify SHA256 checksum. Replace current binary (on Unix: write to temp, rename; on Windows: write to `.new`, use `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)` + self-terminate). Exit code 42 signals service manager to restart. |
| `tests/integration.rs` | 105 | Full integration test: start dummy Komari server (simple TCP that speaks WS protocol), connect agent, verify heartbeat, send task command, verify response, disconnect, verify reconnection. Uses `std::net::TcpListener` for dummy server. |
| **Phase 5 subtotal** | **~1,045** | |

### Internal Dependencies
```
terminal/mod.rs -> terminal/{unix,windows}.rs
server/ping_*.rs -> dns.rs (resolve target)
server/task.rs -> server/ping_*.rs, terminal/mod.rs (real implementations replace stubs)
compress.rs -> encoding.rs (gzip-encode json output)
server.rs -> dns.rs, server/cf_access.rs, update.rs
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| ConPTY: CreatePseudoConsole fails on Windows <10 1809 | Low | Detect at runtime: if `CreatePseudoConsole` absent in kernel32.dll, return error "ConPTY not available (requires Windows 10 1809+)". |
| PTY: posix_openpt not available (musl, old glibc) | Low | Fallback to `open("/dev/ptmx")` + `ptsname()`. |
| ICMP: raw socket requires root/CAP_NET_RAW | High | Attempt raw socket. On PermissionDenied: log warning, ICMP ping returns error "permission denied, use TCP/HTTP ping instead". Server-side should handle this gracefully (it already does for Go agent). |
| Gzip: fixed Huffman produces larger output than dynamic | Low | Acceptable tradeoff per ChatGPT S9. Fixed Huffman is 20-30% larger than optimal but avoids 3000+ lines of deflate. Server-side must accept valid gzip (it does). |
| Self-update: binary replace race condition | Medium | Unix: write new binary to `$binary.new`, `chmod +x`, `rename($binary.new, $binary)` -- atomic on same FS. Windows: `MoveFileEx` with delay-until-reboot, then `ExitProcess(42)`. |
| DNS cache poisoning (stale entries) | Low | TTL honored. Max cache entry lifetime 5 minutes. Config `dns_ttl_override` for override. |
| CI: dummy server integration test timing flaky | Medium | Use generous timeouts (5s for handshake, 10s for reconnection). Dummy server runs in same process as test (`std::thread::spawn`). |

### Success Criteria
- [ ] Terminal: `exec ls -la` returns directory listing via PTY on Linux; `dir` works via ConPTY on Windows 10+.
- [ ] Terminal graceful shutdown: closing WS sends SIGHUP (Unix) / TerminateProcess (Windows), waitpid confirms exit.
- [ ] ICMP ping to 8.8.8.8 returns RTT < 500ms (with CAP_NET_RAW).
- [ ] TCP ping to google.com:443 returns RTT.
- [ ] HTTP ping to https://google.com returns 200.
- [ ] Gzip: encoding 2 KB JSON produces valid gzip accepted by `gunzip`.
- [ ] DNS: resolve with each configured DNS server; prefer IPv4; IPv6 flag routes to AAAA.
- [ ] Self-update: detects newer GitHub release, downloads, replaces binary, exits 42.
- [ ] CF Access: when configured, WS upgrade includes both CF headers.
- [ ] Full integration test passes: connect -> heartbeat -> task exec -> disconnect -> reconnect.

---

## PHASE 6: Polish + Packaging (+200 lines, cumulative 3,900)

**Goal**: Windows toast notifications, auto-discovery, persistent network stats, install scripts, documentation.
**Duration estimate**: 2-3 days.

### File List

| File | Lines | Purpose |
|---|---|---|
| `src/platform/windows_toast.rs` | 75 | Windows toast notification via `windows-rs` `Windows::UI::Notifications`. `ToastNotificationManager::CreateToastNotifier(app_id)`. Template: `ToastNotificationManager::GetTemplateContent(ToastTemplateType_ToastText02)`. Set text, show. Requires app registration for full toast -- fallback to `MessageBoxW` for simple alerts. |
| `src/autodiscovery.rs` | 60 | Auto-registration: on startup, send `POST /api/agent/discover` with host info (hostname, OS, IP). Server returns agent ID or instructs manual registration. Config file `auto-discovery.json` persists registration state. Poll interval: 60s until registered, then never. |
| `src/monitor/netstatic.rs` | 85 | Persistent traffic history to `net_static.json`. `VecDeque<TrafficData>` with max 720 entries (12 hours at 1/min). On tick: push new sample, serialize to JSON, write to temp file, `rename` atomic. On startup: read existing file. API: `get_traffic_between(start, end) -> Vec<TrafficData>`, `get_total_traffic() -> TrafficData`, `clear()`. |
| `scripts/install.sh` | 45 | Linux/macOS install: detect OS, download binary from GitHub releases, install to `/usr/local/bin/komari-agent`, create systemd/launchd service, start. |
| `scripts/install.ps1` | 40 | Windows install: download, extract to `$env:ProgramFiles\komari-agent`, create scheduled task or nssm service, start. |
| `README.md` (expand) | 80 | Full documentation: overview, features, platform support, installation, configuration, building from source, architecture overview (link to plan). |
| `docs/protocol.md` | 60 | Protocol documentation: v2 JSON-RPC methods, v1 fallback format, heartbeat schema, task schema. |
| `docs/building.md` | 40 | Build guide: prerequisites (Rust 1.80+, platform toolchains), `cargo build --release`, cross-compilation, CI. |
| `src/server.rs` (expand) | 25 | Integrate autodiscovery + netstatic flush on shutdown. Graceful shutdown on SIGINT/SIGTERM: flush netstatic, close WS, exit 0. |
| **Phase 6 subtotal** | **~510** | |

### Internal Dependencies
```
src/autodiscovery.rs -> src/config.rs, src/monitor/os.rs
src/monitor/netstatic.rs -> src/monitor/network.rs (TrafficData type), encoding.rs
src/platform/windows_toast.rs -> src/config.rs
```

### Risks
| Risk | Probability | Mitigation |
|---|---|---|
| Windows toast: app registration required for Win32 | Medium | Use `windows-rs` `ToastNotificationManager`. Fallback: `MessageBoxW` (always works). Toast is low-priority cosmetic feature. |
| net_static.json corruption on crash | Low | Write to `net_static.json.tmp`, `rename` atomically on same FS. On startup, if `.tmp` exists, check if valid JSON, use if newer. |
| Auto-discovery: server may reject | Low | Agent handles HTTP 4xx gracefully: log warning, retry after 60s. Server 200 with `{"id": "agent-xxx"}` updates config. |

### Success Criteria
- [ ] Windows toast: test notification appears on Windows 10/11.
- [ ] Auto-discovery: fresh agent registers with server; restart picks up existing `auto-discovery.json`.
- [ ] Network stats: `net_static.json` persists across restarts. `get_total_traffic()` returns cumulative data.
- [ ] Install scripts: `install.sh` completes on Ubuntu 22.04 and macOS 15; `install.ps1` on Windows 11. Agent starts and heartbeats.
- [ ] README covers all sections: install, configure, run, troubleshoot.
- [ ] All Phase 1-5 success criteria still pass (no regressions).

---

## Phase Dependency Graph

```
Phase 1 (Foundation)
  |
  v
Phase 2 (Linux Metrics) ──┐
  |                         |
  v                         v
Phase 3 (Protocol FSM)    Phase 4 (Cross-Platform)  ← can parallelize if 2 developers
  |                         |
  +----------+--------------+
             |
             v
        Phase 5 (Terminal, Ping, DNS, Update)
             |
             v
        Phase 6 (Polish)
```

Critical path: P1 -> P2 -> P3 -> P5 -> P6. P4 can run in parallel with P3.

---

## Cumulative Line Budget

| Phase | Added | Cumulative | Budget |
|---|---|---|---|
| P1: Foundation | 600 | 600 | 500 |
| P2: Linux Metrics | 750 | 1,350 | 1,300 |
| P3: Protocol FSM | 500 | 1,850 | 1,700 |
| P4: Cross-Platform | 1,535 | 3,385 | 2,900 |
| P5: Terminal + Ping | 1,045 | 4,430 | 3,700 |
| P6: Polish | 510 | 4,940 | 3,900 |

**Note**: Estimates exceed nominal ~4,500. At each phase boundary, non-essential helpers can be trimmed. The core hot-path (monitoring + encoding + server) accounts for ~2,500 lines; the rest is platform adapters, protocol, and tooling. These estimates are conservative (include comments, blanks, tests inline).

---

## Key Architecture Decisions (from analysis docs)

1. **No async runtime**. tokio adds 1+ MB binary and 200+ dependencies. Sync `std::net` + `poll()`/`select()` is sufficient for 1 connection + 1s tick. (ChatGPT S2)
2. **No serde in hot path**. `EncodeJson` trait + `JsonBuf` avoids serde's 300+ KB codegen and allocation-heavy Value type. (ChatGPT S1)
3. **Scratch arena, not malloc**. 8 KB stack buffer reset each tick. `SmallVec` for inline storage. All /proc reads borrow from arena. (ChatGPT S3)
4. **Explicit Config, no global**. Every function that needs config takes `&Config`. Testability: inject config without env/setup. (module-inventory S3.1)
5. **cfg-gated type aliases, not trait objects**. `type CurrentCpu = linux::Cpu` avoids vtable dispatch in hot path. (ChatGPT S6)
6. **Self-implemented crypto**. SHA-1 (~100 lines) + base64 (~60 lines) avoids pulling in `sha1` + `base64` crates (+ their dep trees). (ChatGPT S8)
7. **Binary size target**: < 2 MB stripped per platform. Achieved via opt-level="z" + lto="fat" + no async runtime + minimal crate selection. (ChatGPT S7)

---

## Test Strategy

| Phase | Test Type | Coverage Target |
|---|---|---|
| P1 | Unit: SHA-1 RFC vector, base64 RFC vector, JsonBuf round-trip | 100% crypto + encoding |
| P2 | Unit: each /proc parser with fixture files; integration: RSS measurement | 90% monitor/ |
| P3 | Unit: FSM state table (all transitions); integration: recorded session replay | 95% protocol.rs |
| P4 | Unit: per-platform fixtures; CI: all 4 OS green | 80% platform/ |
| P5 | Integration: dummy server full lifecycle; unit: ping, gzip, DNS | 85% server/ |
| P6 | Manual: toast, install scripts; unit: autodiscovery, netstatic | 75% |
