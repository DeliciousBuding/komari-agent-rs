# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - 2026-06-21

### Fixed
- **Windows network metrics (`net.up` / `net.down` / `totalUp` / `totalDown`) reported `0`** even on hosts with heavy traffic. Root cause was a misaligned hand-rolled `MIB_IF_ROW2` FFI struct — two compounding bugs:
  - `InterfaceGuid` is 4-byte aligned (its first member is a `ULONG`), so it sits at offset `0x00C`, **not** `0x010`. The struct wrongly assumed 8-byte GUID alignment, shifting every subsequent field by 4 bytes so the Up+Connected+non-loopback filter rejected all adapters.
  - Missing `OutUcastPkts..OutQLen` tail padding made `sizeof(MIB_IF_ROW2) == 1296` instead of the SDK's `1352 (0x548)`, so the row stride was wrong and every adapter after the first was read from a misaligned address.
  - Offsets re-derived empirically via an `offset_of!` probe against the `microsoft/windows-rs` `MIB_IF_ROW2` `#[repr(C)]` definition and cross-checked against `netioapi.h`.
- Added a host-gated regression test `net_offsets_read_nonzero_octets` that asserts nonzero cumulative octets on an active adapter.

## [0.1.1] - 2026-06-21

### Fixed
- **WebSocket handshake returned the SPA `200` instead of `101 Switching Protocols`** against nginx-fronted Komari servers. nginx serves HTTP/2 by default and silently drops the RFC 6455 `Upgrade` header over h2; rustls negotiated no ALPN and got bumped to h2. Fix: pin ALPN to `["http/1.1"]` in both `rustls::ClientConfig` paths (parity with Go's `gorilla/websocket`).
- **`basicInfo` upload returned HTTP 404** on Komari forks (e.g. v1.1.9) that lack the `/api/clients/v2/rpc` HTTP route. `update_basic_info` now descends a protocol ladder (v2→v1) × payload ladder (full→compat), mirroring the WS FSM's `WsV2→WsV1` downshift.

## [0.1.0] - 2026-06-21

### Added
- Initial public release. Pure-stdlib Rust (only external dependencies: `rustls` + `ring`), synchronous single-threaded, edition 2024.
- Feature-complete rewrite of the Go `komari-agent`, targeting functional parity:
  - **6 platforms**: Linux (x86_64 / aarch64), Windows, macOS (x86_64 / aarch64), FreeBSD. Release binaries under 1.6 MB.
  - **Collectors**: CPU, Memory, Disk, Network, GPU, Load average, TCP/UDP connection counts, Process count, Uptime, Public IP, OS/kernel/virtualization.
  - **Unified `Dialer`**: `HTTPS_PROXY` HTTP CONNECT + `SOCKS5` / `SOCKS5h`, `NO_PROXY` bypass (domain / wildcard / CIDR / IP), `--custom-dns`, `--prefer-ip-version`, Happy Eyeballs (staggered A/AAAA).
  - **Protocol FSM**: JSON-RPC 2.0 over WebSocket with `WsV2 → WsV1 → HttpV2 → HttpV1` automatic downgrade and HTTP POST fallback (JSON-RPC 1.0 compatibility for older forks).
  - **Server tasks**: remote command exec (`sh -s` / `powershell`), task/result upload, ICMP/TCP/HTTP three-tier ping (feature-gated), interactive terminal via PTY (Linux/macOS/FreeBSD) and ConPTY (Windows) (feature-gated), self-update from GitHub Releases (feature-gated).
  - **Compression**: gzip for HTTP POST reports, `permessage-deflate` (RFC 7692) for WebSocket.
- Memory footprint roughly 10× smaller than the Go agent (~3 MB vs 18–32 MB RSS on Linux).

[0.1.2]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.2
[0.1.1]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.0
