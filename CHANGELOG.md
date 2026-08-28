# Changelog

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
