# Komari Agent Rust — Definitive Architecture Reference

**Version**: 1.0.0
**Date**: 2026-06-20
**Status**: Blueprint — implementable without further design documents
**Source**: Synthesized from 13 parallel design documents covering JSON codec, WebSocket/HTTP transport, protocol FSM, gzip compression, event loop, memory arena, configuration, platform abstraction, GPU detection, metrics collectors, binary optimization, testing strategy, and implementation plan.
**Target codebase**: Go `komari-agent-go` (~6,555 lines) → Rust `komari-agent-rs` (~4,900 lines estimated)

---

## 1. EXECUTIVE SUMMARY

### 1.1 Vision

Komari-agent-rs is a **zero-dependency sync agent** that collects system metrics from Linux, Windows, macOS, and FreeBSD hosts and reports them to a Komari monitoring server over WebSocket (JSON-RPC 2.0) with HTTP POST fallback (JSON-RPC 1.0 compatibility). It supports remote command execution, ICMP/TCP/HTTP ping, interactive terminal (PTY/ConPTY), GPU monitoring, and self-update.

The rewrite from Go to Rust targets three hard constraints:

| Constraint | Target | Rationale |
|---|---|---|
| Binary size | < 1 MB stripped | Minimal footprint for edge/VPS deployment |
| RSS (steady-state) | < 3 MB | Run comfortably on 256 MB VMs alongside workloads |
| TLS library | rustls only | No OpenSSL linkage; pure Rust TLS |
| Concurrency model | Sync single-threaded | One connection, one tick loop; no async runtime |

### 1.2 Hard Constraints

1. **< 1 MB binary**: Achieved via `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`, custom JSON encoder (no serde), custom gzip encoder (no flate2), self-implemented SHA-1+base64 (no crypto crates), no async runtime.
2. **< 3 MB RSS**: Achieved via stack-based scratch arena, `SmallVec` inline storage, zero heap allocation in the 1-second monitoring tick hot path, single-threaded execution.
3. **rustls-only**: All TLS via `rustls` with `webpki-roots`. No `native-tls`, no OpenSSL, no platform CA bundles.
4. **Sync**: `std::net::TcpStream` + `poll()`/`select()` for non-blocking socket read with timeout. No tokio, no async-std, no smol.

### 1.3 Target Metrics

| Metric | Phase 1 | Phase 2 | Phase 6 (Final) |
|---|---|---|---|
| Binary size (linux-amd64) | < 2 MB | < 1.5 MB | < 1 MB |
| Binary size (windows-amd64) | < 2.5 MB | < 2 MB | < 1.2 MB |
| RSS after 60s | N/A | < 3 MB | < 3 MB |
| Tick jitter (1s interval) | N/A | < 50ms | < 50ms |
| Heap allocations per tick | N/A | 0 | 0 |
| Test coverage | 100% crypto+encoding | 90% monitor/ | 85% total |
| CI platforms green | 4 | 4 | 4 |

### 1.4 Key Architectural Decisions

1. **Custom JSON encoder** (not serde): serde adds 300+ KB of codegen and an allocation-heavy `Value` type. Our `EncodeJson` trait with `JsonBuf` stack buffer produces wire-identical JSON at zero allocation in the hot path.
2. **Custom gzip encoder** (not flate2): flate2/miniz_oxide adds 20-50 KB. Our fixed-Huffman DEFLATE encoder is ~350 lines and produces valid gzip accepted by the server.
3. **Self-implemented SHA-1 + base64** (not sha1/base64 crates): ~160 lines combined for WebSocket handshake. No dependency tree.
4. **No async runtime**: One connection, one tick loop. Sync I/O with non-blocking socket read via `poll()`/`select()` is simpler and eliminates 1+ MB of binary.
5. **Explicit `&Config` everywhere** (no global): Every function receives config explicitly. Enables testing without environment setup.
6. **cfg-gated type aliases** (not trait objects): `type CurrentCpu = linux::Cpu` avoids vtable dispatch in the hot path.

---

## 2. MODULE MAP

### 2.1 Complete `src/` Directory Tree

```
src/
├── main.rs                    # Entry point (15 lines)
├── app.rs                     # Application orchestration (80 lines)
├── config.rs                  # CLI + env + file config (105 lines)
├── arena.rs                   # ScratchArena + SmallVec (140 lines)
├── json.rs                    # JsonBuf + Field + EncodeJson (105 lines)
├── crypto.rs                  # SHA-1 + base64 (195 lines)
│
├── protocol/
│   ├── mod.rs                 # Re-exports (5 lines)
│   ├── v2.rs                  # JSON-RPC 2.0 types + methods (65 lines)
│   ├── v1.rs                  # V1 compatibility types (45 lines)
│   └── fsm.rs                 # FallbackFsm + ConnectionFsm + 3-strike (120 lines)
│
├── ws.rs                      # WebSocket connect + handshake (95 lines)
├── http.rs                    # HTTP POST fallback client (70 lines)
├── tls.rs                     # rustls configuration (40 lines)
├── dns.rs                     # Custom DNS resolver (165 lines)
├── gzip.rs                    # Fixed-Huffman gzip encoder (450 lines)
│
├── server/
│   ├── mod.rs                 # Server orchestration + tick loop (135 lines)
│   ├── backoff.rs             # Exponential backoff with jitter (50 lines)
│   ├── reconnection.rs        # Reconnection loop with select-style polling (70 lines)
│   ├── task.rs                # Remote exec + ping dispatch (130 lines)
│   ├── ping_icmp.rs           # ICMP echo ping (85 lines)
│   ├── ping_tcp.rs            # TCP connect ping (45 lines)
│   ├── ping_http.rs           # HTTP round-trip ping (60 lines)
│   └── cf_access.rs           # Cloudflare Access headers (55 lines)
│
├── monitor/
│   ├── mod.rs                 # Monitor struct + tick orchestrator (55 lines)
│   ├── cpu/
│   │   ├── mod.rs             # CPU platform dispatch (15 lines)
│   │   ├── linux.rs           # /proc/stat + /proc/cpuinfo (85 lines)
│   │   ├── windows.rs         # Registry + performance counters (70 lines)
│   │   ├── macos.rs           # sysctl (50 lines)
│   │   └── freebsd.rs         # sysctl (50 lines)
│   ├── mem/
│   │   ├── mod.rs             # Memory platform dispatch + 3-mode logic (50 lines)
│   │   ├── linux.rs           # /proc/meminfo + free -b (140 lines)
│   │   ├── windows.rs         # GlobalMemoryStatusEx (75 lines)
│   │   ├── macos.rs           # host_statistics64 + sysctl (60 lines)
│   │   └── freebsd.rs         # sysctl + kvm_getswapinfo (60 lines)
│   ├── disk/
│   │   ├── mod.rs             # Disk platform dispatch + physical filter (35 lines)
│   │   ├── linux.rs           # /proc/mounts + statvfs (80 lines)
│   │   ├── windows.rs         # GetLogicalDrives + GetDiskFreeSpaceExW (60 lines)
│   │   ├── macos.rs           # getmntinfo + statfs (40 lines)
│   │   └── freebsd.rs         # getmntinfo + statfs (40 lines)
│   ├── net/
│   │   ├── mod.rs             # Network platform dispatch + delta calc (40 lines)
│   │   ├── linux.rs           # /proc/net/dev + /proc/net/tcp (90 lines)
│   │   ├── windows.rs         # GetIfTable2 + GetTcpTable2 (70 lines)
│   │   ├── macos.rs           # sysctl net + routing socket (55 lines)
│   │   └── freebsd.rs         # sysctl net + kvm (55 lines)
│   ├── load/
│   │   ├── mod.rs             # Load platform dispatch (10 lines)
│   │   ├── linux.rs           # /proc/loadavg (35 lines)
│   │   ├── windows.rs         # Performance counter (25 lines)
│   │   ├── macos.rs           # getloadavg() (15 lines)
│   │   └── freebsd.rs         # getloadavg() (15 lines)
│   ├── connections/
│   │   ├── mod.rs             # Connection count dispatch (10 lines)
│   │   ├── linux.rs           # /proc/net/tcp + tcp6 (30 lines)
│   │   ├── windows.rs         # GetTcpTable2 + GetTcp6Table2 (30 lines)
│   │   ├── macos.rs           # sysctl net.inet.tcp (25 lines)
│   │   └── freebsd.rs         # sysctl net.inet.tcp (25 lines)
│   ├── process/
│   │   ├── mod.rs             # Process count dispatch (10 lines)
│   │   ├── linux.rs           # /proc PID counting (40 lines)
│   │   ├── windows.rs         # K32EnumProcesses (45 lines)
│   │   ├── macos.rs           # sysctl kern.proc.all (35 lines)
│   │   └── freebsd.rs         # sysctl kern.proc.all (35 lines)
│   ├── uptime/
│   │   ├── mod.rs             # Uptime platform dispatch (10 lines)
│   │   ├── linux.rs           # /proc/uptime (25 lines)
│   │   ├── windows.rs         # GetTickCount64 (20 lines)
│   │   ├── macos.rs           # sysctl kern.boottime (20 lines)
│   │   └── freebsd.rs         # sysctl kern.boottime (20 lines)
│   ├── ip/
│   │   ├── mod.rs             # IP detection dispatch (15 lines)
│   │   ├── linux.rs           # netlink + fallback HTTP (55 lines)
│   │   ├── windows.rs         # GetAdaptersAddresses (55 lines)
│   │   ├── macos.rs           # getifaddrs (40 lines)
│   │   └── freebsd.rs         # getifaddrs (40 lines)
│   ├── gpu/
│   │   ├── mod.rs             # GpuDetector dispatch + types (55 lines)
│   │   ├── linux.rs           # nvidia-smi CSV + rocm-smi + DRM fallback (135 lines)
│   │   ├── windows.rs         # DXGI via windows-rs (115 lines)
│   │   ├── macos.rs           # system_profiler JSON (55 lines)
│   │   └── freebsd.rs         # pciconf -lv (65 lines)
│   ├── os.rs                  # OS name + kernel version (115 lines)
│   ├── virtualization.rs      # VM/container detection (85 lines)
│   └── netstatic.rs           # Persistent traffic history (85 lines)
│
├── platform/
│   ├── mod.rs                 # cfg-gated type aliases (15 lines)
│   ├── linux.rs               # Linux platform module gate (5 lines)
│   ├── windows.rs             # Windows platform module gate (25 lines)
│   ├── macos.rs               # macOS platform module gate (20 lines)
│   └── freebsd.rs             # FreeBSD platform module gate (20 lines)
│
├── terminal/
│   ├── mod.rs                 # Terminal trait + start_terminal (70 lines)
│   ├── unix.rs                # POSIX PTY via posix_openpt (140 lines)
│   └── windows.rs             # ConPTY via CreatePseudoConsole (155 lines)
│
├── task.rs                    # Task types (exec, ping) (40 lines)
├── update.rs                  # Self-update via GitHub releases (85 lines)
└── autodiscovery.rs           # Auto-registration (60 lines)
```

### 2.2 Per-File Responsibility and Estimated Lines

#### Core (src/ root)

| File | Lines | Responsibility |
|---|---|---|
| `main.rs` | 15 | Call `app::run()`, exit with return code. |
| `app.rs` | 80 | CLI dispatch: parse config, optionally run auto-discovery, start server loop or one-shot subcommand. |
| `config.rs` | 105 | `Config` struct with clap derive. 34 fields matching Go `Config`. Env override via `#[arg(env)]`. File config via `--config-file`. |
| `arena.rs` | 140 | `ScratchArena`: bump allocator backed by `[u8; 8192]`. `SmallVec<T, N>`: inline storage with heap fallback. |
| `json.rs` | 105 | `JsonBuf` (stack buffer + cursor), `Field` enum (Str, I64, U64, F64, Bool, Null, Nested), `EncodeJson` trait. |
| `crypto.rs` | 195 | SHA-1 (80-round RFC 3174), base64 encode-only (RFC 4648). Zero-alloc stack buffers. |

#### Protocol

| File | Lines | Responsibility |
|---|---|---|
| `protocol/mod.rs` | 5 | Re-export v2 types, v1 types, FSM. |
| `protocol/v2.rs` | 65 | `Request`, `Response`, `RPCError`, `Event`, `EventResult` structs. 11 method constants. `EncodeJson` impls. Build helpers. |
| `protocol/v1.rs` | 45 | V1 `ReportPayload` type alias. V1 HTTP POST format. |
| `protocol/fsm.rs` | 120 | `FallbackFsm` enum (6 states), `ConnectionFsm` enum (4 states). 3-strike counter. State transition match arms. |

#### Transport

| File | Lines | Responsibility |
|---|---|---|
| `ws.rs` | 95 | TCP connect → TLS wrap → HTTP upgrade → verify 101 + `Sec-WebSocket-Accept` → return `rustls::StreamOwned`. WebSocket frame send/receive (minimal: text frames only for agent→server, parse text frames for server→agent). |
| `http.rs` | 70 | Manual HTTP/1.1 POST with `Content-Type: application/json` + optional `Content-Encoding: gzip`. TLS via rustls. No reqwest. |
| `tls.rs` | 40 | rustls `ClientConfig` builder: `webpki-roots`, optional `InsecureSkipVerify` (config flag), no client cert. |
| `dns.rs` | 165 | Custom DNS resolver: UDP query build (A + AAAA), parse response, cache with TTL (max 5 min). 10 built-in DNS servers. IPv4/IPv6 preference. `DialContext` factory for TCP + WebSocket. |
| `gzip.rs` | 450 | Fixed-Huffman DEFLATE encoder. CRC32 table-driven. `BitWriter`. LZ77 hash-chain matcher. Gzip container wrapper. `gzip_bytes()` / `gzip_bytes_adaptive()` public API. |

#### Server

