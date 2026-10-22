# Changelog

## v0.6.0 (2026-10-22)

### Compatibility changes
- Target Komari 1.5+ v2 JSON-RPC only. WebSocket failure falls back to v2 HTTP;
  removed v1 endpoints are never called. Explicit protocol_version=1 is rejected
  with a migration error. Keep a prior release for pre-v2 servers.
- Default reporting interval is 3 seconds, following the Go agent. Explicit
  existing interval settings remain effective. Control polling and heartbeats
  are independent of a long reporting interval.
- Follow Go NIC prefix/include/exclude priority and whole-name glob patterns.
  Windows filters now match adapter aliases rather than description substrings;
  migrate existing filters. Inactive counters and non-Go-excluded virtual NICs
  are no longer implicitly removed; configure explicit exclusions if needed.
- Komari 1.5.1 requires reports within its 11-second WS deadline; long intervals
  may fall back to HTTP. Komari 1.6.0 supports independent Ping-based keepalive.

### Reliability and platform fixes
- Decode nested task parameters and strict JSON escapes through a shared parser.
  Validate successful HTTP status and the JSON-RPC envelope before accepting a
  response; stale HTTP pull sessions cannot dispatch late events.
- Share bounded subprocess management across exec and metric collectors. Unix
  process groups and Windows Job Objects own ordinary descendants; a 30-second
  exec deadline and 256 KiB shared output cap prevent pipe/deadline deadlocks.
- Bound HTTP, WebSocket and inflated control messages to 1 MiB. Check response
  framing, preserve IPv6/port authorities and avoid reflecting private payloads.
- Preserve partially read WebSocket frames across socket timeouts, distinguish
  clean EOF from reset/truncation, and use the actual package version in Upgrade.
- Save monthly counters through a synced sibling file and atomic replacement;
  failed saves preserve old state. Reject malformed persisted months/counters.
- Bound JSON configuration to 1 MiB and 32 validation levels; reject duplicate
  settings/trailing data, preserve Unicode and redact values from parse errors.
- Cache successful Linux CPU identification, identify ARM cores, count physical
  topology correctly and avoid double-counting Linux guest CPU time.
- Preserve root overlay/loop filesystems, deduplicate devices/ZFS pools by their
  largest capacity and filter network filesystems. Read AMD sysfs utilization,
  VRAM and temperature when command-line tooling is unavailable.
- Verify exact release checksums before installation. Preserve existing config,
  keep credentials out of Windows task arguments, and retain a rollback binary.
  Optional self-update handles API formatting/HTTPS redirects safely, strips
  authentication on origin changes and rolls back failed replacement/re-exec.
  Windows reports that an installed update needs a supervisor restart.

