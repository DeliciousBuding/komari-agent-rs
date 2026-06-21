# komari-agent-rs

[![CI](https://github.com/DeliciousBuding/komari-agent-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/DeliciousBuding/komari-agent-rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Latest Release](https://img.shields.io/github/v/release/DeliciousBuding/komari-agent-rs?color=green)](https://github.com/DeliciousBuding/komari-agent-rs/releases/latest)
[![Changelog](https://img.shields.io/badge/changelog-CHANGELOG.md-blue)](CHANGELOG.md)

**Featherweight Komari monitoring agent — sync single-threaded Rust, ~1.5 MB binary (196 KB our code + ~1 MB mandatory TLS stack), &lt;3 MB RSS.**

## Quick Start

```bash
# Download and run (Linux)
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-linux-amd64 -o komari-agent
chmod +x komari-agent
./komari-agent --token YOUR_TOKEN --endpoint wss://your-komari-server/ws
```

One binary. No runtime deps. No OpenSSL.

## Features

- **CPU** — per-core utilization, model name, core count
- **Memory** — total / used / available / swap, 3-mode reporting (raw / with-cache / used-only)
- **Disk** — per-partition total / used / free, physical-device filter
- **Network** — per-interface RX/TX bytes/sec delta, TCP/UDP connection counts
- **GPU** — name / utilization / VRAM / temperature (NVIDIA, AMD ROCm, Intel DRM, Apple Metal, DXGI)
- **Load** — 1/5/15-minute load averages
- **Connections** — TCP4/TCP6/UDP socket counts
- **Processes** — total process count
- **Uptime** — system uptime in seconds
- **IP** — public IP auto-detection
- **OS info** — OS name, kernel version, virtualization detection
- **v1/v2 protocol** — JSON-RPC 2.0 over WebSocket with HTTP POST fallback (JSON-RPC 1.0 compatibility)
- **Remote exec** — execute commands on agent via server
- **ICMP/TCP/HTTP ping** — three-tier ping with automatic fallback
- **Interactive terminal** — PTY (Linux/macOS/FreeBSD) and ConPTY (Windows)
- **Self-update** — fetch and apply latest GitHub Release
- **Cross-platform** — Linux, macOS, Windows, FreeBSD — full feature parity

## Comparison

| | Go Agent | Zig Agent | **komari-agent-rs** |
|---|---|---|---|
| Binary size | ~14 MB | ~1.3 MB | **~1.5 MB** (196 KB our code + ~1 MB TLS stack) |
| Steady-state RSS | ~18-32 MB | ~8.5 MB | **~3 MB** |
| Concurrency | goroutines | async | **sync single-threaded** |
| TLS | crypto/tls | OS-native | **rustls + ring** |
| JSON | encoding/json | std.json | **custom zero-alloc** |
| Gzip | compress/gzip | std.compress | **fixed-Huffman encoder** |
| Async runtime | built-in | built-in | **none — no tokio, no async-std** |
| Build dep | Go toolchain | Zig compiler | **Rust (stable) only** |

**Where the binary goes** (cargo-bloat, Linux musl release, stripped):

| Component | Size | Share |
|---|---:|---:|
| rustls (TLS impl) | ~1.1 MB | 40% |
| ring (crypto primitives) | ~528 KB | 19% |
| webpki (root cert bundle) | ~262 KB | 10% |
| std (Rust stdlib) | ~471 KB | 17% |
| **Our agent code** | **~196 KB** | **7%** |
| Other (misc crates) | ~rest | 7% |

The binary is **TLS-bound**: rustls + ring + webpki (~70%) is the irreducible cost of speaking HTTPS/WSS without OpenSSL. The actual monitoring agent we wrote is **196 KB** — that is the "featherweight" achievement. The operational win that matters on a tiny VPS is **RSS ~3 MB vs the Go agent's 18-32 MB**: an order of magnitude less resident memory for the same job.

## Installation

### Linux

```bash
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-linux-amd64 -o komari-agent
chmod +x komari-agent
sudo mv komari-agent /usr/local/bin/
komari-agent --token YOUR_TOKEN --endpoint wss://your-server/ws
```

### macOS

```bash
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-darwin-amd64 -o komari-agent
chmod +x komari-agent
sudo mv komari-agent /usr/local/bin/
komari-agent --token YOUR_TOKEN --endpoint wss://your-server/ws
```

### Windows

```powershell
Invoke-WebRequest -Uri "https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-windows-amd64.exe" -OutFile "komari-agent.exe"
.\komari-agent.exe --token YOUR_TOKEN --endpoint wss://your-server/ws
```

### FreeBSD

```bash
fetch https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-freebsd-amd64 -o komari-agent
chmod +x komari-agent
mv komari-agent /usr/local/bin/
komari-agent --token YOUR_TOKEN --endpoint wss://your-server/ws
```

## Build from Source

Requires Rust stable (1.75+).

```bash
git clone https://github.com/DeliciousBuding/komari-agent-rs.git
cd komari-agent-rs

# Core build — monitoring + v1/v2 protocol + HTTP fallback
cargo build --release

# Full build — everything enabled
cargo build --release --features full

# Feature-gated builds
cargo build --release --features gpu-detection     # +GPU monitoring
cargo build --release --features terminal           # +Interactive terminal
cargo build --release --features ping               # +ICMP/TCP/HTTP ping
cargo build --release --features self-update        # +Self-update
```

> **Binary size note:** Total stripped Linux musl binary is ~1.5 MB. ~1 MB of that is the mandatory TLS stack (rustls + ring + webpki-roots); our own monitoring code is ~196 KB. Use `cargo bloat --release --crates` to reproduce the breakdown.

## Configuration

| Flag | Env Var | Default | Description |
|---|---|---|---|
| `--token` | `KOMARI_TOKEN` | (required) | Agent authentication token |
| `--endpoint` | `KOMARI_ENDPOINT` | `wss://127.0.0.1/ws` | Komari server WebSocket URL |
| `--interval` | `KOMARI_INTERVAL` | `1` | Metrics reporting interval in seconds |
| `--memory-mode` | `KOMARI_MEMORY_MODE` | `0` | Memory reporting: 0=default, 1=include-cache, 2=raw-used |
| `--hostname` | `KOMARI_HOSTNAME` | auto | Override hostname |
| `--config-file` | `KOMARI_CONFIG_FILE` | — | Path to JSON config file |
| `--timeout` | `KOMARI_TIMEOUT` | `30` | Connection timeout in seconds |
| `--retry` | `KOMARI_RETRY` | `3` | Max retry attempts |
| `--disable-gpu` | `KOMARI_DISABLE_GPU` | `false` | Disable GPU detection |
| `--disable-ping` | `KOMARI_DISABLE_PING` | `false` | Disable ping endpoint |
| `--disable-terminal` | `KOMARI_DISABLE_TERMINAL` | `false` | Disable interactive terminal |
| `--log-level` | `KOMARI_LOG_LEVEL` | `info` | Log verbosity: trace, debug, info, warn, error |

All flags can be set via environment variable or JSON config file. CLI args take highest precedence.

## Architecture

The agent connects to a Komari server via WebSocket (JSON-RPC 2.0), falling back to HTTP POST (JSON-RPC 1.0 compatibility) when WebSocket is unavailable. A 1-second tick loop collects system metrics into a stack-based scratch arena with **zero heap allocation in the hot path**.

```
┌──────────────┐   WebSocket/HTTP   ┌──────────────┐
│ komari-agent │ ◄──────────────────► │ Komari Server │
│   (sync,     │   JSON-RPC 2.0      │              │
│  single-thrd)│   (TLS 1.3 via      │              │
│              │    rustls + ring)    │              │
└──────┬───────┘                     └──────────────┘
       │
       │ 1s tick (zero-alloc)
       │
  ┌────┴────────────────────────────┐
  │  CPU / MEM / DISK / NET / GPU   │
  │  Load / Connections / Processes │
  │  Uptime / IP / OS / Virt        │
  └─────────────────────────────────┘
```

Full blueprint: **[docs/plan/architecture-reference.md](docs/plan/architecture-reference.md)** (2,707 lines, 13 parallel design documents).

## Feature Matrix

| Feature | Our-code delta | Default | Description |
|---|---|---|---|
| Core (monitoring + v1/v2 + HTTP) | ~196 KB (incl. TLS floor ~1 MB) | Yes | Essential metrics collection and reporting |
| `gpu-detection` | +~80 KB | No | GPU name, utilization, VRAM, temperature |
| `terminal` | +~60 KB | No | PTY/ConPTY interactive shell |
| `ping` | +~30 KB | No | ICMP → TCP → HTTP three-tier ping |
| `self-update` | +~15 KB | No | GitHub Release auto-update |
| **`full` (all features)** | **~196 KB agent + ~1 MB TLS** | — | Complete agent, ~1.5 MB stripped total |

> Deltas reflect our own code only. Every build pays a fixed ~1 MB for the rustls+ring+webpki TLS stack regardless of features.

## Design Principles

- **Zero dependencies in the hot path** — no serde, no clap, no flate2, no tokio
- **Custom JSON encoder** (~300 lines) — wire-identical output, zero allocation
- **Custom gzip encoder** (~200 lines) — fixed-Huffman DEFLATE, valid gzip, no decode needed
- **Self-implemented SHA-1 + Base64** (~160 lines) — WebSocket handshake with no crypto crates
- **OS-native TLS root certificates** — `/etc/ssl/certs` on Linux, CryptoAPI on Windows, Security.framework on macOS
- **cfg-gated platform dispatch** — compile-time type selection, no vtable overhead
- **Explicit config passing** — no globals, fully testable without environment setup

## License

MIT — see [LICENSE](LICENSE) for details.
