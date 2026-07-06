# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.10] - 2026-07-06

### Fixed
- HTTP ping result uploads now send plain JSON instead of gzip-compressed JSON-RPC. The current Komari `/api/clients/v2/rpc` POST compatibility path reads raw request bodies and does not decompress gzip payloads.

## [0.1.9] - 2026-07-06

### Fixed
- HTTP-only agents now poll Komari ping tasks via `/api/clients/ping/tasks`, execute them according to each task's configured interval, and upload results back through JSON-RPC v2 `agent.pingResult`. This restores delay monitoring for deployments that intentionally avoid WebSocket transport.

## [0.1.8] - 2026-07-04

### Fixed
- Linux disk aggregation now deduplicates repeated `statfs(total, used)` samples so overlay/bind mounts of the same root filesystem cannot multiply disk usage.
- CPU usage delta calculation now tolerates counter resets and idle-counter anomalies without unsigned underflow, panic, wraparound, or percentages outside `0..100`.
- Report encoding clamps impossible percentage metrics and guarantees `used <= total` for RAM, swap, disk, and GPU memory before upload.

## [0.1.6] - 2026-06-21

### Added
- **`http://` endpoint support**: the agent now accepts plain-HTTP endpoints (e.g. `http://127.0.0.1:25774`), enabling agent + Komari server on the same host with the server bound to localhost — no TLS needed. Previously the URL parser rejected anything other than `https://`, which forced a public-https round-trip (via Cloudflare/nginx) just to self-monitor a co-located server. Implementation: a `MaybeTls` enum unifies rustls TLS streams and plain `TcpStream` behind a single `Read`+`Write` impl; `parse_https_url` → `parse_url` handles both schemes (default port 80 for `http://`, 443 for `https://`).
- **Auto `--http-only` for `http://`**: plain HTTP cannot do a `wss://` WebSocket upgrade, so an `http://` endpoint automatically forces HTTP POST reporting (with a startup notice). WS-over-plain-HTTP remains unsupported by design — use `https://` + WS, or `http://` + HTTP POST.

### Changed
- `parse_https_url` renamed to `parse_url` (now scheme-aware).

## [0.1.5] - 2026-06-21

### Added
- **`--help` / `-h` and `--version` / `-V`**: the CLI now answers the two commands every user runs first. Previously `--help` was mis-parsed as a value flag (`MissingValue("--help")`) because the hand-rolled parser tried to consume the next argument as its value. Added a meta-flag short-circuit at the top of `parse_args`, plus a categorized `help_text()` covering every option (Required / Connection / Network / Metrics / Behavior / Meta) and an `AGENT_*` environment-variable note. The argument-error path now points users to `--help`.
- Unit tests assert `help_text()` lists every user-facing flag (guards against adding a flag but forgetting the help line) and that `version_text()` carries `CARGO_PKG_VERSION`.

### Fixed
- **Package version was stuck at `0.1.0`** in `Cargo.toml` through the v0.1.1–v0.1.4 releases. Consequence: `--version` reported `0.1.0`, and — more seriously — the self-update check (`CURRENT_VERSION = env!("CARGO_PKG_VERSION")` in `update.rs`) would always treat the running binary as out of date (`0.1.0` < latest GitHub tag), forcing a spurious re-download on every launch for anyone with self-update enabled. Bumped to `0.1.5` to match the release tag; going forward the Cargo version tracks the latest tag.

## [0.1.4] - 2026-06-21

### Added
- **`--http-only` / `AGENT_HTTP_ONLY`**: escape hatch for networks where a DPI / middlebox breaks the WebSocket upgrade (observed behind some cloud middleboxes and TUN proxies — WS handshake returns the SPA `200` instead of `101`). Forces the protocol FSM to start and stay at `HttpV1`: the agent reports over plain HTTP POST and never attempts WS, sidestepping the interference entirely. Wire-compatible — the v1 report endpoint accepts the same payload the WS v1 path sends (verified `status=200 {"status":"success"}`).

### Changed
- `HttpV1` report tick no longer dispatches the bare `{"status":"success"}` ack as a server message (it was logging "unhandled v1 message" every tick); only responses carrying a `method`/`id` field (real task/exec/ping pushes) are dispatched.

### Fixed
- `ProtocolFsm::new` now takes an `http_only` flag (`Default` impl + tests updated).

## [0.1.3] - 2026-06-21

### Documentation & repo hygiene (no binary behavior change)
- **README**: rebuilt the Configuration table from the actual `Config` struct — the old table listed flags and a `KOMARI_*` env prefix that do not exist in the code (real prefix is `AGENT_*`, real flag is `--config`).
- **README**: fixed all download asset names to match the release artifacts (`komari-agent-rs-{platform}-{arch}`, e.g. `komari-agent-rs-linux-x86_64` — the old names 404'd).
- **README**: added upstream references ([`komari-monitor/komari`](https://github.com/komari-monitor/komari) + [`komari-monitor/komari-agent`](https://github.com/komari-monitor/komari-agent)) and switched endpoint examples to `https://`.
- Added `CHANGELOG.md` (Keep a Changelog) and the **MIT `LICENSE` file** — the README badge declared MIT but no file existed.
- Scrubbed operator-specific hostnames and local paths from the public repo (`src/tls.rs` comment, `docs/TESTING.md`, `docs/COMPARISON.md`).

### Tests / CI
- `cargo fmt --check` now passes (it was red-flagging the v0.1.2 commit).
- `net_offsets_read_nonzero_octets` regression test made CI-stable: asserts that ≥1 adapter passes the Up+Connected filter rather than hard-asserting octets > 0, so a momentarily-quiet CI NIC doesn't flake. The core regression (offsets shifted 4 bytes → zero adapters) is still caught.

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

[0.1.10]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.10
[0.1.9]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.9
[0.1.8]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.8
[0.1.6]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.6
[0.1.5]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.5
[0.1.4]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.4
[0.1.3]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.3
[0.1.2]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.2
[0.1.1]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/DeliciousBuding/komari-agent-rs/releases/tag/v0.1.0