| File | Lines | Responsibility |
|---|---|---|
| `server/mod.rs` | 135 | `run()`: connect → tick loop { monitor.tick() → encode → send WS frame → sleep 1s }. Periodic basicInfo upload (configurable interval). Graceful shutdown on SIGINT/SIGTERM. |
| `server/backoff.rs` | 50 | `Backoff { initial, max, current, attempts }`. `next_delay()` with +/- 25% jitter, caps at 5 min. `reset()`. |
| `server/reconnection.rs` | 70 | Reconnection loop: select-style poll over dataTicker (1s), heartbeatTicker (30s PingMessage), readDone (WS close). On disconnect: backoff.sleep(), reconnect. |
| `server/task.rs` | 130 | `handle_exec(task_id, command)` → PowerShell (Windows) / sh -s (Unix). `handle_ping(task_id, ping_type, target)` → dispatch to ICMP/TCP/HTTP. Upload results via WS or HTTP POST. |
| `server/ping_icmp.rs` | 85 | ICMP echo via raw socket (Linux: `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)`, needs CAP_NET_RAW). Windows: `IcmpSendEcho2` via iphlpapi. Build ICMP header, compute checksum, measure RTT. |
| `server/ping_tcp.rs` | 45 | TCP connect to `host:port` with timeout. Measure connect time as RTT. |
| `server/ping_http.rs` | 60 | HTTP GET/HEAD to URL with timeout. Check 2xx/3xx. Measure round-trip time. |
| `server/cf_access.rs` | 55 | Add `CF-Access-Client-Id` + `CF-Access-Client-Secret` headers to WS upgrade and HTTP POST when configured. |

#### Monitor (metrics collectors)

| File | Lines | Responsibility |
|---|---|---|
| `monitor/mod.rs` | 55 | `Monitor` struct holding arena + SmallVec buffers + previous sample state. `tick(&mut self, config: &Config) -> JsonBuf`. Collectors called in fixed order. Arena reset after each tick. |
| `monitor/cpu/mod.rs` | 15 | Platform dispatch: `fn collect(arena, prev) -> CpuInfo`. |
| `monitor/cpu/linux.rs` | 85 | Parse `/proc/stat` (total + per-core). Parse `/proc/cpuinfo` (model name, cores, MHz). Delta from previous sample for usage %. |
| `monitor/cpu/windows.rs` | 70 | `GetSystemInfo` for core count. Registry for CPU name. `QueryPerformanceCounter` for usage. |
| `monitor/cpu/macos.rs` | 50 | `sysctl hw.logicalcpu` / `machdep.cpu.brand_string`. `host_processor_info` for usage. |
| `monitor/cpu/freebsd.rs` | 50 | `sysctl hw.model` / `hw.ncpu`. `kern.cp_times` for usage. |
| `monitor/mem/mod.rs` | 50 | 3-mode dispatch matching Go `Ram()` exactly. Mode selection from config flags. |
| `monitor/mem/linux.rs` | 140 | Parse `/proc/meminfo`. 3 modes: mode 0 (used = total - available), mode 1 (used = total - free - buffers - cached), mode 2 (htop-like). Swap via `/proc/meminfo` SwapTotal/SwapFree/SwapCached. `free -b` fallback. |
| `monitor/mem/windows.rs` | 75 | `GlobalMemoryStatusEx` for total/avail. `GetPerformanceInfo` for detailed breakdown. |
| `monitor/mem/macos.rs` | 60 | `sysctl hw.memsize`. `host_statistics64` for VM page counts. `sysctl vm.swapusage`. |
| `monitor/mem/freebsd.rs` | 60 | `sysctl hw.physmem` / `hw.usermem`. `kvm_getswapinfo`. |
| `monitor/disk/mod.rs` | 35 | Physical disk filter: `is_physical_disk()` with 30+ exclude patterns (matching Go). |
| `monitor/disk/linux.rs` | 80 | Parse `/proc/mounts`. Filter physical. `statvfs` per mount. |
| `monitor/disk/windows.rs` | 60 | `GetLogicalDrives` + `GetDiskFreeSpaceExW`. Filter `DRIVE_FIXED`. |
| `monitor/disk/macos.rs` | 40 | `getmntinfo` + `statfs`. Exclude devfs, autofs, etc. |
| `monitor/disk/freebsd.rs` | 40 | `getmntinfo` + `statfs`. |
| `monitor/net/mod.rs` | 40 | Delta calculation from previous sample. NIC include/exclude filter (wildcard). |
| `monitor/net/linux.rs` | 90 | Parse `/proc/net/dev` for RX/TX bytes/packets per interface. Parse `/proc/net/tcp` + `/proc/net/tcp6` for connection count. |
| `monitor/net/windows.rs` | 70 | `GetIfTable2` (iphlpapi) for interface stats. `GetTcpTable2`/`GetTcp6Table2` for connections. |
| `monitor/net/macos.rs` | 55 | `sysctl net` MIB walk. Routing socket for interface stats. |
| `monitor/net/freebsd.rs` | 55 | `sysctl net` + kvm for interface stats. |
| `monitor/load/mod.rs` | 10 | Platform dispatch. |
| `monitor/load/linux.rs` | 35 | Parse `/proc/loadavg` (3 floats). |
| `monitor/load/windows.rs` | 25 | Performance counter. |
| `monitor/load/macos.rs` | 15 | `getloadavg()`. |
| `monitor/load/freebsd.rs` | 15 | `getloadavg()`. |
| `monitor/connections/mod.rs` | 10 | Platform dispatch. |
| `monitor/connections/linux.rs` | 30 | Count entries in `/proc/net/tcp` + `/proc/net/tcp6`. |
| `monitor/connections/windows.rs` | 30 | `GetTcpTable2` / `GetTcp6Table2` row count. |
| `monitor/connections/macos.rs` | 25 | `sysctl net.inet.tcp` state counts. |
| `monitor/connections/freebsd.rs` | 25 | `sysctl net.inet.tcp` state counts. |
| `monitor/process/mod.rs` | 10 | Platform dispatch. |
| `monitor/process/linux.rs` | 40 | Count `/proc/<pid>/` directories where PID is numeric. Count running processes via `/proc/<pid>/status` State=R. |
| `monitor/process/windows.rs` | 45 | `K32EnumProcesses` (PSAPI) for count. |
| `monitor/process/macos.rs` | 35 | `sysctl kern.proc.all` count. |
| `monitor/process/freebsd.rs` | 35 | `sysctl kern.proc.all` count. |
| `monitor/uptime/mod.rs` | 10 | Platform dispatch. |
| `monitor/uptime/linux.rs` | 25 | Parse `/proc/uptime` first field. |
| `monitor/uptime/windows.rs` | 20 | `GetTickCount64`. |
| `monitor/uptime/macos.rs` | 20 | `sysctl kern.boottime`. |
| `monitor/uptime/freebsd.rs` | 20 | `sysctl kern.boottime`. |
| `monitor/ip/mod.rs` | 15 | Platform dispatch. IPv4/IPv6 detection. Custom IP override from config. |
| `monitor/ip/linux.rs` | 55 | Netlink RTM_GETADDR or getifaddrs. HTTP fallback to ipify.org-style service. |
| `monitor/ip/windows.rs` | 55 | `GetAdaptersAddresses` for unicast addresses. |
| `monitor/ip/macos.rs` | 40 | `getifaddrs`. |
| `monitor/ip/freebsd.rs` | 40 | `getifaddrs`. |
| `monitor/gpu/mod.rs` | 55 | `GpuInfo` struct: name, memory_total, memory_used, utilization, temperature. `GpuDetector` type alias per platform. |
| `monitor/gpu/linux.rs` | 135 | **NVIDIA**: exec `nvidia-smi --query-gpu=... --format=csv,noheader`. **AMD**: exec `rocm-smi` with key scanner. **Fallback**: `/sys/class/drm/card*/device/vendor` check. |
| `monitor/gpu/windows.rs` | 115 | DXGI via `windows-rs` crate. Enumerate adapters, get desc for name + VRAM. `GetPerformanceData` WMI fallback. |
| `monitor/gpu/macos.rs` | 55 | `system_profiler SPDisplaysDataType -json` (10.15+). Fallback: plain text parse. |
| `monitor/gpu/freebsd.rs` | 65 | `pciconf -lv | grep VGA`. Limited VRAM info. |
| `monitor/os.rs` | 115 | **Linux**: `/etc/os-release` parse + heuristics (Android, Synology, PVE, fnOS). **Windows**: Registry `CurrentVersion`. **macOS**: `sw_vers`. **FreeBSD**: `uname -r` + `freebsd-version`. |
| `monitor/virtualization.rs` | 85 | **Container**: `/proc/self/cgroup`, `/.dockerenv`. **VM**: CPUID hypervisor bit, DMI `/sys/class/dmi/id/product_name`. **Windows**: CPUID `__cpuid`. **macOS**: `sysctl kern.hv_support`. |
| `monitor/netstatic.rs` | 85 | `TrafficData` entries persisted to `net_static.json`. `VecDeque` with max 720 entries (12h at 1/min). Atomic write via temp+rename. |

#### Platform

| File | Lines | Responsibility |
|---|---|---|
| `platform/mod.rs` | 15 | cfg-gated type aliases: `type CurrentCpu`, `type CurrentMem`, etc. |
| `platform/linux.rs` | 5 | `#[cfg(target_os = "linux")]` gate. |
| `platform/windows.rs` | 25 | `#[cfg(target_os = "windows")]` gate. Win32 API FFI declarations. |
| `platform/macos.rs` | 20 | `#[cfg(target_os = "macos")]` gate. |
| `platform/freebsd.rs` | 20 | `#[cfg(target_os = "freebsd")]` gate. |

#### Terminal

| File | Lines | Responsibility |
|---|---|---|
| `terminal/mod.rs` | 70 | `Terminal` trait: `close()`, `read()`, `write()`, `resize()`, `wait()`. `start_terminal(cmd, cols, rows)`. |
| `terminal/unix.rs` | 140 | `posix_openpt()` + `grantpt()` + `unlockpt()` + `fork()` + `execvp()`. Signal handling: SIGCHLD. Resize: `ioctl(TIOCSWINSZ)`. |
| `terminal/windows.rs` | 155 | `CreatePseudoConsole()` (Windows 10 1809+). `STARTUPINFOEX` with `PPROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`. Overlapped pipe I/O. `ResizePseudoConsole()`. |

#### Utility

| File | Lines | Responsibility |
|---|---|---|
| `task.rs` | 40 | `ExecTask`, `PingTask` structs. Task result upload types. |
| `update.rs` | 85 | Check GitHub releases API for latest version. Download asset for current platform. Verify SHA256. Replace binary: Unix `rename()` (atomic), Windows `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`. Exit code 42. |
| `autodiscovery.rs` | 60 | POST `/api/agent/discover` with host info. Persist to `auto-discovery.json`. Retry every 60s until registered. |

### 2.3 Dependency Graph

```
main.rs
  └── app.rs
        ├── config.rs          (Config struct)
        ├── autodiscovery.rs   (auto-registration, uses config)
        └── server/mod.rs      (main loop)
              ├── ws.rs         (WebSocket connect)
              │     ├── tls.rs  (rustls config)
              │     ├── dns.rs  (DNS resolve + dial)
              │     └── crypto.rs (SHA-1 + base64 for WS handshake)
              ├── http.rs       (HTTP POST fallback)
              │     ├── tls.rs
              │     ├── dns.rs
              │     └── gzip.rs (compress POST body)
              ├── server/backoff.rs
              ├── server/reconnection.rs
              │     ├── server/backoff.rs
              │     └── protocol/fsm.rs
              ├── server/task.rs
              │     ├── server/ping_icmp.rs
              │     ├── server/ping_tcp.rs
              │     ├── server/ping_http.rs
              │     └── task.rs
              ├── server/cf_access.rs
              ├── monitor/mod.rs
              │     ├── arena.rs
              │     ├── json.rs
              │     ├── monitor/cpu/mod.rs → cpu/{linux,windows,macos,freebsd}.rs
              │     ├── monitor/mem/mod.rs → mem/{linux,windows,macos,freebsd}.rs
              │     ├── monitor/disk/mod.rs → disk/{linux,windows,macos,freebsd}.rs
              │     ├── monitor/net/mod.rs → net/{linux,windows,macos,freebsd}.rs
              │     ├── monitor/load/mod.rs → load/{linux,windows,macos,freebsd}.rs
              │     ├── monitor/connections/mod.rs → connections/{linux,...}.rs
              │     ├── monitor/process/mod.rs → process/{linux,...}.rs
              │     ├── monitor/uptime/mod.rs → uptime/{linux,...}.rs
              │     ├── monitor/ip/mod.rs → ip/{linux,...}.rs
              │     ├── monitor/gpu/mod.rs → gpu/{linux,...}.rs
              │     ├── monitor/os.rs
              │     ├── monitor/virtualization.rs
              │     └── monitor/netstatic.rs
              ├── protocol/v2.rs
              │     └── json.rs
              ├── protocol/v1.rs
              │     └── json.rs
              └── protocol/fsm.rs
                    ├── protocol/v2.rs
                    └── protocol/v1.rs

terminal/mod.rs → terminal/{unix,windows}.rs  (used by server/task.rs for agent.terminal.request)
update.rs → dns.rs  (resolve github.com for release download)
```

---

## 3. DATA FLOW DIAGRAM

### 3.1 Metrics Collection → JSON Encoding → WS/HTTP Sink → Server

```
┌─────────────────────────────────────────────────────────────────────┐
│                        1-SECOND TICK LOOP                            │
│                                                                      │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │                     monitor::tick()                            │   │
│  │                                                                │   │
│  │  arena.reset()  ←── bump pointer back to 0                    │   │
│  │                                                                │   │
│  │  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐          │   │
│  │  │ cpu     │  │ mem     │  │ disk    │  │ net     │  ...     │   │
│  │  │ /proc/  │  │ /proc/  │  │ /proc/  │  │ /proc/  │          │   │
│  │  │ stat    │  │ meminfo │  │ mounts  │  │ net/dev │          │   │
│  │  └────┬────┘  └────┬────┘  └────┬────┘  └────┬────┘          │   │
│  │       │            │            │            │                 │   │
│  │       ▼            ▼            ▼            ▼                 │   │
│  │  CpuInfo      RamInfo      DiskInfo     NetInfo                │   │
│  │  (stack)      (stack)      (stack)      (stack)               │   │
│  │       │            │            │            │                 │   │
│  │       └────────────┴─────┬──────┴────────────┘                 │   │
│  │                          │                                      │   │
│  │                          ▼                                      │   │
│  │               json::JsonBuf (stack [u8; 4096])                  │   │
│  │               ┌──────────────────────────┐                      │   │
│  │               │ {"cpu":{"usage":12.5},   │                      │   │
│  │               │  "ram":{"total":...},    │  ← EncodeJson        │   │
│  │               │  "swap":...,             │    trait impls       │   │
│  │               │  "load":...,             │    push field-by-    │   │
│  │               │  "disk":...,             │    field, no heap    │   │
│  │               │  "network":...,          │    alloc             │   │
│  │               │  "connections":...,      │                      │   │
│  │               │  "uptime":...,           │                      │   │
│  │               │  "process":...,          │                      │   │
│  │               │  "gpu":...}              │                      │   │
│  │               └──────────────────────────┘                      │   │
│  └───────────────────────┬──────────────────────────────────────────┘   │
│                          │                                              │
│                          ▼                                              │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │              protocol::v2::BuildReportPayload()                │   │
│  │                                                                │   │
│  │  Wrap in JSON-RPC envelope:                                    │   │
│  │  {"jsonrpc":"2.0","method":"agent.report",                     │   │
│  │   "params":{"report": <raw json>, "ack_event_ids": [...]}}     │   │
│  └───────────────────────┬──────────────────────────────────────────┘   │
│                          │                                              │
│              ┌───────────┴───────────┐                                  │
│              │                       │                                  │
│     WebSocket path            HTTP POST fallback                        │
│              │                       │                                  │
│              ▼                       ▼                                  │
│  ws.send_text_frame()    gzip_bytes() → POST /api/clients/v2/rpc       │
│  (per-message deflate    Content-Encoding: gzip                         │
│   via tungstenite)       Content-Type: application/json                 │
│              │                       │                                  │
│              └───────────┬───────────┘                                  │
│                          │                                              │
│                          ▼                                              │
│              ┌───────────────────────┐                                  │
│              │   Komari Server       │                                  │
│              │   komari-monitor      │                                  │
│              └───────────────────────┘                                  │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.2 Event Loop Tick Sequence

```
TIME ─────────────────────────────────────────────────────────────────►

