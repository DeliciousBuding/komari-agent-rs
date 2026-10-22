# Upstream tracking and open work
Last updated: 2026-10-22

## Reviewed references

| Project | Stable | Reviewed main |
|---|---|---|
| Go agent | 1.5.11, c7bafb79b1a73ed9e14e2c248ae4d4163e506036 | 828afafe7fb5b1b58c1232094e1e46320719016e |
| Komari server | 1.6.0, 1b76b7ad5bc3de6529bb7f0577069b4ac241fec3 | c24c4b9c7c1216d90a8cda82d7d556613dc4d128 |

The server's `protocol/v2` and its dispatcher/handlers define the active wire contract. A standalone protocol repository or an unused DTO does not establish an implemented feature.

## Decisions through 0.7

- Follow v2 WS/HTTP reporting, events and acknowledgements; stop calling v1 routes removed by current servers.
- Follow the Go default 3-second report interval while preserving explicit configuration.
- Align Linux root/deduplicated filesystem accounting and AMD sysfs collection; correct CPU and Windows NIC data.
- Preserve default-deny independent exec/WebSSH gates and redacted configuration snapshots. File control follows the WebSSH gate; version switching additionally follows the self-update/auto-update gate.
- Follow Komari 1.6 stable file metadata and streaming-transfer contracts with bounded workers and same-filesystem atomic commit; support auto-discovery without exposing credentials.
- Upstream Go issue #133 is a Go-only gopsutil macOS CPU-frequency crash; this agent uses the macOS sysctl path directly and has no shared dependency.
- Keep synchronous collection and existing rustls/ring dependencies; apply bounded resources and targeted tests instead of an async/framework rewrite.
- Review community contributions individually and retain credit when an integrated fix supersedes a PR.

startupConfig and switchVersion are released server features in 1.6.0, not merely main-branch experiments. Supporting those event shapes does not authorize changing server notification configuration or enabling remote control.

## Remaining scope

Btrfs qgroup quota reporting remains under review. The upstream Go-agent PR combines automatic quota detection with a background filesystem scan and has limited real-system coverage; it is not part of 0.7. Network-test/iperf/trace DTOs still do not establish a live server contract.

Native FreeBSD execution, optional GPU hardware combinations and native terminal behavior require platform-specific testing beyond a successful release build. A lower fixed MSRV and universal RSS guarantee are not established.

Further upstream changes should be reviewed against this baseline, implemented with focused regression tests, then validated with actual supported server versions before release. Finished work is recorded in [CHANGELOG](CHANGELOG.md), not repeated as a permanent task backlog here.
