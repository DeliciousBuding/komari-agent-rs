# Changelog

## Unreleased

### Fixed
- **permessage-deflate root cause**: inflate now appends gorilla/websocket trailer (`00 00 FF FF` + empty final stored block `01 00 00 FF FF`). RFC-only trailer left `inflate_raw` / Go flate at UnexpectedEof on real server frames.
- Wire interactive terminal end-to-end when built with `--features terminal`
- WebSocket upgrade query separator for paths that already have `?id=`

### Changed
- `disable_exec` is independent of `disable_web_ssh` (JSON no longer mirrors web_ssh → exec)
- Terminal: max 2 concurrent sessions; 30min idle timeout
- Sensitive ops on server require 2FA enrollment (tokendance-komari)

### Notes
- Default build still has no `terminal` feature; `disable_web_ssh` / `disable_exec` default **true**
- Auto-degrade on deflate failure remains as safety net; primary path is correct inflate


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