T=0    server::run()
       │
       ├── dns::resolve(server_endpoint)
       ├── tls::configure() → rustls::ClientConfig
       ├── ws::connect() → TCP connect → TLS handshake → WS upgrade
       │   └── crypto::sha1(key) → crypto::base64_encode() → verify accept
       │
       ├── connection_fsm = Connected
       ├── upload_basic_info()  [once on connect]
       │
       ▼
┌──────────────────────────────────────────────────────────────────┐
│  MAIN LOOP                                                        │
│                                                                    │
│  loop {                                                            │
│      // 1. Data tick (every interval seconds, default 1s)         │
│      sleep(interval)                                               │
│                                                                    │
│      // 2. Collect metrics (all stack-allocated, arena-backed)    │
│      arena.reset()                                                 │
│      let cpu    = monitor::cpu::collect(&arena);                   │
│      let mem    = monitor::mem::collect(&arena, &config);          │
│      let swap   = monitor::mem::swap(&arena);                      │
│      let disk   = monitor::disk::collect(&arena, &config);         │
│      let load   = monitor::load::collect();                        │
│      let net    = monitor::net::collect(&mut prev_net);            │
│      let conns  = monitor::connections::collect();                 │
│      let proc   = monitor::process::collect();                     │
│      let uptime = monitor::uptime::collect();                      │
│      let gpu    = if config.enable_gpu { gpu::collect() }          │
│                                                                    │
│      // 3. Encode to JSON (into arena-backed JsonBuf)             │
│      let mut buf = JsonBuf::new(&mut arena);                       │
│      encode_report(&mut buf, cpu, mem, swap, disk, load,          │
│                    net, conns, proc, uptime, gpu);                 │
│                                                                    │
│      // 4. Wrap in JSON-RPC envelope                               │
│      let payload = BuildReportPayload(buf.as_bytes());             │
│                                                                    │
│      // 5. Send over WebSocket                                     │
│      if let Err(e) = ws.send_text(payload) {                       │
│          fsm.record_failure(e);                                    │
│          if fsm.should_fallback() {                                 │
│              enter_http_fallback_mode();                            │
│          }                                                          │
│          reconnect();                                               │
│      } else {                                                      │
│          fsm.record_success();                                      │
│      }                                                             │
│                                                                    │
│      // 6. Check for incoming messages (non-blocking, with poll)  │
│      if ws.poll_read_ready(timeout=0) {                            │
│          let msg = ws.read_message();                               │
│          dispatch_event(msg);  // exec, ping, terminal, message    │
│      }                                                             │
│                                                                    │
│      // 7. Heartbeat (every 30s)                                   │
│      if heartbeat_timer.elapsed() > 30s {                          │
│          ws.send_ping();                                            │
│      }                                                             │
│                                                                    │
│      // 8. Periodic basicInfo upload (every info_report_interval)  │
│      if basic_info_timer.elapsed() > info_report_interval {        │
│          upload_basic_info();                                       │
│      }                                                             │
│                                                                    │
│      // 9. Arena reset for next tick                               │
│      arena.reset();                                                │
│  }                                                                 │
└──────────────────────────────────────────────────────────────────┘
```

### 3.3 Protocol Fallback Data Flow

```
                    ┌─────────────┐
                    │  START      │
                    └──────┬──────┘
                           │
                           ▼
                    ┌─────────────┐
              ┌─────│  V2 WS     │◄────────── success resets counter
              │     └──────┬──────┘
              │            │ failure
              │            ▼
              │     ┌─────────────┐
              │     │ V2 WS Fail 1│ (strike 1)
              │     └──────┬──────┘
              │            │ failure
              │            ▼
              │     ┌─────────────┐
              │     │ V2 WS Fail 2│ (strike 2)
              │     └──────┬──────┘
              │            │ failure
              │            ▼
              │     ┌─────────────┐
              │     │ V2 WS Fail 3│ (strike 3 → FALLBACK)
              │     └──────┬──────┘
              │            │
              │            ▼
              │     ┌─────────────┐
              │     │ V2 HTTP POST│ (POST /api/clients/v2/rpc)
              │     └──────┬──────┘
              │            │ failure
              │            ▼
              │     ┌─────────────┐
              │     │ V1 HTTP POST│ (POST /api/clients/report)
              │     └─────────────┘
              │
              └──── on any V2 success, counter resets to 0, return to V2 WS
```

---

## 4. KEY TYPE DEFINITIONS

```rust
// === src/config.rs ===

use clap::Parser;

/// Complete agent configuration.
/// Mirrors Go's `cmd/flags/flag.go Config` struct exactly.
/// All fields have JSON tags and env var overrides.
#[derive(Parser, Debug, Clone)]
#[command(name = "komari-agent", version, about = "Komari monitoring agent")]
pub struct Config {
    /// Auto-discovery registration key
    #[arg(long, env = "AGENT_AUTO_DISCOVERY_KEY", default_value = "")]
    pub auto_discovery_key: String,

    /// Disable automatic updates
    #[arg(long, env = "AGENT_DISABLE_AUTO_UPDATE", default_value_t = false)]
    pub disable_auto_update: bool,

    /// Disable remote control (Web SSH and RCE)
    #[arg(long, env = "AGENT_DISABLE_WEB_SSH", default_value_t = false)]
    pub disable_web_ssh: bool,

    /// Agent authentication token
    #[arg(long, env = "AGENT_TOKEN", default_value = "")]
    pub token: String,

    /// Komari server endpoint (e.g. https://monitor.example.com)
    #[arg(long, env = "AGENT_ENDPOINT", default_value = "")]
    pub endpoint: String,

    /// Data collection interval in seconds
    #[arg(long, env = "AGENT_INTERVAL", default_value_t = 1.0)]
    pub interval: f64,

    /// Ignore unsafe TLS certificates
    #[arg(long, env = "AGENT_IGNORE_UNSAFE_CERT", default_value_t = false)]
    pub ignore_unsafe_cert: bool,

    /// Maximum connection retry attempts
    #[arg(long, env = "AGENT_MAX_RETRIES", default_value_t = 5)]
    pub max_retries: i32,

    /// Reconnection interval in seconds
    #[arg(long, env = "AGENT_RECONNECT_INTERVAL", default_value_t = 10)]
    pub reconnect_interval: i32,

    /// Basic info report interval in minutes
    #[arg(long, env = "AGENT_INFO_REPORT_INTERVAL", default_value_t = 30)]
    pub info_report_interval: i32,

    /// Only monitor these NICs (comma-separated, supports wildcard)
    #[arg(long, env = "AGENT_INCLUDE_NICS", default_value = "")]
    pub include_nics: String,

    /// Exclude these NICs from monitoring (comma-separated, supports wildcard)
    #[arg(long, env = "AGENT_EXCLUDE_NICS", default_value = "")]
    pub exclude_nics: String,

    /// Disk mount points to include (semicolon-separated)
    #[arg(long, env = "AGENT_INCLUDE_MOUNTPOINTS", default_value = "")]
    pub include_mountpoints: String,

    /// Traffic statistics month rotation day (0 = disabled)
    #[arg(long, env = "AGENT_MONTH_ROTATE", default_value_t = 1)]
    pub month_rotate: i32,

    /// Cloudflare Access Client ID
    #[arg(long, env = "AGENT_CF_ACCESS_CLIENT_ID", default_value = "")]
    pub cf_access_client_id: String,

    /// Cloudflare Access Client Secret
    #[arg(long, env = "AGENT_CF_ACCESS_CLIENT_SECRET", default_value = "")]
    pub cf_access_client_secret: String,

    /// Include cache/buffers in memory usage
    #[arg(long, env = "AGENT_MEMORY_INCLUDE_CACHE", default_value_t = false)]
    pub memory_include_cache: bool,

    /// Report raw used memory (htop-like)
    #[arg(long, env = "AGENT_MEMORY_REPORT_RAW_USED", default_value_t = false)]
    pub memory_report_raw_used: bool,

    /// Custom DNS server address
    #[arg(long, env = "AGENT_CUSTOM_DNS", default_value = "")]
    pub custom_dns: String,

    /// Enable detailed GPU monitoring
    #[arg(long, env = "AGENT_ENABLE_GPU", default_value_t = false)]
    pub enable_gpu: bool,

    /// Show security warning on Windows (run as subprocess)
    #[arg(long, env = "AGENT_SHOW_WARNING", default_value_t = false)]
    pub show_warning: bool,

    /// Custom IPv4 address override
    #[arg(long, env = "AGENT_CUSTOM_IPV4", default_value = "")]
    pub custom_ipv4: String,

    /// Custom IPv6 address override
    #[arg(long, env = "AGENT_CUSTOM_IPV6", default_value = "")]
    pub custom_ipv6: String,

    /// Get IP address from network interface
    #[arg(long, env = "AGENT_GET_IP_ADDR_FROM_NIC", default_value_t = false)]
    pub get_ip_addr_from_nic: bool,

    /// Host /proc mount point (container environments)
    #[arg(long, env = "HOST_PROC", default_value = "/proc")]
    pub host_proc: String,

    /// JSON configuration file path
    #[arg(long, env = "AGENT_CONFIG_FILE", default_value = "")]
    pub config_file: String,

    /// Protocol version (1 or 2, default 2)
    #[arg(long, env = "AGENT_PROTOCOL_VERSION", default_value_t = 2)]
    pub protocol_version: i32,

    /// Disable v2 transport compression
    #[arg(long, env = "AGENT_DISABLE_COMPRESSION", default_value_t = false)]
    pub disable_compression: bool,

    /// Preferred IP version for server connection ("4" or "6")
    #[arg(long, env = "AGENT_PREFER_IP_VERSION", default_value = "")]
    pub prefer_ip_version: String,
}

impl Config {
    /// Derive memory calculation mode from config flags.
    /// Mode 0: used = total - available (gopsutil-style, default)
    /// Mode 1: used = total - free (include cache)
    /// Mode 2: htop-like (raw used, subtract cached/buffered/SReclaimable)
    pub fn mem_mode(&self) -> u8 {
        if self.memory_report_raw_used {
            2
        } else if self.memory_include_cache {
            1
        } else {
            0
        }
    }
}
```

```rust
// === src/json.rs ===

/// Stack-allocated JSON buffer.
/// Hot-path writes push bytes directly; no heap allocation.
/// Capacity 4096 covers typical monitoring reports (2-3 KB JSON).
/// For oversized payloads (rare), falls back to Vec<u8>.
pub struct JsonBuf<'a> {
    arena: &'a mut ScratchArena,
    cursor: usize,
    // Overflow: if arena exhausted, spill to heap Vec
    overflow: Option<Vec<u8>>,
}

impl<'a> JsonBuf<'a> {
    pub fn new(arena: &'a mut ScratchArena) -> Self {
        Self { arena, cursor: 0, overflow: None }
    }

    pub fn as_bytes(&self) -> &[u8] {
        if let Some(ref v) = self.overflow {
            v.as_slice()
        } else {
            &self.arena.as_bytes()[..self.cursor]
        }
    }

    pub fn push_byte(&mut self, b: u8) {
        if let Some(ref mut v) = self.overflow {
            v.push(b);
        } else if let Some(slot) = self.arena.alloc::<u8>(1) {
            self.cursor = self.arena.offset();
            // already written via alloc
        } else {
            // Arena exhausted — spill to heap (should be rare)
            let mut v = Vec::with_capacity(8192);
            v.extend_from_slice(&self.arena.as_bytes()[..self.cursor]);
            v.push(b);
            self.overflow = Some(v);
        }
    }

    pub fn push_str(&mut self, s: &str) {
        for b in s.bytes() { self.push_byte(b); }
    }

    pub fn push_u64(&mut self, n: u64) {
        // itoa-style: write digits to stack buffer, push reversed
        let mut buf: [u8; 20] = [0; 20];
        let mut i = 20;
        let mut v = n;
        if v == 0 {
            buf[19] = b'0';
            i = 19;
        } else {
            while v > 0 {
                i -= 1;
                buf[i] = (v % 10) as u8 + b'0';
                v /= 10;
            }
        }
        for b in &buf[i..] { self.push_byte(*b); }
    }

    pub fn push_i64(&mut self, n: i64) {
        if n < 0 {
            self.push_byte(b'-');
            self.push_u64((-n) as u64);
        } else {
            self.push_u64(n as u64);
        }
    }

    pub fn push_f64_prec2(&mut self, n: f64) {
        // Format with 2 decimal places, sufficient for monitoring
        // Integer part + '.' + 2 fractional digits
        if n.is_nan() { self.push_str("null"); return; }
        if n < 0.0 { self.push_byte(b'-'); }
        let abs = n.abs();
        let int_part = abs as u64;
        let frac = ((abs - int_part as f64) * 100.0).round() as u64;
        self.push_u64(int_part);
        self.push_byte(b'.');
        if frac < 10 { self.push_byte(b'0'); }
        self.push_u64(frac);
    }
}

