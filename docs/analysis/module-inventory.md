# Komari Agent Go-to-Rust: Module Inventory

**Generated**: 2026-06-20
**Source codebase**: `D:/Code/Projects/external/komari-agent-go`
**Target**: `D:/Code/Projects/edgehub/komari-agent-rs`
**Total non-test Go source**: ~6,555 lines across 46 files (+ ~834 test lines across 10 files)

---

## 1. Module Summary Table

| Module | Responsibility | Internal Deps | External Deps | Files | Lines | Complexity | S.U.P.E.R |
|---|---|---|---|---|---|---|---|
| `main.go` | Entry point | cmd | None | 1 | 13 | Low | A |
| `cmd/` | CLI, flags, autodiscovery, subcommands | cmd/flags, dnsresolver, monitoring/*, server, update, utils | spf13/cobra, go-ole, golang.org/x/sys, gopkg.in/toast.v1 | 7 | 992 | High | C |
| `monitoring/` | Core data collection (cpu/mem/disk/net/gpu/ip/load/proc/uptime/virt) | cmd/flags, dnsresolver, monitoring/netstatic, utils | gopsutil/v4 (cpu,mem,disk,load,net,host), cpuid/v2, golang.org/x/sys | 27 | 4,366 | Critical | C |
| `server/` | WebSocket, basicInfo, protocol fallback, task exec, ping | monitoring, dnsresolver, protocol/*, terminal, update, utils, ws, cmd/flags | gorilla/websocket, pro-bing | 4 | 1,586 | Critical | C |
| `protocol/` | JSON-RPC v2, v1 report, gzip | protocol/v1 | None | 3 | 134 | Low | A |
| `terminal/` | Interactive WebSocket shell (PTY/ConPTY) | cmd/flags | gorilla/websocket, creack/pty, UserExistsError/conpty | 3 | 444 | High | B |
| `update/` | Self-update via GitHub releases | dnsresolver | rhysd/go-github-selfupdate, blang/semver | 1 | 155 | Medium | B |
| `dnsresolver/` | Custom DNS, IPv4/IPv6 preference, dialer factory | cmd/flags | None (std lib only) | 1 | 326 | Medium | B |
| `ws/` | Thread-safe WebSocket wrapper | None | gorilla/websocket | 1 | 58 | Low | A |
| `utils/` | IDN conversion, date helpers | None | golang.org/x/net/idna | 2 | 147 | Low | A |
> S.U.P.E.R: A=Excellent, B=Good, C=Needs Work, D=Problematic

---

## 2. Module Deep Dives

---

### 2.1 `main.go`

**Path**: `D:/Code/Projects/external/komari-agent-go/main.go` (13 lines)

**Responsibility**: Single-file entry point. Calls `cmd.Execute()` and exits with code 0.

**Public API**: None. Only `func main()`.

**Complexity**: Low (5 executable lines, pure delegation)

**S.U.P.E.R Assessment**:
- S: Perfect. One job: bootstrap.
- U: N/A.
- P: N/A.
- E: Fully environment-agnostic.
- R: Trivial to replace.

**Transformation Notes**: In Rust, `main.rs` will also be a thin entry point.

---

### 2.2 `cmd/` Module

**Path**: `D:/Code/Projects/external/komari-agent-go/cmd/`

**Files**: flags/flag.go (38), root.go (232), autodiscovery.go (202), checkMem.go (55), listDisk.go (42), warn.go (14), warn_windows.go (409)

**Exported**: `Config` struct (34 fields with json + env tags), `GlobalConfig *Config`, `Execute()`, `handleAutoDiscovery()`, Windows toast helpers.

**Internal Deps**: cmd/flags, dnsresolver, monitoring/netstatic, monitoring/unit, server, update, utils

**External Deps**: spf13/cobra, spf13/pflag, go-ole, golang.org/x/sys/windows, gopkg.in/toast.v1

**Complexity**: High
- 100-line RunE mixing signal handling, init, auto-update, main loop.
- warn_windows.go uses raw Win32 via syscall.SyscallN with manual COM vtables.

**S.U.P.E.R**: C -- violates S (mixes CLI + init + Windows toast), U (GlobalConfig imported everywhere), E (hardcoded Win32 calls), R (cobra lock-in).

**Transformation Notes**:
- Replace cobra with clap (derive parser).
- Replace GlobalConfig with explicit `Config` parameter passing.
- `loadFromEnv()` env tag reflection -> clap derive with env attribute.
- Windows toast: `#[cfg(windows)]` module with `windows-rs` for Win32 COM/WTS API.
- `os.Exit(42)` -> return exit code from `main()`.

---

### 2.3 `monitoring/` Module

**Path**: `D:/Code/Projects/external/komari-agent-go/monitoring/`

**Sub-modules**: monitoring/monitoring.go (orchestrator), monitoring/unit/ (collectors), monitoring/netstatic/ (traffic history)

**Key exported types**: `CpuInfo`, `RamInfo`, `ProcMemInfo`, `DiskInfo`, `LoadInfo`, `DetailedGPUInfo`, `NetStatic`, `TrafficData`

**Key exported functions (20+)**: Cpu, Ram (3-mode dispatch), Swap, Disk, Load, NetworkSpeed, ConnectionsCount, InterfaceList, GetIPAddress, GetIPv4/v6Address, ProcessCount, Uptime, GpuName, GetDetailedGPUInfo, Virtualized, OSName, KernelVersion

Plus netstatic API: StartOrContinue, Stop, Clear, SetNewConfig, GetNetStatic, GetTotalTraffic, Get*Between variants

**Platform dispatch (build tags)**:
| Collector | linux | windows | darwin | freebsd |
|---|---|---|---|---|
| ProcessCount | /proc dir walk | K32EnumProcesses | ps -A | ps -ax |
| GpuName | lspci + sysfs DRM | DXGI COM | system_profiler | pciconf |
| OSName | /etc/os-release+heuristics | Registry | sw_vers | uname |
| KernelVersion | uname -r | Registry+UBR | uname -r | uname -r |
| DetailedGPU | nvidia-smi/rocm-smi | stubbed | stubbed | stubbed |

**Complexity**: Critical (27 files, 4366 lines, largest module)
- isPhysicalDisk() with 30+ hardcoded mount excludes
- networkSpeed() 1-second sleep sampling
- Windows GPU via raw COM vtables
- Three memory calculation modes with per-OS branching
- OS detection for Android/Synology/PVE/fnOS

**S.U.P.E.R**: C -- S violated (too many collectors flat), U cycle (monitoring -> unit -> netstatic -> monitoring), E severe (/proc, exec free -b, Android build.prop, hardcoded GPU tool paths), R hurt by GlobalConfig.

**Transformation Notes**:
- **Most challenging port.** No gopsutil equivalent in Rust. Options: sysinfo crate + manual /proc/Win32 API/sysctl.
- Ram() 3-mode dispatch must match exactly for compat.
- isPhysicalDisk() filter lists MUST port identically.
- GPU: exec nvidia-smi/rocm-smi (Linux), DXGI via windows-rs (Windows), system_profiler (macOS).
- netstatic: file-based `VecDeque<TrafficData>` for time-bounded persistence.
- All GlobalConfig access -> explicit `&Config` parameter.

---

### 2.4 `server/` Module

**Files**: websocket.go (526), basicInfo.go (155), task.go (416), protocol_fallback.go (160)

**Exported Funcs**: `EstablishWebSocketConnection()`, `UpdateBasicInfo()`, `DoUploadBasicInfoWorks()`, `NewTask()`, `NewPingTask()`

**Complexity**: Critical -- reconnection loop, 3-strike protocol fallback, POST fallback mode, PowerShell/sh task exec, ICMP/TCP/HTTP ping.

**Transformation Notes**:
- Replace gorilla/websocket with tungstenite (sync mode)
- Reconnection loop MUST match dataTicker/heartbeatTicker/readDone select pattern
- gzipBytes() DUPLICATED in task.go and protocol/transport/ -- unify
- ICMP: CAP_NET_RAW on Linux, admin on Windows

---

### 2.5 `protocol/` Module

Pure JSON-RPC 2.0 types. 11 method constants. Gzip helper. Low complexity.

**Transformation**: Direct serde mapping. interface{} -> serde_json::Value. Gzip -> flate2.

---

### 2.6 `terminal/` Module

PTY/ConPTY bridge over WebSocket. Terminal interface (Close/Read/Write/Resize/Wait). High complexity.

**Transformation**: No Rust PTY equivalent. Need portable-pty or raw posix_openpt (Unix) / CreatePseudoConsole (Windows). Graceful shutdown sequence must match.

---

### 2.7 `update/` Module

Self-update via GitHub releases. os.Exit(42) signals service manager restart. Medium complexity.

**Transformation**: self_update crate. Preserve exit code 42.

---

### 2.8 `dnsresolver/` Module

Custom DNS with IPv4/IPv6 preference, HTTP client caching, 10 built-in DNS servers. 327 lines. Medium complexity.

**Transformation**: No Rust net.Resolver equivalent. Must implement custom DNS resolver (trust-dns-resolver or manual UDP).

---

### 2.9 `ws/` Module

Thread-safe WebSocket wrapper (mutex on writes, not reads). 59 lines. Low complexity.

**Transformation**: Mutex unneeded in sync single-threaded Rust. Use tungstenite::WebSocket directly.

---

### 2.10 `utils/` Module

IDN conversion (ConvertIDNToASCII, ConvertHostToASCII) + date utility (GetLastResetDate). 147 lines. Low complexity.

**Transformation**: idna crate for IDN, chrono for date math.

---


## 3. Cross-Cutting Concerns

### 3.1 Global Config Antipattern

The `cmd/flags/flag.go` module defines `GlobalConfig *Config` imported by EVERY major module (monitoring, server, dnsresolver, terminal). Impact: hard to unit test. Fix: pass `Config` explicitly.

### 3.2 Protocol Version State Machine

3-strike counter: WebSocket attempt -> success -> HTTP error ++failures -> at 3, fall back to v1. Success resets counter.

### 3.3 Duplicated Gzip

`gzipBytes()` is duplicated in `server/task.go` and `protocol/transport/compress.go`. Unify in Rust.

### 3.4 Hardcoded Platform Paths

| Path | File | Purpose |
|---|---|---|
| /proc/cpuinfo | cpu.go | CPU name |
| /proc/meminfo | mem.go | Memory |
| /proc/self/cgroup | virtualization.go | Container detection |
| /etc/os-release | os_linux.go | OS name |
| /etc/passwd | terminal_unix.go | User shell |
| /usr/bin/nvidia-smi | gpu_detailed_linux.go | NVIDIA GPU |
| /opt/rocm/bin/rocm-smi | gpu_detailed_linux.go | AMD GPU |
| ./net_static.json | netstatic/static.go | Traffic history |
| auto-discovery.json | cmd/autodiscovery.go | Auto-discovery config |

### 3.5 Circular Dependency

`monitoring/monitoring.go` -> `monitoring/unit` -> `monitoring/netstatic` -> monitoring. In Rust, submodules of one crate naturally avoid this.

---

## 4. Complexity Summary for Rust Rewrite

| Aspect | Difficulty | Notes |
|---|---|---|
| CLI + Config | Low | clap replaces cobra. Eliminate global config. |
| Protocol types | Low | Direct serde mapping. |
| DNS resolver | Medium | No net.Resolver equivalent. Custom impl needed. |
| WebSocket wrapper | Low | tungstenite direct. |
| Terminal | High | No Rust PTY library. Raw posix_openpt / CreatePseudoConsole. |
| Self-update | Medium | self_update crate. |
| Data collection | Critical | 27 files, 4366 lines. Replace gopsutil. |
| Server + protocol | High | Reconnection loop with exact state machine. |
| IDN utils | Low | idna crate. |

**Estimated Rust modules**: 12 (vs 9 in Go)
**Estimated Rust lines**: ~6000-7000 non-test

---

## 5. Module Dependency Graph (Go)

```
main.go
  +-- cmd/
        +-- cmd/flags       <-- imported by EVERYTHING (GlobalConfig hub)
        +-- dnsresolver     <-- imported by: cmd, monitoring/unit, server, update
        +-- monitoring/unit <-- imported by: cmd, monitoring, server
        +-- monitoring/netstatic <-- imported by: cmd, monitoring/unit
        +-- monitoring      <-- imported by: server
        +-- server          <-- imported by: cmd
        +-- update          <-- imported by: cmd, server
        +-- utils           <-- imported by: cmd, server, monitoring/unit
        +-- ws              <-- imported by: server
        +-- terminal        <-- imported by: server
```

Key insight: `cmd/flags` is the dependency hub. Breaking it into explicit parameter passing is the single most impactful architectural improvement.

---

## 6. Conclusion

The Go codebase is well-structured for a 1:1 Rust rewrite. Top challenges ranked:

1. **gopsutil replacement** -- no Rust equivalent, requires manual /proc/ sysfs/ Win32 API
2. **Global Config antipattern** -- architectural change to explicit parameter passing
3. **Terminal PTY** -- platform-specific syscall access
4. **DNS resolver** -- Go `net.Resolver` has no Rust equivalent
5. **Protocol fallback state machine** -- subtle reconnection edge cases

S.U.P.E.R scores:
- **Clean (A)**: protocol/, ws/, utils/ -- ready for direct Rust mapping
- **Good (B)**: terminal/, update/, dnsresolver/ -- manageable library replacement
- **Needs work (C)**: monitoring/, server/, cmd/ -- most complex, architectural changes needed

**Recommended Rust module structure**:

```
src/
  main.rs              -- entry point
  config.rs            -- CLI flags + Config struct
  cli.rs               -- argument parsing + subcommands
  autodiscovery.rs     -- auto-registration
  monitor/
    mod.rs             -- report builder
    cpu.rs             -- CPU metrics
    memory.rs          -- RAM + swap (3 modes)
    disk.rs            -- disk usage
    network.rs         -- network speed + connections
    load.rs            -- load average
    process.rs         -- process count (platform dispatch)
    uptime.rs          -- system uptime
    gpu.rs             -- GPU (nvidia-smi/rocm-smi/DXGI)
    ip.rs              -- external IP detection
    virtualization.rs  -- VM/container detection
    os.rs              -- OS name + kernel version
    netstatic.rs       -- persistent traffic history
  server/
    mod.rs             -- reconnection loop
    websocket.rs       -- WS connect + message handling
    basic_info.rs      -- periodic HW info upload
    task.rs            -- remote command execution + ping
    protocol.rs        -- v1/v2 fallback state machine
  rpc/
    v2.rs              -- JSON-RPC 2.0 types + builders
    v1.rs              -- v1 type alias
    compress.rs        -- gzip helper
  terminal/
    mod.rs             -- Terminal trait + StartTerminal
    unix.rs            -- Unix PTY
    windows.rs         -- Windows ConPTY
  update.rs            -- self-update
  dns.rs               -- custom DNS resolver
  utils.rs             -- IDN + date helpers
```
