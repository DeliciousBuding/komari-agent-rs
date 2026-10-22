# Compatibility and support matrix
Last updated: 2026-10-22

This matrix describes this agent's supported surface, not a performance ranking. The wire contract comes from the upstream Komari server, with the Go agent used as a behavior reference.

## Features

| Surface | Support and condition |
|---|---|
| Basic host info and CPU/RAM/swap/load/disk/net/process/uptime | Default build; platform collectors differ |
| v2 report / basicInfo | WebSocket and HTTP POST |
| v2 pull / event ack | HTTP fallback; deduplicated dispatch |
| ICMP/TCP/HTTP ping | Default ping feature; platform/network privileges still apply |
| GPU | gpu-detection feature and runtime enable_gpu; external tooling or platform fallback |
| One-shot exec | Explicit disable_exec=false; separate from WebSSH; bounded execution/output |
| Interactive terminal | terminal feature, disable_web_ssh=false and non-http_only |
| startupConfig | Effective settings and version/features, credentials redacted |
| Self-update | self-update feature and explicit disable_auto_update=false |
| Server-requested switchVersion | self-update/full build plus disable_web_ssh=false and disable_auto_update=false; stable checksum-verified releases only |
| File management | Komari 1.6 list/stat/mutate/search and streaming upload/download; disable_web_ssh=false; bounded workers/chunks |
| Network-test/iperf/trace DTOs | Unsupported; upstream definitions alone do not establish a live contract |
| Auto-discovery enrollment | Shared key obtains and atomically persists a node credential; existing manual tokens remain supported |
| v1 report/basicInfo endpoints | Removed in 0.6; use a prior release with old servers |

Runtime permission checks remain authoritative if a server sends an unadvertised event.

## Platforms and release assets

| Platform | Release assets | Verification |
|---|---|---|
| Linux x86_64 musl | default / full | CI tests, build and size gate |
| Linux aarch64 musl | default / full | Cross-build and size gate; live measurements are deployment-specific |
| Windows x86_64 MSVC | full executable | CI tests, offline installer tests, build and size gate |
| macOS x86_64 / arm64 | full executables | Native release builds and size gates; no PTY/hardware claim from cross-build alone |
| FreeBSD x86_64 | full executable | Cross-build; native runtime validation remains a separate requirement |

The release workflow is the source of truth for current target names. A compiled asset does not establish every optional hardware collector or terminal behavior on that OS.

## Resource measurements

Release CI enforces the configured 2 MiB binary-size gate where supported. Memory must be measured with the same release/features, OS, trust-store size and traffic. This project does not publish old estimates as current RSS benchmarks or compare unmeasured third-party implementations.