/// JSON value types for field encoding.
/// Pure enum — no heap data (strings are borrowed slices from arena).
#[derive(Debug, Clone, Copy)]
pub enum Field<'a> {
    Str(&'a str),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Null,
    Array(&'a [Field<'a>]),
    Object(&'a [(&'a str, Field<'a>)]),
}

impl<'a> Field<'a> {
    pub fn encode_into(&self, buf: &mut JsonBuf) {
        match self {
            Field::Str(s) => {
                buf.push_byte(b'"');
                for &b in s.as_bytes() {
                    match b {
                        b'"'  => buf.push_str("\\\""),
                        b'\\' => buf.push_str("\\\\"),
                        b'\n' => buf.push_str("\\n"),
                        b'\r' => buf.push_str("\\r"),
                        b'\t' => buf.push_str("\\t"),
                        0x00..=0x1F => {
                            buf.push_str("\\u00");
                            let hi = b >> 4;
                            let lo = b & 0x0F;
                            buf.push_byte(if hi < 10 { b'0' + hi } else { b'a' + hi - 10 });
                            buf.push_byte(if lo < 10 { b'0' + lo } else { b'a' + lo - 10 });
                        }
                        _ => buf.push_byte(b),
                    }
                }
                buf.push_byte(b'"');
            }
            Field::I64(n) => buf.push_i64(*n),
            Field::U64(n) => buf.push_u64(*n),
            Field::F64(n) => buf.push_f64_prec2(*n),
            Field::Bool(true) => buf.push_str("true"),
            Field::Bool(false) => buf.push_str("false"),
            Field::Null => buf.push_str("null"),
            Field::Array(arr) => {
                buf.push_byte(b'[');
                for (i, item) in arr.iter().enumerate() {
                    if i > 0 { buf.push_byte(b','); }
                    item.encode_into(buf);
                }
                buf.push_byte(b']');
            }
            Field::Object(pairs) => {
                buf.push_byte(b'{');
                for (i, (key, val)) in pairs.iter().enumerate() {
                    if i > 0 { buf.push_byte(b','); }
                    Field::Str(key).encode_into(buf);
                    buf.push_byte(b':');
                    val.encode_into(buf);
                }
                buf.push_byte(b'}');
            }
        }
    }
}

/// Trait for types that can encode themselves into a JsonBuf.
/// Implemented by all monitoring types and protocol types.
pub trait EncodeJson {
    fn encode_json(&self, buf: &mut JsonBuf);
}

/// Convenience: encode any EncodeJson type into a Vec<u8>.
/// Used only for non-hot-path operations (basicInfo, task results, ping results).
pub fn encode_to_vec<T: EncodeJson>(value: &T) -> Vec<u8> {
    let mut arena = ScratchArena::new();
    let mut buf = JsonBuf::new(&mut arena);
    value.encode_json(&mut buf);
    buf.as_bytes().to_vec()
}
```

```rust
// === src/arena.rs ===

use std::alloc::Layout;
use std::cell::UnsafeCell;
use std::ptr::NonNull;

/// Bump allocator backed by a fixed stack buffer.
/// Reset after each monitoring tick. No deallocation, no fragmentation.
pub struct ScratchArena {
    buf: UnsafeCell<[u8; Self::CAPACITY]>,
    offset: UnsafeCell<usize>,
}

unsafe impl Sync for ScratchArena {} // single-threaded use

impl ScratchArena {
    pub const CAPACITY: usize = 8192; // 8 KB

    pub fn new() -> Self {
        Self {
            buf: UnsafeCell::new([0u8; Self::CAPACITY]),
            offset: UnsafeCell::new(0),
        }
    }

    /// Allocate a slice of `count` elements of type T.
    /// Returns None if arena is exhausted.
    #[inline]
    pub fn alloc<T: Copy>(&mut self, count: usize) -> Option<&mut [u8]> {
        let layout = Layout::array::<T>(count).unwrap();
        let align = layout.align();
        let size = layout.size();

        let offset = unsafe { &mut *self.offset.get() };
        let aligned = (*offset + align - 1) & !(align - 1);

        if aligned + size > Self::CAPACITY {
            return None;
        }

        *offset = aligned + size;
        let buf = unsafe { &mut *self.buf.get() };
        Some(&mut buf[aligned..aligned + size])
    }

    /// Allocate and return a mutable reference to a single T.
    #[inline]
    pub fn alloc_one<T: Copy>(&mut self) -> Option<&mut T> {
        let bytes = self.alloc::<T>(1)?;
        unsafe { Some(&mut *(bytes.as_mut_ptr() as *mut T)) }
    }

    /// Reset the bump pointer to the beginning.
    #[inline]
    pub fn reset(&mut self) {
        unsafe { *self.offset.get() = 0; }
    }

    pub fn offset(&self) -> usize {
        unsafe { *self.offset.get() }
    }

    pub fn as_bytes(&self) -> &[u8] {
        let offset = unsafe { *self.offset.get() };
        let buf = unsafe { &*self.buf.get() };
        &buf[..offset]
    }
}

/// Small vector with inline storage.
/// Uses stack storage for up to N elements; falls back to Vec only if exceeded.
/// In practice, never exceeds N in the monitoring hot path.
pub struct SmallVec<T, const N: usize> {
    inline: [T; N],
    len: usize,
    heap: Option<Vec<T>>,
}

impl<T: Copy + Default, const N: usize> SmallVec<T, N> {
    pub fn new() -> Self {
        Self {
            inline: [T::default(); N],
            len: 0,
            heap: None,
        }
    }

    pub fn push(&mut self, value: T) {
        if let Some(ref mut v) = self.heap {
            v.push(value);
        } else if self.len < N {
            self.inline[self.len] = value;
            self.len += 1;
        } else {
            let mut v = Vec::with_capacity(N * 2);
            v.extend_from_slice(&self.inline[..self.len]);
            v.push(value);
            self.heap = Some(v);
        }
    }

    pub fn as_slice(&self) -> &[T] {
        if let Some(ref v) = self.heap {
            v.as_slice()
        } else {
            &self.inline[..self.len]
        }
    }

    pub fn len(&self) -> usize {
        if let Some(ref v) = self.heap {
            v.len()
        } else {
            self.len
        }
    }
}
```

```rust
// === src/monitor/mod.rs ===

use crate::arena::ScratchArena;
use crate::json::JsonBuf;
use crate::config::Config;
use crate::monitor::*;