### Community and documentation
- Thanks to Hongquan Zou ([IShinji](https://github.com/IShinji)) for the reviewed
  contributions in #105 through #113. Integrated fixes extend those patches with
  failure-path tests, configuration preservation and resource limits.
- Align macOS C/Rust deployment targets and cache keys; run offline Windows
  installer regressions. Publish current architecture, compatibility and testing
  boundaries; historical planning entries no longer claim runtime authority.

The security fixes below were prepared as 0.5.1 source changes and are included
in 0.6.0; no separate 0.5.1 release assets were published.

### Fixed
- Update locked `rustls` to 0.23.46 for RUSTSEC-2026-0346 through
  RUSTSEC-2026-0349 (session-secret Debug redaction, ECH rejection ordering,
  Brotli decompression bounds and unauthenticated key-share prediction).
  Advisory applicability depends on enabled features and client configuration;
  this release does not enable ECH, Brotli or post-quantum providers.
- Correct Unix terminal FFI signatures for current Rust compilers (community
  contribution by [IShinji](https://github.com/IShinji),
  [#107](https://github.com/DeliciousBuding/komari-agent-rs/pull/107)).
- Update locked `rustls-webpki` to 0.103.16, addressing
  [RUSTSEC-2026-0382 / CVE-2026-111943](https://rustsec.org/advisories/RUSTSEC-2026-0382.html).
  Certificate paths containing distrusted emSign intermediate public keys are
  rejected even when the operating system's trust anchors permit the path.
- Windows network collection filters NDIS filter-interface rows before applying
  its bounded interface capacity, so filter rows cannot crowd out real adapters.
  Rate snapshots use interface LUIDs rather than truncated descriptions, fixing
  missing or ambiguous baselines for long adapter names. Display names are
  truncated only at UTF-8 character boundaries.

### Verification
- Windows CI now runs the test suite, including synthetic filter-saturation,
  long-name/rate and FFI-layout regressions.
- CI and release builds use `--locked` to build the reviewed dependency graph.

### Upgrade notes
- Restart the agent after replacing the binary. Native system trust roots are
  loaded at process startup; changing the operating system CA store alone does
  not update an already-running process.
- Keep `ignore_unsafe_cert=false` (the default). Explicitly disabling certificate
  verification bypasses these protections. No configuration migration is needed.

## v0.5.0 (2026-09-23)

Follows the upstream server's post-1.5.0-fix1 agent-control additions
(`komari-monitor/komari@main`, snapshot track): `agent.startupConfig` and
`agent.switchVersion`. Both are inert against servers <= 1.5.0-fix1, which
never emit these events, so this release is safe to deploy ahead of the next
server upgrade.

### Added
- **`agent.startupConfig` support** (capability `startup_config`): answers
  the server's admin-initiated request with a snapshot of every effective
  config field plus `version`/`features` build stamps. The result returns as
  an `agent.startupConfig.result` **request** (id echoes the server-assigned
  `request_id`) over the authenticated POST transport for both WS and pull
  modes, same as the upstream Go agent.
- **`agent.switchVersion` policy log**: the event is recognised and logged as
  ignored by policy, instead of the generic "unhandled method" line.

### Deliberate deviations from upstream
- **Secrets are redacted in the startup-config snapshot**: `token` and
  `auto_discovery_key` are reported as `"[REDACTED]"` (empty stays empty, so
  set/unset state remains auditable). The Go agent uploads the full config
  including credentials; a fleet audit surface must not expand credential
  exposure to the server process and admin panel responses.
- **`agent.switchVersion` is permanently unsupported**: remote version
  switching conflicts with `disable_auto_update` + SHA-256-pinned manual
  rollout. The `switch_version` capability is not advertised.

### Notes
- Protocol spec source of truth is the server repo's
  `protocol/v2/jsonrpc.go`; the `komari-monitor/komari-protocol` contract
  repo has not moved since 2026-08-04 and no longer reflects the live wire.

## v0.4.0 (2026-09-18)

Komari 1.5.0 compatibility release. Komari 1.5.0 removed every v1 agent
endpoint (`POST /api/clients/report`, `uploadBasicInfo`, `task/result`,
`ping/*`); only the v2 JSON-RPC transport (`/api/clients/v2/rpc`, WS or POST)
remains. v0.3.0 agents against 1.5.0 lost all reporting (fleet blackout on
2026-09-18, root-caused below).

### Fixed
- **`agent.report` params are now wrapped in `{"report": ...}`** — the server
  binds params into `ReportParams{report}`; the bare flat report got a 200
  but ingested nothing (silent data loss in all v2 modes).
- **v2 ping/task-result params use snake_case** (`task_id`/`ping_type`/
  `finished_at`), matching the server's `PingResultParams`/`TaskResultParams`
  (camelCase was silently dropped).
- **FSM downgrade now works when connect succeeds but the session dies
  instantly**: counters only reset after a session survives 30 s
  (`MIN_HEALTHY_SESSION`). Previously a middlebox/server closing the WS right
  after upgrade caused infinite flapping in WsV2 with no downgrade.
- **Terminal-mode self-healing**: 3 consecutive HttpV1 failures (e.g. v1
  endpoint removed by a server upgrade) escalate back to the preferred mode
  instead of retrying a dead endpoint forever; fallback modes are also
  re-probed every 10 min.
- **HttpV2 tick checks HTTP status** (404 → downgrade path, other non-200 →
  tick failure); HttpV1 detects 404 (`v1 endpoint gone`) for escalation.

### Added
- **HTTP v2 pull thread** (`agent.pull` long-poll, 35 s timeout): HTTP-mode
  agents now receive server-pushed events (ping/exec/terminal) in near real
  time. Required for Komari 1.5.0, where `GET /api/clients/ping/tasks` is
  gone and ping events carry a 3 s TTL. Mirrors the Go agent's
  `runV2PullLoop` goroutine; thread lifecycle is owned by a generation
  counter so stale pulls exit on mode change.
- **v2 event bus**: dedup by event id + idempotent re-ack via
  `ack_event_ids` on report/pull, shared between tick loop and pull thread.
- Report responses' piggybacked events are dispatched (exec/task results use
  `agent.taskResult` over v2 when in v2 mode).
- `http_post_timeout` (configurable read/write timeout; default stays 30 s).

### Changed
- **`http_only` / plain-`http://` endpoints now start at HttpV2** (was
  HttpV1). Loopback deployments (e.g. agent co-located with the server on
  `http://127.0.0.1`) no longer pin themselves to the legacy protocol.
- Basic-info upload tries v2 first in all modes (was v1-only in http_only).
## v0.3.0 (2026-08-29)

### Fixed
- **`default` build now includes `ping`** — ping is a core monitoring feature;
  shipping it only in `full` let fleet installs of the `default` asset silently
  report 100% packet loss in Komari while the network was fine. Kept heavy
  extras (gpu-detection/terminal/self-update) behind `full`.
- **Net collector self-checks**: zero eligible interfaces (e.g. a misconfigured
  `include_nics` that matches no real NIC) now logs a one-shot WARN naming the
  filter, instead of silently reporting all-zero net metrics.
- **`--version` prints features** (`komari-agent-rs 0.3.0 (features: ping)`),
  so a deployment's capabilities are inspectable without touching the binary.
- **Asset naming unified**: Linux arm64 now publishes both
  `linux-arm64-default` and `linux-arm64-full` (previously one bare
  `linux-arm64` full build), matching the x86_64 `-default`/`-full` pair.
- **`install.sh` asset mapping fixed**: it previously requested
  `komari-agent-rs-linux-amd64` — a name no release ever published — and now
  maps to the actual `-full` assets (Linux) / platform assets (macOS/FreeBSD),
  and prints the installed `--version` after installation.

## v0.2.2 (2026-08-15)

### Added
- **Windows NVIDIA detailed GPU metrics**: `nvidia-smi` CSV fallback fills
  per-GPU utilization, temperature, and used VRAM that DXGI cannot report
  (mirrors upstream komari-agent-go `e5aefd4f`; DXGI remains the fallback)
- `install.sh --user`: per-user systemd user service (non-root, `systemctl --user`
  + `loginctl enable-linger`, XDG paths)

### Changed
- **Drop Cloudflare Access credentials** (`cf_access_client_id` /
  `cf_access_client_secret`, `--cf-access-*`, `AGENT_CF_ACCESS_*`): the agent
  no longer holds a long-lived CF Access service token. Aligns with upstream
  komari-agent-go `8cd92149`; deployments use CF Access edge bypass instead.
- Release workflow rebuilt as a single matrix job (parallel per-target builds),
  added `Swatinem/rust-cache@v2`, and fixed macOS x86_64 to build on the Intel
  `macos-13` runner (previously mislabeled arm64 output as x86_64)
- CI workflow: added `rust-cache@v2` and `fail-fast: false`

## v0.2.1 (2026-07-15)

### Fixed
- **permessage-deflate root cause**: inflate appends gorilla/websocket trailer (`00 00 FF FF` + empty final stored block `01 00 00 FF FF`) so real server frames no longer UnexpectedEof
- Interactive terminal end-to-end when built with `--features terminal` / `full`
- WebSocket upgrade query separator for paths that already have `?id=`
- `disable_exec` is independent of `disable_web_ssh` (no JSON mirror)

### Changed
- Terminal: max 2 concurrent sessions; 30min idle timeout
- Release workflow emits **linux-musl default** (no terminal) and **full** assets for fleet vs fire-axe
- Deflate inflate failure auto-disables compression as safety net

### Notes
- Default build still has no `terminal` feature; `disable_web_ssh` / `disable_exec` default **true**
- prod E2E WebSSH verified 2026-07-15 with compression ON

## v0.2.0 (2026-07-14)

### Added
- GPU detailed metrics: utilization (%), temperature (°C), vendor/device IDs (#62)
- High-latency ping retry: >1000ms auto-retries 3x, TCP retransmission detection (#63)
- Virtual GPU filtering: exclude virtio/vmware/qxl/bochs/cirrus/hyperv etc. (#64)
- Container detection: podman (/.containerenv), LXC (/dev/.lxc-boot-id), precise cgroup matching (#65)
- GPU driver name mapping: i915→"Intel", amdgpu→"AMD GPU", etc. (#66)
- nvidia-smi/rocm-smi binary path detection with fallback (#67)
- Memory accuracy: shmem in htop-like mode, SwapCached subtraction, Zswap fields (#90)
- Network accuracy: counter wraparound clamp, connection counting ss/netstat fallback (#91)
- Subprocess timeout: all 18 Command::output() calls now bounded to 30s (#92)
- Self-update: asset name alignment, atomic Windows replace, GITHUB_TOKEN support (#93)
- Disk accuracy: mountpoint prefix exclusion, ZFS dataset dedup, fuseblk/loop handling (#94)
- IP detection: HTTPS support, 3 additional endpoints, forced IPv4/IPv6 binding (#95)
- CI/CD pipeline with feature matrix testing (#97)

### Fixed
- Task ID validation: reject negative IDs before u64 cast (#96)
- DEFLATE bomb protection: 64MB decompression output cap (#98)
- /dev/urandom graceful fallback instead of panic (#98)
- Virtual network interface filter: cni/podman/flannel/vmbr/fwbr/fwpr (#98)

### Changed
- Container detection bare-metal returns "none" instead of empty string

## v0.1.10 (2026-06-20)
- Initial Rust rewrite baseline
- Full Go feature parity: v2 protocol FSM, GPU detection, terminal, ping, self-update
- 4-platform support: Linux, Windows, macOS, FreeBSD
- Binary ~1.5MB, RSS ~3MB