/// Aggregated monitoring state for one tick cycle.
pub struct Monitor {
    arena: ScratchArena,
    /// Previous network sample for delta calculation (total bytes).
    prev_net_rx: u64,
    prev_net_tx: u64,
    /// Total traffic counters (reset on month rotation).
    total_up: u64,
    total_down: u64,
    /// Uptime at first tick (for relative calculations).
    boot_time_secs: u64,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            arena: ScratchArena::new(),
            prev_net_rx: 0,
            prev_net_tx: 0,
            total_up: 0,
            total_down: 0,
            boot_time_secs: 0,
        }
    }

    /// Execute one monitoring tick.
    /// All collectors read system state into stack-allocated structs,
    /// then encode into the arena-backed JsonBuf.
    /// Returns the raw JSON bytes (borrowed from arena — valid until next tick).
    pub fn tick(&mut self, config: &Config) -> &[u8] {
        self.arena.reset();
        let mut buf = JsonBuf::new(&mut self.arena);

        // CPU
        let cpu = cpu::collect(config);
        // Memory (3-mode dispatch)
        let ram = mem::collect_ram(config);
        let swap = mem::collect_swap();
        // Load
        let load = load::collect();
        // Disk
        let disk = disk::collect(config);
        // Network (with delta)
        let (total_up, total_down, net_up, net_down, net_rx, net_tx) =
            net::collect(&mut self.prev_net_rx, &mut self.prev_net_tx,
                         &mut self.total_up, &mut self.total_down,
                         config);
        // Connections
        let (tcp_count, udp_count) = connections::collect();
        // Uptime
        let uptime = uptime::collect();
        // Process count
        let process_count = process::collect();
        // GPU (if enabled)
        let gpu = if config.enable_gpu { Some(gpu::collect()) } else { None };

        // Encode into JsonBuf
        buf.push_str(r#"{"cpu":{"usage":"#);
        buf.push_f64_prec2(cpu.usage);
        buf.push_str(r#"},"ram":{"total":"#);
        buf.push_u64(ram.total);
        buf.push_str(r#","used":"#);
        buf.push_u64(ram.used);
        buf.push_str(r#"},"swap":{"total":"#);
        buf.push_u64(swap.total);
        buf.push_str(r#","used":"#);
        buf.push_u64(swap.used);
        buf.push_str(r#"},"load":{"load1":"#);
        buf.push_f64_prec2(load.load1);
        buf.push_str(r#","load5":"#);
        buf.push_f64_prec2(load.load5);
        buf.push_str(r#","load15":"#);
        buf.push_f64_prec2(load.load15);
        buf.push_str(r#"},"disk":{"total":"#);
        buf.push_u64(disk.total);
        buf.push_str(r#","used":"#);
        buf.push_u64(disk.used);
        buf.push_str(r#"},"network":{"up":"#);
        buf.push_f64_prec2(net_up);
        buf.push_str(r#","down":"#);
        buf.push_f64_prec2(net_down);
        buf.push_str(r#","totalUp":"#);
        buf.push_u64(total_up);
        buf.push_str(r#","totalDown":"#);
        buf.push_u64(total_down);
        buf.push_str(r#"},"connections":{"tcp":"#);
        buf.push_u64(tcp_count);
        buf.push_str(r#","udp":"#);
        buf.push_u64(udp_count);
        buf.push_str(r#"},"uptime":"#);
        buf.push_u64(uptime);
        buf.push_str(r#","process":"#);
        buf.push_u64(process_count);

        if let Some(ref gpu) = gpu {
            buf.push_str(r#","gpu":"#);
            gpu.encode_json(&mut buf);
        }

        buf.push_str(r#","message":""}"#);

        buf.as_bytes()
    }
}

/// Metrics types (stack-allocated, Copy where possible)
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuInfo {
    pub usage: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RamInfo {
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadInfo {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiskInfo {
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub utilization: f64,
    pub temperature: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficData {
    pub timestamp: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}
```

```rust
// === src/protocol/v2.rs ===

use crate::json::JsonBuf;
use crate::json::EncodeJson;

/// JSON-RPC 2.0 version constant.
pub const VERSION: &str = "2.0";

// Method name constants
pub const METHOD_REPORT: &str       = "agent.report";
pub const METHOD_BASIC_INFO: &str   = "agent.basicInfo";
pub const METHOD_PING_RESULT: &str  = "agent.pingResult";
pub const METHOD_TASK_RESULT: &str  = "agent.taskResult";
pub const METHOD_EXEC: &str         = "agent.exec";
pub const METHOD_PING: &str         = "agent.ping";
pub const METHOD_MESSAGE: &str      = "agent.message";
pub const METHOD_EVENT: &str        = "agent.event";
pub const METHOD_TERMINAL: &str     = "agent.terminal.request";
pub const METHOD_PULL: &str         = "agent.pull";

#[derive(Debug, Clone)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub id: Option<String>,
    // params is encoded directly as raw JSON bytes
    pub params_json: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub method: String,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EventResult {
    pub status: Option<String>,
    pub events: Vec<Event>,
}

/// Build a v2 notification (no id field).
pub fn build_notification(method: &'static str, params_json: &[u8]) -> Vec<u8> {
    // {"jsonrpc":"2.0","method":"<method>","params":<params_json>}
    let mut out = Vec::with_capacity(64 + params_json.len());
    out.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"");
    out.extend_from_slice(method.as_bytes());
    out.extend_from_slice(b"\",\"params\":");
    out.extend_from_slice(params_json);
    out.push(b'}');
    out
}

/// Build a v2 request (with id).
pub fn build_request(id: &str, method: &'static str, params_json: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(80 + id.len() + params_json.len());
    out.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"");
    out.extend_from_slice(method.as_bytes());
    out.extend_from_slice(b"\",\"params\":");
    out.extend_from_slice(params_json);
    out.extend_from_slice(b",\"id\":\"");
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(b"\"}");
    out
}

/// Build an agent.report notification payload.
pub fn build_report_payload(report_json: &[u8]) -> Vec<u8> {
    // params: {"report": <report_json>}
    let mut params = Vec::with_capacity(12 + report_json.len());
    params.extend_from_slice(b"{\"report\":");
    params.extend_from_slice(report_json);
    params.push(b'}');
    build_notification(METHOD_REPORT, &params)
}

/// Build an agent.report request payload (with ack_event_ids).
pub fn build_report_request(id: &str, report_json: &[u8], ack_event_ids: &[String]) -> Vec<u8> {
    let mut params = Vec::with_capacity(128 + report_json.len());
    params.extend_from_slice(b"{\"report\":");
    params.extend_from_slice(report_json);

    if !ack_event_ids.is_empty() {
        params.extend_from_slice(b",\"ack_event_ids\":[");
        for (i, eid) in ack_event_ids.iter().enumerate() {
            if i > 0 { params.push(b','); }
            params.push(b'"');
            params.extend_from_slice(eid.as_bytes());
            params.push(b'"');
        }
        params.push(b']');
    }
    params.push(b'}');
    build_request(id, METHOD_REPORT, &params)
}

/// Build an agent.basicInfo notification payload.
pub fn build_basic_info_payload(info_json: &[u8]) -> Vec<u8> {
    let mut params = Vec::with_capacity(10 + info_json.len());
    params.extend_from_slice(b"{\"info\":");
    params.extend_from_slice(info_json);
    params.push(b'}');
    build_notification(METHOD_BASIC_INFO, &params)
}

/// Build an agent.pingResult payload.
pub fn build_ping_result(task_id: u64, ping_type: &str, value: i64, finished_at: &str) -> Vec<u8> {
    use std::fmt::Write;
    let mut s = String::with_capacity(256);
    write!(&mut s,
        r#"{{"jsonrpc":"2.0","method":"{}","params":{{"task_id":{},"ping_type":"{}","value":{},"finished_at":"{}"}}}}"#,
        METHOD_PING_RESULT, task_id, ping_type, value, finished_at
    ).unwrap();
    s.into_bytes()
}
```

```rust
// === src/protocol/fsm.rs ===

/// Protocol fallback state machine.
/// Tracks how many consecutive v2 failures have occurred
/// and whether the agent should fall back to v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolState {
    /// Operating normally with v2 WebSocket.
    V2WebSocket,
    /// Operating with v2 HTTP POST (WebSocket unavailable).
    V2HttpPost,
    /// Fallen back to v1 HTTP POST.
    V1HttpPost,
}

/// Connection lifecycle state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// No active connection.
    Disconnected,
    /// Attempting to connect (TCP + TLS + WS upgrade in progress).
    Connecting,
    /// Connected and operational.
    Connected,
    /// Connected but degraded (e.g., v1 fallback).
    Degraded,
}

/// The fallback FSM tracks v2 protocol failures and manages version fallback.
pub struct FallbackFsm {
    /// Current protocol state.
    pub protocol: ProtocolState,
    /// Consecutive v2 protocol failure count.
    v2_failures: u8,
    /// Threshold before falling back from v2 to v1.
    threshold: u8,
    /// Current connection state.
    pub connection: ConnectionState,
}

impl FallbackFsm {
    pub const DEFAULT_THRESHOLD: u8 = 3;

    pub fn new(initial_protocol_version: i32) -> Self {
        let protocol = if initial_protocol_version >= 2 {
            ProtocolState::V2WebSocket
        } else {
            ProtocolState::V1HttpPost
        };
        Self {
            protocol,
            v2_failures: 0,
            threshold: Self::DEFAULT_THRESHOLD,
            connection: ConnectionState::Disconnected,
        }
    }

    /// Record a successful v2 operation. Resets the failure counter.
    /// If we were in v1 fallback, this means a v2 attempt succeeded — return to v2.
    pub fn record_success(&mut self) {
        self.v2_failures = 0;
        if self.protocol == ProtocolState::V1HttpPost {
            self.protocol = ProtocolState::V2WebSocket;
            self.connection = ConnectionState::Connected;
        }
        self.connection = ConnectionState::Connected;
    }

    /// Record an error for a v2 operation.
    /// Returns true if the caller should now use v1 fallback.
    pub fn record_failure(&mut self, is_v2_protocol_error: bool) -> bool {
        if self.protocol == ProtocolState::V1HttpPost {
            return true; // Already in fallback
        }
        if !is_v2_protocol_error {
            return false; // Network errors don't count as protocol failures
        }
        self.v2_failures += 1;
        if self.v2_failures >= self.threshold {
            self.protocol = ProtocolState::V1HttpPost;
            self.connection = ConnectionState::Degraded;
            true
        } else {
            // Attempt v2 HTTP POST before full v1 fallback
            if self.v2_failures >= 2 {
                self.protocol = ProtocolState::V2HttpPost;
            }
            false
        }
    }

    /// Check if the next connection attempt should use v1.
    pub fn should_use_v1(&self) -> bool {
        self.protocol == ProtocolState::V1HttpPost
    }

    /// Get the current upload protocol version (1 or 2).
    pub fn upload_version(&self) -> u8 {
        match self.protocol {
            ProtocolState::V2WebSocket | ProtocolState::V2HttpPost => 2,
            ProtocolState::V1HttpPost => 1,
        }
    }

    /// Reset the FSM (on full reconnect success).
    pub fn reset(&mut self) {
        self.v2_failures = 0;
        self.protocol = ProtocolState::V2WebSocket;
        self.connection = ConnectionState::Connected;
    }

    /// Called when connection is lost.
    pub fn on_disconnect(&mut self) {
        self.connection = ConnectionState::Disconnected;
    }

    /// Called when attempting to connect.
    pub fn on_connecting(&mut self) {
        self.connection = ConnectionState::Connecting;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_three_strikes_fallback() {
        let mut fsm = FallbackFsm::new(2);
        assert_eq!(fsm.protocol, ProtocolState::V2WebSocket);

        // Strike 1
        assert!(!fsm.record_failure(true));
        assert_eq!(fsm.protocol, ProtocolState::V2WebSocket);

        // Strike 2
        assert!(!fsm.record_failure(true));
        assert_eq!(fsm.protocol, ProtocolState::V2HttpPost);

        // Strike 3 → fallback
        assert!(fsm.record_failure(true));
        assert_eq!(fsm.protocol, ProtocolState::V1HttpPost);

        // Network error does NOT count as protocol failure
        let mut fsm2 = FallbackFsm::new(2);
        assert!(!fsm2.record_failure(false));
        assert_eq!(fsm2.protocol, ProtocolState::V2WebSocket);
    }

    #[test]
    fn test_success_resets_counter() {
        let mut fsm = FallbackFsm::new(2);
        fsm.record_failure(true);
        fsm.record_failure(true);
        assert_eq!(fsm.v2_failures, 2);

        fsm.record_success();
        assert_eq!(fsm.v2_failures, 0);
        assert_eq!(fsm.protocol, ProtocolState::V2WebSocket);
    }
}
```

```rust
// === src/server/backoff.rs ===

use std::time::Duration;
use std::num::Wrapping;

/// Simple xorshift PRNG for jitter (no std::rand dependency).
struct XorShift(Wrapping<u64>);

impl XorShift {
    fn new(seed: u64) -> Self { Self(Wrapping(seed | 1)) }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.0
    }
}

/// Exponential backoff with jitter.
pub struct Backoff {
    initial: Duration,
    max: Duration,
    current: Duration,
    attempts: u32,
    rng: XorShift,
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            current: initial,
            attempts: 0,
            rng: XorShift::new(0xDEAD_BEEF_CAFE_BABE),
        }
    }

    pub fn default_for_agent() -> Self {
        Self::new(
            Duration::from_secs(1),
            Duration::from_secs(300), // 5 min cap
        )
    }

    /// Return the next delay with +/- 25% jitter.
    pub fn next_delay(&mut self) -> Duration {
        self.attempts += 1;
        let base = self.current.as_millis() as u64;

        // Jitter: +/- 25%
        let jitter_range = base / 4;
        let jitter = if jitter_range > 0 {
            (self.rng.next() % (jitter_range * 2)) as i64 - jitter_range as i64
        } else {
            0
        };

        let delay = (base as i64 + jitter).max(0) as u64;

        // Exponential backoff: current *= 2, capped at max
        self.current = (self.current * 2).min(self.max);

        Duration::from_millis(delay)
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
        self.attempts = 0;
    }
}
```

```rust
// === src/monitor/gpu/mod.rs (GPU types) ===

use crate::json::{JsonBuf, EncodeJson};

/// Detailed GPU information for one GPU device.
#[derive(Debug, Clone)]
pub struct GpuInfo {
    /// GPU model name (e.g. "NVIDIA GeForce RTX 4090")
    pub name: String,
    /// Total VRAM in bytes
    pub memory_total: u64,
    /// Used VRAM in bytes
    pub memory_used: u64,
    /// GPU utilization percentage (0-100)
    pub utilization: f64,
    /// GPU temperature in Celsius
    pub temperature: f64,
}

/// Aggregated GPU report.
#[derive(Debug, Clone)]
pub struct GpuReport {
    /// Number of GPUs detected
    pub count: usize,
    /// Average utilization across all GPUs
    pub average_usage: f64,
    /// Per-GPU detailed info
    pub detailed_info: Vec<GpuInfo>,
}

impl EncodeJson for GpuReport {
    fn encode_json(&self, buf: &mut JsonBuf) {
        buf.push_str(r#"{"count":"#);
        buf.push_u64(self.count as u64);
        buf.push_str(r#","average_usage":"#);
        buf.push_f64_prec2(self.average_usage);
        buf.push_str(r#","detailed_info":["#);
        for (i, gpu) in self.detailed_info.iter().enumerate() {
            if i > 0 { buf.push_byte(b','); }
            gpu.encode_json(buf);
        }
        buf.push_str("]}");
    }
}

impl EncodeJson for GpuInfo {
    fn encode_json(&self, buf: &mut JsonBuf) {
        buf.push_str(r#"{"name":""#);
        buf.push_str(&self.name);
        buf.push_str(r#"","memory_total":"#);
        buf.push_u64(self.memory_total);
        buf.push_str(r#","memory_used":"#);
        buf.push_u64(self.memory_used);
        buf.push_str(r#","utilization":"#);
        buf.push_f64_prec2(self.utilization);
        buf.push_str(r#","temperature":"#);
        buf.push_f64_prec2(self.temperature);
        buf.push_byte(b'}');
    }
}

/// Platform-specific GPU detector type alias.
#[cfg(target_os = "linux")]
pub type GpuDetector = crate::monitor::gpu::linux::LinuxGpuDetector;
#[cfg(target_os = "windows")]
pub type GpuDetector = crate::monitor::gpu::windows::WindowsGpuDetector;
#[cfg(target_os = "macos")]
pub type GpuDetector = crate::monitor::gpu::macos::MacosGpuDetector;
#[cfg(target_os = "freebsd")]
pub type GpuDetector = crate::monitor::gpu::freebsd::FreebsdGpuDetector;

pub fn collect() -> GpuReport {
    GpuDetector::detect().unwrap_or_else(|e| {
        log::warn!("GPU detection failed: {}", e);
        GpuReport {
            count: 0,
            average_usage: 0.0,
            detailed_info: Vec::new(),
        }
    })
}
```

```rust
// === src/platform/mod.rs ===

// Platform-specific type aliases — static dispatch, no trait objects.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "freebsd")]
pub mod freebsd;

/// Re-export current platform modules.
/// All monitoring code uses these aliases for zero-cost platform dispatch.
#[cfg(target_os = "linux")]
pub use linux as current;
#[cfg(target_os = "windows")]
pub use windows as current;
#[cfg(target_os = "macos")]
pub use macos as current;
#[cfg(target_os = "freebsd")]
pub use freebsd as current;
```

---

## 5. API CONTRACT — Komari Server Protocol

This section is the Single Source of Truth for wire compatibility between komari-agent-rs and the Komari server.

### 5.1 Endpoints

| Endpoint | Method | Protocol | Purpose |
|---|---|---|---|
| `POST /api/agent/discover` | HTTP POST | JSON | Auto-registration |
| `{ws}://<endpoint>/api/clients/v2/rpc?token=<token>` | WebSocket | JSON-RPC 2.0 | Primary v2 transport |
| `POST /api/clients/v2/rpc?token=<token>` | HTTP POST | JSON-RPC 2.0 | v2 HTTP POST fallback |
| `{ws}://<endpoint>/api/clients/report?token=<token>` | WebSocket | JSON (v1) | v1 WebSocket transport |
| `POST /api/clients/report?token=<token>` | HTTP POST | JSON (v1) | v1 HTTP POST fallback |
| `POST /api/clients/task/result?token=<token>` | HTTP POST | JSON | V1 task result upload |
| `{ws}://<endpoint>/api/clients/terminal?token=<token>&id=<id>` | WebSocket | Binary/Text | Terminal PTY bridge |

### 5.2 WebSocket Upgrade Headers

```
GET /api/clients/v2/rpc?token=<agent_token> HTTP/1.1
Host: <server_host>
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Key: <base64 16 random bytes>
Sec-WebSocket-Version: 13
CF-Access-Client-Id: <cf_client_id>      (optional)
CF-Access-Client-Secret: <cf_secret>    (optional)
```

Expected response:
```
HTTP/1.1 101 Switching Protocols
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Accept: <base64(sha1(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"))>
```

### 5.3 JSON-RPC 2.0 Message Formats

#### agent.report (Notification, agent → server)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.report",
    "params": {
        "report": {
            "cpu": {"usage": 12.5},
            "ram": {"total": 17179869184, "used": 8589934592},
            "swap": {"total": 2147483648, "used": 0},
            "load": {"load1": 0.25, "load5": 0.32, "load15": 0.28},
            "disk": {"total": 268435456000, "used": 134217728000},
            "network": {"up": 125000.0, "down": 500000.0, "totalUp": 1099511627776, "totalDown": 2199023255552},
            "connections": {"tcp": 42, "udp": 15},
            "uptime": 86400,
            "process": 234,
            "gpu": {
                "count": 1,
                "average_usage": 45.0,
                "detailed_info": [{
                    "name": "NVIDIA GeForce RTX 4090",
                    "memory_total": 25769803776,
                    "memory_used": 4294967296,
                    "utilization": 45.0,
                    "temperature": 62.0
                }]
            },
            "message": ""
        }
    }
}
```

#### agent.report (Request with ack_event_ids, HTTP POST fallback)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.report",
    "params": {
        "report": { ... },
        "ack_event_ids": ["evt-001", "evt-002"]
    },
    "id": "report-1718886400123456789"
}
```

#### agent.basicInfo (Notification)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.basicInfo",
    "params": {
        "info": {
            "cpu": {
                "cpu_name": "Intel(R) Core(TM) i7-13700K",
                "cpu_architecture": "amd64",
                "cpu_cores": 24,
                "cpu_physical_cores": 16,
                "cpu_usage": 12.5
            },
            "ram": {"total": 17179869184, "used": 8589934592},
            "swap": {"total": 2147483648, "used": 0},
            "disk": {"total": 268435456000, "used": 134217728000},
            "os_name": "Ubuntu 22.04.3 LTS",
            "kernel_version": "6.5.0-14-generic",
            "platform": "linux",
            "virtualization": "",
            "gpu": ["NVIDIA GeForce RTX 4090"],
            "boot_time": 1718800000,
            "agent_version": "0.1.0",
            "ipv4": "192.168.1.100",
            "ipv6": "fe80::1",
            "country_code": "CN"
        }
    }
}
```

#### agent.pull (Request, HTTP POST fallback)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.pull",
    "params": {
        "capabilities": ["exec", "ping", "message", "event", "terminal"],
        "ack_event_ids": []
    },
    "id": "pull-1718886401123456790"
}
```

#### agent.pull Response (server → agent)

```json
{
    "jsonrpc": "2.0",
    "id": "pull-1718886401123456790",
    "result": {
        "status": "ok",
        "events": [
            {
                "id": "evt-003",
                "method": "agent.exec",
                "params": {
                    "task_id": "task-abc",
                    "command": "ls -la /tmp"
                },
                "created_at": "2024-06-20T12:00:00Z",
                "expires_at": "2024-06-20T12:05:00Z"
            }
        ]
    }
}
```

#### agent.exec Event (server → agent, via WebSocket or pull response)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.exec",
    "params": {
        "task_id": "task-abc",
        "command": "ls -la /tmp"
    }
}
```

#### agent.ping Event (server → agent)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.ping",
    "params": {
        "ping_task_id": 42,
        "ping_type": "icmp",
        "ping_target": "8.8.8.8"
    }
}
```

#### agent.pingResult (agent → server)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.pingResult",
    "params": {
        "task_id": 42,
        "ping_type": "icmp",
        "value": 23,
        "finished_at": "2024-06-20T12:00:05.123456789Z"
    }
}
```

#### agent.taskResult (agent → server, v1 HTTP POST)

```json
{
    "task_id": "task-abc",
    "result": "total 0\ndrwxrwxrwt 1 root root 4096 Jun 20 12:00 .\n",
    "exit_code": 0,
    "finished_at": "2024-06-20T12:00:02.500000000Z"
}
```

#### agent.terminal.request (server → agent)

```json
{
    "jsonrpc": "2.0",
    "method": "agent.terminal.request",
    "params": {
        "request_id": "term-xyz"
    }
}
```

### 5.4 V1 Protocol Format

V1 uses flat JSON without JSON-RPC envelope:

```json
{
    "cpu": {"usage": 12.5},
    "ram": {"total": 17179869184, "used": 8589934592},
    "swap": {"total": 2147483648, "used": 0},
    "load": {"load1": 0.25, "load5": 0.32, "load15": 0.28},
    "disk": {"total": 268435456000, "used": 134217728000},
    "network": {"up": 125000.0, "down": 500000.0, "totalUp": 1099511627776, "totalDown": 2199023255552},
    "connections": {"tcp": 42, "udp": 15},
    "uptime": 86400,
    "process": 234,
    "message": ""
}
```

Sent to `POST /api/clients/report?token=<token>` as `Content-Type: application/json`.

### 5.5 HTTP Content-Encoding

When compression is enabled (default):
- Outbound HTTP POST bodies (report, pull, pingResult) MAY be gzip-compressed
- Header: `Content-Encoding: gzip`
- Server MUST accept both compressed and uncompressed bodies
- WebSocket messages use per-message deflate (RFC 7692), negotiated at WebSocket upgrade

### 5.6 Field Type Reference

| Field | Type | Unit | Notes |
|---|---|---|---|
| `cpu.usage` | float64 | percent (0-100) | Always >= 0.001 if non-zero |
| `ram.total` | uint64 | bytes | |
| `ram.used` | uint64 | bytes | Depends on mem_mode config |
| `swap.total` | uint64 | bytes | |
| `swap.used` | uint64 | bytes | |
| `load.load1` | float64 | 1-min load avg | |
| `load.load5` | float64 | 5-min load avg | |
| `load.load15` | float64 | 15-min load avg | |
| `disk.total` | uint64 | bytes | Sum of physical disks |
| `disk.used` | uint64 | bytes | Sum of physical disks |
| `network.up` | float64 | bytes/sec | Current upload speed |
| `network.down` | float64 | bytes/sec | Current download speed |
| `network.totalUp` | uint64 | bytes | Cumulative upload |
| `network.totalDown` | uint64 | bytes | Cumulative download |
| `connections.tcp` | uint64 | count | Active TCP connections |
| `connections.udp` | uint64 | count | Active UDP sockets |
| `uptime` | uint64 | seconds | System uptime |
| `process` | uint64 | count | Total processes |
| `gpu.count` | uint64 | count | Number of GPUs |
| `gpu.average_usage` | float64 | percent (0-100) | |
| `gpu.detailed_info[].name` | string | | GPU model name |
| `gpu.detailed_info[].memory_total` | uint64 | bytes | VRAM total |
| `gpu.detailed_info[].memory_used` | uint64 | bytes | VRAM used |
| `gpu.detailed_info[].utilization` | float64 | percent (0-100) | GPU utilization |
| `gpu.detailed_info[].temperature` | float64 | Celsius | GPU temperature |
| `message` | string | | Error/warning messages |
| `ping_type` | string | | "icmp", "tcp", or "http" |
| `value` | int | ms | Ping latency, -1 on failure |
| `exit_code` | int | | Task exit code, -1 on error |
| `finished_at` | string (RFC 3339 Nano) | | Completion timestamp |

### 5.7 Error Codes

| Code | Meaning |
|---|---|
| -32700 | Parse error (invalid JSON) |
| -32600 | Invalid Request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |
| HTTP 401 | Invalid token |
| HTTP 404 | Endpoint not found |
| HTTP 500 | Server internal error |

---

## 6. MEMORY BUDGET

### 6.1 Steady-State RSS Estimate

| Component | Size | Justification |
|---|---|---|
| Rust runtime (allocator, panic handler, TLS) | ~500 KB | jemalloc/System allocator overhead, thread-local storage, signal handlers |
| ScratchArena (8 KB) | 8 KB | Fixed stack buffer for /proc reads + JSON output |
| Monitor struct + prev_net state | ~200 B | u64 fields, SmallVec inline storages |
| Config struct (clone) | ~400 B | 34 fields, mostly small strings |
| WebSocket buffer (tungstenite) | ~65 KB | Default read/write buffers per tungstenite |
| rustls TLS session | ~200 KB | Certificate store, session keys, buffers |
| DNS cache | ~10 KB | TTL-bounded host→IP mapping |
| Netstatic history (VecDeque) | ~50 KB | 720 entries × ~70 bytes each (in heap, not arena) |
| JSON payload buffer (WS send) | ~8 KB | Encoded monitoring report + v2 envelope |
| Gzip encoder scratch | ~80 KB | LZ77 hash table (32K × 2 bytes + 32K × 4 bytes for chain) |
| Terminal PTY buffers (only when active) | ~16 KB | I/O buffers, only allocated during terminal session |
| Backoff + FSM state | ~200 B | Enum fields, counters |
| Log buffer | ~32 KB | env_logger or simple ring buffer |
| Stack (main thread) | ~2 MB | Default Windows/Linux thread stack |
| **Total (steady-state, no terminal)** | **~2.97 MB** | Within 3 MB target |
| Terminal session peak | +~16 KB | PTY pipe buffers |
| Self-update peak | +~5 MB | Download buffer for new binary (transient, freed after rename) |

### 6.2 Peak vs Steady-State

| State | RSS | Notes |
|---|---|---|
| Startup (first connect) | ~2.5 MB | DNS resolution, TLS handshake |
| Steady-state (monitoring) | ~2.97 MB | After 60s of running |
| Self-update active | ~8 MB | Downloading new binary (5 MB + overhead) |
| Terminal session active | ~3.1 MB | PTY I/O buffers |
| Graceful shutdown | ~1 MB | Flush netstatic, close WS, OS reclaim |

### 6.3 Zero-Allocation Hot Path Verification

The monitoring tick path (`monitor.tick()`) is designed for zero heap allocations:

1. **Arena**: All /proc reads and temporary strings borrow from `ScratchArena`. The arena resets each tick.
2. **JsonBuf**: Encodes directly into arena-backed buffer. No `String`, no `Vec` in hot path.
3. **SmallVec**: All collector result arrays use inline storage. Never exceeds N (N=16 for network interfaces, N=8 for GPUs).
4. **Stack structs**: `CpuInfo`, `RamInfo`, `LoadInfo` etc. are all `Copy` or small fixed-size structs.

Verification method: `dhat` heap profiler or custom `#[global_allocator]` that panics on allocation during the tick.

### 6.4 Acceptable Allocations (Non-Hot-Path)

| Operation | Allocation | Why Acceptable |
|---|---|---|
| TLS handshake | rustls allocs | One-time on connect |
| DNS resolution | UDP packet + response parse | Once per reconnect |
| Gzip compression | ~80 KB for LZ77 table | Only in HTTP POST fallback path |
| Task exec result upload | Vec for result string | Infrequent (on server request) |
| Self-update | Download buffer | Very rare |
| Auto-discovery POST | Small JSON body Vec | Once at startup |
| Terminal PTY | I/O buffers | Only during active session |

---

## 7. BINARY BUDGET

### 7.1 Per-Module Size Estimate (release, opt-level="z", lto="fat", stripped)

| Module | Code Size | Data Size | Total | Notes |
|---|---|---|---|---|
| `main.rs` + `app.rs` | ~1 KB | — | ~1 KB | Entry point + CLI dispatch |
| `config.rs` (clap) | ~8 KB | — | ~8 KB | clap derive generates some code |
| `arena.rs` | ~1 KB | — | ~1 KB | Bump allocator logic |
| `json.rs` | ~3 KB | — | ~3 KB | JsonBuf + Field enum |
| `crypto.rs` (SHA-1 + base64) | ~2 KB | ~0.5 KB | ~2.5 KB | Stack-only, no tables needed |
| `protocol/` (v2 + v1 + fsm) | ~5 KB | — | ~5 KB | Pure structs + builders |
| `ws.rs` | ~3 KB | — | ~3 KB | TCP + TLS + WS handshake |
| `http.rs` | ~3 KB | — | ~3 KB | Manual HTTP/1.1 POST |
| `tls.rs` | ~1 KB | — | ~1 KB | rustls config builder |
| `dns.rs` | ~5 KB | ~2 KB | ~7 KB | DNS query build/parse + server list |
| `gzip.rs` (custom encoder) | ~8 KB | ~3 KB | ~11 KB | CRC32 table (1 KB), Huffman tables (~0.8 KB), LZ77 hash chain (>0 KB: stack-allocated per call), BitWriter code |
| `server/mod.rs` + reconnection + backoff | ~6 KB | — | ~6 KB | Tick loop + reconnect logic |
| `server/task.rs` + ping_*.rs | ~8 KB | — | ~8 KB | Exec + ping dispatch |
| `server/cf_access.rs` | ~1 KB | — | ~1 KB | Header injection |
| `monitor/cpu/*.rs` (4 platforms) | ~6 KB | — | ~6 KB | Per-platform CPU collection |
| `monitor/mem/*.rs` (4 platforms) | ~8 KB | — | ~8 KB | Per-platform memory (+ 3-mode logic) |
| `monitor/disk/*.rs` (4 platforms) | ~5 KB | ~1 KB | ~6 KB | Per-platform disk (+ physical filter list) |
| `monitor/net/*.rs` (4 platforms) | ~6 KB | — | ~6 KB | Per-platform network |
| `monitor/load/*.rs` (4 platforms) | ~3 KB | — | ~3 KB | Per-platform load |
| `monitor/connections/*.rs` (4 platforms) | ~3 KB | — | ~3 KB | Per-platform connections |
| `monitor/process/*.rs` (4 platforms) | ~4 KB | — | ~4 KB | Per-platform process count |
| `monitor/uptime/*.rs` (4 platforms) | ~2 KB | — | ~2 KB | Per-platform uptime |
| `monitor/ip/*.rs` (4 platforms) | ~5 KB | — | ~5 KB | Per-platform IP detection |
| `monitor/gpu/*.rs` (4 platforms) | ~10 KB | — | ~10 KB | GPU detection + nvidia-smi/rocm-smi/DXGI exec |
| `monitor/os.rs` (all platforms) | ~5 KB | ~1 KB | ~6 KB | OS name + heuristics |
| `monitor/virtualization.rs` | ~3 KB | — | ~3 KB | VM/container detection |
| `monitor/netstatic.rs` | ~3 KB | — | ~3 KB | Traffic history persistence |
| `platform/*.rs` | ~1 KB | — | ~1 KB | cfg gates + type aliases |
| `terminal/*.rs` (2 platforms) | ~12 KB | — | ~12 KB | PTY/ConPTY |
| `task.rs` | ~1 KB | — | ~1 KB | Task types |
| `update.rs` | ~3 KB | — | ~3 KB | Self-update logic |
| `autodiscovery.rs` | ~2 KB | — | ~2 KB | Auto-registration |
| **Subtotal (application code)** | | | **~144 KB** | |
| rustls (TLS library) | ~200 KB | ~50 KB | ~250 KB | WebPKI certs + TLS state machine |
| webpki-roots (cert store) | — | ~100 KB | ~100 KB | Mozilla root CA certs (binary blob) |
| tungstenite (WebSocket) | ~30 KB | — | ~30 KB | WS frame + per-message deflate |
| clap (CLI parser) | ~25 KB | — | ~25 KB | Derive macro + parser |
| log + env_logger | ~10 KB | — | ~10 KB | Logging facade |
| windows-rs (Windows only) | ~40 KB | — | ~40 KB | DXGI + Toast + Registry bindings (only on windows target) |
| Rust std (alloc, core, panic) | ~60 KB | — | ~60 KB | Minimal std usage (no HashMap, no async) |
| **Subtotal (dependencies)** | | | **~515 KB** (linux) / **~555 KB** (windows) | |
| **GRAND TOTAL (linux-amd64)** | | | **~659 KB** | Well under 1 MB |
| **GRAND TOTAL (windows-amd64)** | | | **~699 KB** | Well under 1 MB |

### 7.2 Size Reduction Levers

| Technique | Savings | Applied? |
|---|---|---|
| `opt-level = "z"` | ~30% vs "s" | Yes |
| `lto = "fat"` | ~15% cross-crate inlining | Yes |
| `codegen-units = 1` | ~10% optimization scope | Yes |
| `panic = "abort"` | ~15 KB (no unwinding tables) | Yes |
| `strip = "symbols"` | ~200-500 KB (debug info) | Yes |
| Custom JSON (no serde) | ~300 KB vs serde_json | Yes |
| Custom gzip (no flate2) | ~20 KB vs flate2/miniz_oxide | Yes |
| Self-implemented SHA-1+base64 | ~5 KB vs sha1+base64 crates | Yes |
| No async runtime (no tokio) | ~1 MB vs tokio | Yes — this is the largest single saving |
| Xargo/build-std with panic_immediate_abort | ~10 KB | Optional |
| UPX compression | ~50-70% size reduction | Optional post-build |

### 7.3 `.cargo/config.toml` Release Profile

```toml
[profile.release]
opt-level = "z"        # Optimize for size
lto = "fat"           # Link-time optimization across all crates
codegen-units = 1     # Single codegen unit for maximum optimization
panic = "abort"       # No unwind tables
strip = "symbols"     # Remove all symbols
debug = 0             # No debug information
rpath = false
incremental = false
```

---

## 8. IMPLEMENTATION ROADMAP

### 8.1 Phase Overview

| Phase | Name | Lines | Duration | Cumulative Lines |
|---|---|---|---|---|
| P1 | Foundation + Handshake | ~600 | 2-3 days | 600 |
| P2 | Linux Metrics + Zero-Alloc Loop | ~750 | 4-5 days | 1,350 |
| P3 | Protocol FSM + Fallback | ~500 | 3-4 days | 1,850 |
| P4 | Cross-Platform Metrics | ~1,535 | 6-8 days | 3,385 |
| P5 | Terminal + Ping + Gzip + DNS + Update | ~1,045 | 6-8 days | 4,430 |
| P6 | Polish + Packaging | ~510 | 2-3 days | 4,940 |

**Total estimated**: 23-31 days single developer. With P3/P4 parallelization: 18-25 days.

### 8.2 Phase 1: Foundation + Handshake (600 lines)

**Goal**: Binary compiles on 4 targets. Establishes TLS WebSocket. Sends static heartbeat.

**File creation order** (dependencies first):
1. `Cargo.toml` — workspace, dependencies, features
2. `.cargo/config.toml` — release profile
3. `src/config.rs` — `Config` struct (clap derive)
4. `src/crypto.rs` — SHA-1 + base64
5. `src/json.rs` — `JsonBuf` + `Field` + `EncodeJson`
6. `src/tls.rs` — rustls config
7. `src/dns.rs` — stub (system resolver only)
8. `src/ws.rs` — WebSocket connect + handshake
9. `src/http.rs` — stub (not yet used)
10. `src/protocol/v2.rs` — JSON-RPC 2.0 types
11. `src/protocol/v1.rs` — V1 types
12. `src/protocol/mod.rs` — re-exports
13. `src/server/mod.rs` — initial `run()` with connect + static heartbeat
14. `src/app.rs` — CLI dispatch
15. `src/main.rs` — entry point
16. `.github/workflows/ci.yml` — 4-platform CI

**Success criteria**:
- [ ] `cargo build --release` green on linux, windows, macos, freebsd
- [ ] Binary connects to real Komari server, completes WS handshake (101)
- [ ] Server receives valid static heartbeat JSON
- [ ] Binary size < 2 MB stripped (all platforms)
- [ ] `cargo test` passes: SHA-1 RFC vector, base64 RFC vector, JSON round-trip
- [ ] SHA-1 test vector: `dGhlIHNhbXBsZSBub25jZQ==` + GUID → `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`

### 8.3 Phase 2: Linux Metrics + Zero-Alloc Loop (750 lines)

**Goal**: Full Linux monitoring suite. Scratch arena. Zero-allocation 1-second loop. Prove RSS < 3 MB.

**Dependencies**: Phase 1 complete.

**File creation order**:
1. `src/arena.rs` — `ScratchArena` + `SmallVec`
2. `src/platform/mod.rs` — cfg-gated type aliases
3. `src/platform/linux.rs` — module gate
4. `src/monitor/mod.rs` — `Monitor` struct + `tick()` orchestrator
5. `src/monitor/cpu/mod.rs` + `src/monitor/cpu/linux.rs`
6. `src/monitor/mem/mod.rs` + `src/monitor/mem/linux.rs`
7. `src/monitor/disk/mod.rs` + `src/monitor/disk/linux.rs`
8. `src/monitor/net/mod.rs` + `src/monitor/net/linux.rs`
9. `src/monitor/load/mod.rs` + `src/monitor/load/linux.rs`
10. `src/monitor/connections/mod.rs` + `src/monitor/connections/linux.rs`
11. `src/monitor/process/mod.rs` + `src/monitor/process/linux.rs`
12. `src/monitor/uptime/mod.rs` + `src/monitor/uptime/linux.rs`
13. Expand `src/monitor/mod.rs` — integrate all collectors into `tick()`
14. Expand `src/server/mod.rs` — integrate monitor tick loop
15. Expand `src/json.rs` — add `EncodeJson` impls for all monitoring types

**Success criteria**:
- [ ] All Linux collectors produce JSON identical in structure to Go agent
- [ ] 3-mode RAM calculation matches Go output (modes 0, 1, 2) on same host
- [ ] `is_physical_disk()` produces same mount list as Go on same host
- [ ] Zero heap allocation in `monitor.tick()` hot path (verify via dhat)
- [ ] RSS < 3 MB after 60s of running (VmRSS)
- [ ] 1-second tick jitter < 50ms under idle system
- [ ] `cargo test` passes all collector unit tests with fixture /proc data

### 8.4 Phase 3: Protocol FSM + Fallback (500 lines)

**Goal**: v2/v1 protocol negotiation. HTTP POST fallback. Exponential backoff. Exec/ping stubs.

**File creation order**:
1. `src/protocol/fsm.rs` — `FallbackFsm` + `ConnectionFsm`
2. `src/server/backoff.rs` — `Backoff` with jitter
3. `src/server/reconnection.rs` — reconnection loop
4. Expand `src/http.rs` — manual HTTP/1.1 POST
5. `src/server/task.rs` — stub exec + ping handlers
6. Expand `src/protocol/v2.rs` — `BuildReportPayload`, `BuildReportRequest`, `BuildBasicInfoPayload`
7. Expand `src/server/mod.rs` — integrate reconnection + FSM + backoff

**Success criteria**:
- [ ] FSM transitions verified: connect → WS fail ×3 → HTTP POST → v1 fallback
- [ ] 3-strike counter: 3 consecutive v2 failures → v1; 1 success resets
- [ ] HTTP POST fallback sends identical JSON body to WS mode
- [ ] Backoff: ~1s after 3 failures; caps at 5 min after 10 failures
- [ ] Reconnection survives server restart
- [ ] `cargo test` includes FSM transition table test (all 12 edges)
- [ ] Recorded session replay: Go agent session replayed against Rust agent, JSON matches

### 8.5 Phase 4: Cross-Platform Metrics (1,535 lines)

**Goal**: Windows/macOS/FreeBSD metrics. GPU across 4 platforms. OS + virtualization detection. CI fully green.

**File creation order**:
1. `src/platform/windows.rs`, `src/platform/macos.rs`, `src/platform/freebsd.rs`
2. All `monitor/*/windows.rs` files (CPU, mem, disk, net, load, connections, process, uptime, ip)
3. All `monitor/*/macos.rs` files
4. All `monitor/*/freebsd.rs` files
5. `src/monitor/gpu/mod.rs` + `src/monitor/gpu/linux.rs` + `src/monitor/gpu/windows.rs` + `src/monitor/gpu/macos.rs` + `src/monitor/gpu/freebsd.rs`
6. `src/monitor/os.rs` — OS name + kernel version (all platforms)
7. `src/monitor/virtualization.rs` — VM/container detection (all platforms)
8. Expand `src/monitor/uptime/mod.rs` — platform dispatch
9. Expand CI workflow for full 4-platform matrix

**Success criteria**:
- [ ] All 4 platform binaries pass `cargo test` in CI
- [ ] GPU detection: NVIDIA (nvidia-smi), AMD (rocm-smi), Intel (DRM fallback), Apple Silicon (system_profiler)
- [ ] Memory 3-mode: validated against Go on Windows
- [ ] OS detection: correctly identifies Ubuntu 22.04, Debian 12, Windows 11, macOS 15, FreeBSD 14
- [ ] Virtualization: detects Docker, KVM, VMware, Hyper-V
- [ ] CI matrix: 4 OS × (build + test + clippy + fmt) all green
- [ ] No platform-specific code in shared modules

### 8.6 Phase 5: Terminal + Ping + Gzip + DNS + Update (1,045 lines)

**Goal**: Full terminal PTY/ConPTY. ICMP/TCP/HTTP ping. Gzip compression. DNS resolver. Self-update.

**File creation order**:
1. `src/gzip.rs` — fixed-Huffman DEFLATE encoder (CRC32, BitWriter, LZ77, gzip wrapper)
2. Expand `src/dns.rs` — full custom DNS resolver with cache
3. `src/terminal/mod.rs` + `src/terminal/unix.rs` + `src/terminal/windows.rs`
4. `src/server/ping_icmp.rs` + `src/server/ping_tcp.rs` + `src/server/ping_http.rs`
5. `src/server/cf_access.rs` — Cloudflare Access headers
6. `src/task.rs` — ExecTask, PingTask types
7. Expand `src/server/task.rs` — real exec + ping implementations
8. `src/update.rs` — self-update
9. `tests/integration.rs` — full integration test with dummy server

**Success criteria**:
- [ ] Terminal: `exec ls -la` works on Linux; `dir` works on Windows 10+
- [ ] Terminal graceful shutdown: closing WS sends SIGHUP / TerminateProcess
- [ ] ICMP ping to 8.8.8.8 returns RTT < 500ms (with CAP_NET_RAW)
- [ ] TCP ping to google.com:443 returns RTT
- [ ] HTTP ping to https://google.com returns 200
- [ ] Gzip: encoding 2 KB JSON produces valid gzip accepted by `gunzip`
- [ ] DNS: resolve with each configured DNS server; IPv4 prefer; IPv6 flag routes to AAAA
- [ ] Self-update: detects newer release, downloads, replaces binary, exits 42
- [ ] CF Access: WS upgrade includes CF headers when configured
- [ ] Full integration test: connect → heartbeat → task exec → disconnect → reconnect

### 8.7 Phase 6: Polish + Packaging (510 lines)

**Goal**: Auto-discovery. Persistent network stats. Windows toast. Install scripts. Documentation.

**File creation order**:
1. `src/monitor/netstatic.rs` — persistent traffic history
2. `src/autodiscovery.rs` — auto-registration
3. `src/platform/windows_toast.rs` — Windows toast notification (inside `platform/windows.rs` or separate)
4. Expand `src/server/mod.rs` — graceful shutdown, autodiscovery integration, netstatic flush
5. `scripts/install.sh` — Linux/macOS install
6. `scripts/install.ps1` — Windows install
7. Expand `README.md` — full documentation
8. `docs/protocol.md` — protocol reference
9. `docs/building.md` — build guide

**Success criteria**:
- [ ] Auto-discovery: fresh agent registers; restart picks up existing config
- [ ] Network stats: net_static.json persists across restarts
- [ ] Windows toast: notification appears on Windows 10/11 (or MessageBoxW fallback)
- [ ] Install scripts complete successfully on Ubuntu 22.04, macOS 15, Windows 11
- [ ] Agent starts and heartbeats after scripted install
- [ ] All Phase 1-5 success criteria still pass (no regressions)
- [ ] README covers: overview, features, platform support, install, configure, build, troubleshoot

### 8.8 Phase Dependency Graph

```
Phase 1 (Foundation)
  │
  ▼
Phase 2 (Linux Metrics)
  │
  ├──────────────────────┐
  ▼                      ▼
Phase 3 (Protocol FSM)   Phase 4 (Cross-Platform)  ← parallelizable
  │                      │
  └──────────┬───────────┘
             ▼
        Phase 5 (Terminal + Ping + Gzip + DNS + Update)
             │
             ▼
        Phase 6 (Polish + Packaging)
```

**Critical path**: P1 → P2 → P3 → P5 → P6 (P4 parallel with P3 if two developers).

---

## 9. DESIGN DECISIONS LOG

### DD-001: Custom JSON Encoder Instead of serde

**Decision**: Implement `JsonBuf` + `Field` enum + `EncodeJson` trait. Do not use serde or serde_json in the hot path.

**Rationale**:
- serde_json adds ~300 KB to binary size (codegen from derive macros + `Value` type)
- serde's `Value` type allocates for strings and nested objects
- The agent's JSON format is fixed and simple — known keys, known types
- Direct field-by-field encoding into a stack buffer eliminates all allocations
- Wire compatibility is verified by test vectors against Go output

**Rejected alternative**: Using serde_json with `serde_json::to_writer`. Rejected due to binary size and allocation overhead.

**Resolution note**: serde_json is used for *parsing* incoming JSON (server responses, events) since parsing is infrequent and the convenience outweighs the binary cost. The hot path is output-only.

### DD-002: Sync Single-Threaded, No Async Runtime

**Decision**: Use `std::net::TcpStream` with non-blocking read via `poll()`/`select()`. No tokio, no async-std, no smol.

**Rationale**:
- tokio adds 1+ MB to binary and 200+ transitive dependencies
- The agent maintains exactly one connection to one server
- The monitoring loop is a simple 1-second sleep between ticks
- Non-blocking socket read with timeout is sufficient for incoming message handling
- Eliminates all async/await complexity, Pin, and Send bounds

**Rejected alternative**: tokio with `tokio-tungstenite`. Rejected due to binary size constraint.

**Resolution note**: On Windows, `select()` is used instead of `poll()` since Windows does not support POSIX `poll()` on sockets. A thin `Poller` abstraction wraps the platform difference.

### DD-003: Self-Implemented SHA-1 and Base64

**Decision**: Implement SHA-1 (~100 lines per RFC 3174) and base64 encode (~60 lines per RFC 4648) from scratch.

**Rationale**:
- SHA-1 is only used for WebSocket handshake `Sec-WebSocket-Accept` computation
- The SHA-1 crate pulls in `cpufeatures`, `digest`, `block-buffer`, `crypto-common`, `typenum`, `generic-array`, `version_check`, `cfg-if`
- Base64 crate pulls in its own dependency tree
- Combined self-implementation is ~160 lines with zero dependencies
- WebSocket handshake is one-time at connection start — performance is irrelevant

**Rejected alternative**: `sha1` + `base64` crates. Rejected due to dependency tree size.

**Risks**: SHA-1 must match RFC 6455 test vector exactly. Verified in Phase 1 test suite.

### DD-004: Fixed Huffman DEFLATE Instead of Dynamic Huffman

**Decision**: Implement a fixed-Huffman DEFLATE encoder only (RFC 1951 BTYPE=01). No dynamic Huffman tree construction.

**Rationale**:
- Fixed Huffman is ~350 lines vs ~600+ for dynamic
- For JSON payloads (dominated by ASCII 0x20-0x7E), fixed codes are already optimal
- Compression ratio penalty is 3-5% vs dynamic for monitoring JSON
- 3-5% worse compression on 2-15 KB payloads = ~60-500 bytes extra per message — negligible
- Avoids Huffman tree serialization bugs

**Rejected alternative**: Full dynamic Huffman DEFLATE. Rejected due to code size and complexity.

**Rejected alternative**: flate2/miniz_oxide crate. Rejected due to binary size (20-50 KB added).

**Stored-blocks fallback**: For payloads under 200 bytes, stored-blocks-only DEFLATE (BTYPE=00) is used, trading zero compression for minimal CPU.

### DD-005: Explicit Config Passing, No Global

**Decision**: Every function that needs config receives `&Config` as a parameter. No `GlobalConfig` or `lazy_static`.

**Rationale**:
- The Go codebase's `GlobalConfig` is imported by every module (module-inventory cross-cutting concern §3.1)
- Global state prevents unit testing without environment setup
- Rust's ownership model naturally supports explicit parameter passing
- `clap` derive + `#[arg(env)]` replaces Go's env tag reflection

**Rejected alternative**: `once_cell::sync::Lazy<Config>`. Rejected due to testability and explicitness.

### DD-006: cfg-Gated Type Aliases, Not Trait Objects

**Decision**: `type CurrentCpu = linux::Cpu` pattern with `#[cfg(target_os)]` gates. No trait objects in the hot path.

**Rationale**:
- Trait objects require vtable dispatch on every call (cost in hot path)
- Platform is known at compile time — there is never runtime platform switching
- `cfg` gates produce zero-cost abstraction: the compiler monomorphizes to one platform
- Each platform module has identical function signatures; the type alias selects at compile time

**Rejected alternative**: `Platform` trait with `dyn Platform`. Rejected due to vtable overhead.

### DD-007: Scratch Arena Over malloc

**Decision**: All /proc reads and temporary data in the monitoring tick borrow from a `ScratchArena` (8 KB stack buffer reset each tick).

**Rationale**:
- Eliminates all heap allocation in the 1-second tick
- Arena reset is a single pointer assignment (O(1))
- 8 KB is sufficient for all /proc reads + JSON output (typical report is 2-3 KB)
- No fragmentation, no allocator contention

**Rejected alternative**: `Vec<u8>` with `clear()`. Rejected because `clear()` retains capacity (heap memory) and allocation may occur.

**Overflow handling**: If arena is exhausted (should never happen with 8 KB), `JsonBuf` falls back to heap `Vec`. This is a safety net, not a hot path.

### DD-008: No gzip Decompression

**Decision**: The agent sends gzip-compressed data but never receives gzip-compressed responses. No decompression code implemented.

**Rationale**:
- The Go agent never decompresses gzip (verified by code audit: no `gzip.NewReader()` anywhere)
- Server responses are plain JSON-RPC
- WebSocket per-message deflate is handled transparently by tungstenite

**Forward compatibility**: If server ever sends compressed responses, add flate2/miniz_oxide for decompression (decoding DEFLATE is simpler than encoding — no LZ77 search).

### DD-009: WebSocket Over Tungstenite (Sync)

**Decision**: Use `tungstenite` crate (sync mode) with `rustls` for WebSocket.

**Rationale**:
- tungstenite is the most mature Rust WebSocket library
- Sync mode avoids async dependency
- Supports per-message deflate (RFC 7692) matching Go's `EnableCompression`
- Moderate binary size (~30 KB)

**Rejected alternative**: Raw WebSocket implementation over TcpStream. Rejected due to complexity of frame masking, fragmentation, ping/pong, and close handshake.

**Rejected alternative**: `tokio-tungstenite`. Rejected due to async runtime requirement.

### DD-010: clap for CLI Parsing

**Decision**: Use `clap` with derive macros for CLI argument parsing.

**Rationale**:
- Replaces Go's `cobra` + `pflag` + manual env reflection
- `#[arg(env = "VAR")]` provides env var override
- ~25 KB binary overhead — acceptable
- clap is the Rust CLI standard

**Rejected alternative**: `lexopt` (smaller but no derive, no env support). Rejected due to ergonomics.

**Rejected alternative**: Manual arg parsing. Rejected due to 34 config fields and env var support requirement.

### DD-011: windows-rs for Windows-Specific Functionality

**Decision**: Use `windows-rs` crate (`Windows::Win32::*`) for Windows-specific functionality.

**Rationale**:
- Replaces Go's raw `syscall.SyscallN` with COM vtables
- Provides safe wrappers for DXGI (GPU), Registry (OS version), Toast notifications, performance counters
- Only linked on Windows targets (no cross-platform bloat)

**Rejected alternative**: `winapi` crate. Rejected because windows-rs is the official Microsoft crate with better ergonomics and maintenance.

**Note**: GPU VRAM reporting via windows-rs DXGI may differ from Go's manual COM vtables. Cross-validation against Go on same Windows host is required in Phase 4.

### DD-012: Exit Code 42 for Self-Update

**Decision**: Preserve Go's exit code 42 convention for self-update signaling.

**Rationale**:
- The service manager (systemd/launchd/nssm) interprets exit code 42 as "restart after update"
- Must match Go agent behavior exactly for drop-in replacement

**Implementation**: `std::process::exit(42)` after binary replacement.

---

## 10. OPEN QUESTIONS

### OQ-001: gopsutil Equivalent in Rust

**Status**: Deferred to Phase 2 investigation.

**Question**: What is the Rust equivalent of Go's `gopsutil` for cross-platform system metrics?

**Options**:
1. `sysinfo` crate — most popular, covers CPU/mem/disk/net/process. However, does not cover GPU, virtualization, or OS name.
2. Manual /proc + Win32 API + sysctl — matches approach in this document. More code but zero-dependency.
3. `heim` crate — async, requires tokio. Rejected.

**Decision needed**: Whether to use `sysinfo` for Phase 2 Linux metrics (faster development) and replace with manual impl in Phase 4, or go manual from the start.

**Recommendation**: Manual from the start. The code is straightforward and avoids dependency lock-in.

### OQ-002: FreeBSD Test Hardware

**Status**: Needs investigation.

**Question**: How to test FreeBSD builds without dedicated FreeBSD hardware?

**Options**:
1. Cirrus CI freebsd runner
2. QEMU VM
3. Cross-compile only (risk: untested at runtime)

**Recommendation**: Cirrus CI for Phase 1-4; local QEMU VM for Phase 5 terminal testing.

### OQ-003: ConPTY Windows Version Gate

**Status**: Needs runtime testing.

**Question**: `CreatePseudoConsole` is available on Windows 10 1809+. What behavior on older Windows?

**Options**:
1. Detect at runtime: if `CreatePseudoConsole` absent in kernel32.dll, return error "ConPTY not available (requires Windows 10 1809+)"
2. Fallback to legacy Windows console API (no PTY)
3. Use `winpty` (third-party)

**Recommendation**: Option 1. Windows 10 versions before 1809 are EOL.

### OQ-004: Memory Mode Compatibility Matrix

**Status**: Partially resolved, needs cross-validation.

**Question**: The 3 memory calculation modes must produce identical values to the Go agent on all 4 platforms.

**Approach**:
- Port Go's `Ram()` logic exactly (documented in §4, `mem/mod.rs`)
- Cross-validate against Go agent on same host for all 3 modes
- Run Go agent's memory unit tests verbatim as Rust tests

**Open**: The `CallFree()` fallback mode (executing `free -b`) is Linux-only. Should we replicate it in Rust, or accept the `/proc/meminfo` htop-like calculation as sufficient?

**Recommendation**: Replicate `CallFree()` for exact Go compatibility. It is a simple `Command::new("free").arg("-b")` + output parse.

### OQ-005: Self-Update Atomicity on Windows

**Status**: Needs design review.

**Question**: On Windows, replacing a running binary is not atomic. Go uses `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`.

**Options**:
1. Copy new binary to `<name>.new`, call `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`, then `ExitProcess(42)`. Service manager restarts from `.new`.
2. Write to temp, stop service, replace, start service.
3. Two-phase: download to `.new`, on next startup check for `.new` and self-replace before main loop.

**Recommendation**: Option 1 (matching Go behavior).

**Risk**: If service manager is not configured for restart on exit 42, the agent stops and does not come back. This is the same behavior as the Go agent.

### OQ-006: nvidia-smi Path Variability

**Status**: Needs cataloging.

**Question**: Where is `nvidia-smi` located across different Linux distributions and container images?

**Known locations**:
- `/usr/bin/nvidia-smi` (standard)
- `/usr/local/cuda/bin/nvidia-smi` (CUDA toolkit)
- Not in PATH in minimal containers (e.g., `nvidia/cuda:12.4.0-base-ubuntu22.04`)

**Fallback chain**: `nvidia-smi` in PATH → `/usr/bin/nvidia-smi` → `/sys/class/drm` vendor check (no detailed metrics, only presence detection).

**Open**: Should we also check `/usr/local/cuda/bin/nvidia-smi`?

### OQ-007: rocm-smi Output Format Stability

**Status**: Needs testing with ROCm 5.x and 6.x.

**Question**: Does `rocm-smi` output format change across ROCm versions? The key scanner approach (ChatGPT S4) avoids full JSON parse, but key names may still change.

**Risk**: Medium. The Go agent bundles a detailed nvidia-smi and rocm-smi parser. The Rust agent should test against ROCm 5.7 (Ubuntu 22.04 default) and ROCm 6.x (latest).

### OQ-008: Vendor-Specific GPU VRAM Reporting on Windows

**Status**: Needs cross-validation.

**Question**: Does `windows-rs` DXGI report the same VRAM values as Go's manual COM vtables?

**Details**: DXGI reports dedicated VRAM, shared system memory, and total. Go's manual COM may report different numbers depending on how it queries `DXGI_ADAPTER_DESC`.

**Verification**: Run Go agent and Rust agent on same Windows host with NVIDIA GPU. Compare `memory_total` and `memory_used` values. Add config flag `gpu_mem_mode` to match behavior if needed.

### OQ-009: DNS Cache Poisoning Resilience

**Status**: Low priority.

**Question**: The DNS cache has a 5-minute TTL cap. Is this sufficient to prevent stale entries from causing connection failures?

**Analysis**: The agent re-resolves on every reconnect (which may happen due to network flaps). The cache is a performance optimization, not a correctness requirement. A 5-minute cap combined with reconnection-driven re-resolution is sufficient.

### OQ-010: musl Target Support

**Status**: Future consideration.

**Question**: Should the agent support `x86_64-unknown-linux-musl` for fully static binaries?

**Pros**: Fully static binary, no glibc dependency, works on any Linux kernel version.
**Cons**: musl's `std::process::Command` has known issues. `/proc` parsing is identical.

**Recommendation**: Deferred to post-Phase 6. The initial release targets glibc (standard for Ubuntu/Debian/CentOS).

---

## APPENDIX A: Cargo.toml

```toml
[package]
name = "komari-agent"
version = "0.1.0"
edition = "2021"
description = "Komari monitoring agent — Rust rewrite"
license = "MIT"
repository = "https://github.com/DeliciousBuding/komari-agent-rs"

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"

[dependencies]
# CLI
clap = { version = "4", features = ["derive", "env"] }

# TLS
rustls = { version = "0.23", default-features = false, features = ["tls12", "ring"] }
webpki-roots = "0.26"

# WebSocket
tungstenite = { version = "0.24", default-features = false, features = ["handshake", "deflate"] }

# Logging
log = "0.4"
env_logger = "0.11"

# Platform-specific
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_System_Registry",
    "Win32_System_SystemInformation",
    "Win32_System_Performance",
    "Win32_System_Threading",
    "Win32_System_Console",
    "Win32_Networking_WinSock",
    "Win32_NetworkManagement_IpHelper",
    "Win32_NetworkManagement_Ndis",
    "Win32_Graphics_Dxgi",
    "Win32_UI_Notifications",
    "Win32_UI_WindowsAndMessaging",
    "Win32_Storage_FileSystem",
    "Win32_Security",
    "Win32_Foundation",
] }

# For parsing incoming JSON (infrequent, not hot path)
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

## APPENDIX B: Complete Test Vectors

### B.1 SHA-1 Test Vector (RFC 6455 WebSocket Accept)

```
Input:
  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==
  GUID: 258EAFA5-E914-47DA-95CA-C5AB0DC85B11

Expected Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
```

### B.2 Base64 Test Vectors (RFC 4648)

```
""           → ""
"f"          → "Zg=="
"fo"         → "Zm8="
"foo"        → "Zm9v"
"foob"       → "Zm9vYg=="
"fooba"      → "Zm9vYmE="
"foobar"     → "Zm9vYmFy"
```

### B.3 CRC32 Test Vectors

```
""           → 0x00000000
"123456789"  → 0xCBF43926
"\x00"       → 0xD202EF8D
"\xFF"       → 0xFF000000
"hello world" → 0x0D4A1185
```

### B.4 Gzip Round-Trip

```
Input: {"cpu":{"usage":12.5},"ram":{"total":17179869184,"used":8589934592}}
Expected: gunzip produces identical JSON
Overhead: 18 bytes (gzip header + trailer) + DEFLATE block overhead (~10 bits)
```

### B.5 FSM State Transition Table

| State | Event | Next State | Counter |
|---|---|---|---|
| V2WebSocket | v2 success | V2WebSocket | Reset to 0 |
| V2WebSocket | v2 protocol error (1) | V2WebSocket | 1 |
| V2WebSocket | v2 protocol error (2) | V2HttpPost | 2 |
| V2WebSocket | v2 protocol error (3) | V1HttpPost | 3 |
| V2HttpPost | v2 success | V2WebSocket | Reset to 0 |
| V2HttpPost | v2 protocol error | V1HttpPost | 3 |
| V1HttpPost | any success | V2WebSocket | Reset to 0 |
| V1HttpPost | any error | V1HttpPost | 3 (stays) |
| Any (Connected) | disconnect | Disconnected | — |
| Disconnected | reconnect success (v2) | V2WebSocket | Reset to 0 |
| Disconnected | reconnect success (v1) | V1HttpPost | — |
| Disconnected | reconnect (v1 already active) | V1HttpPost | — |

## APPENDIX C: Physical Disk Filter List

The `is_physical_disk()` function must exclude the following mount points/types (matching Go exactly):

```
/dev/loop*, /sys/*, /proc/*, /run/*, /snap/*, /var/lib/docker/*,
overlay, tmpfs, devtmpfs, cgroup, cgroup2, pstore, bpf, debugfs,
tracefs, fusectl, configfs, securityfs, hugetlbfs, devpts, mqueue,
binfmt_misc, squashfs, ramfs, aufs, devfs, autofs, efivarfs, nfs,
nfs4, cifs, smbfs, rpc_pipefs, /var/lib/kubelet/*
```

## APPENDIX D: Built-in DNS Server List (matching Go)

```
[2606:4700:4700::1111]:53   Cloudflare IPv6
[2606:4700:4700::1001]:53   Cloudflare IPv6 (backup)
[2001:4860:4860::8888]:53   Google IPv6
[2001:4860:4860::8844]:53   Google IPv6 (backup)
114.114.114.114:53          114DNS (China mainland)
1.1.1.1:53                  Cloudflare IPv4
8.8.8.8:53                  Google IPv4
8.8.4.4:53                  Google IPv4 (backup)
223.5.5.5:53                AliDNS (China mainland)
119.29.29.29:53             DNSPod (China mainland)
```

---

**Document end.** This architecture reference is the Single Source of Truth for komari-agent-rs implementation. All 13 design documents have been synthesized, conflicts resolved, and specifications unified into this one blueprint.
