# komari-agent-rs
Last updated: 2026-10-22

[简体中文](README.zh-CN.md) · [Releases](https://github.com/DeliciousBuding/komari-agent-rs/releases) · [Changelog](CHANGELOG.md)

A Rust monitoring agent for [Komari](https://github.com/komari-monitor/komari), using a synchronous collection loop and OS-native certificate roots. The default build includes metrics and ping probes; GPU collection, interactive terminals and self-update are optional features.

Version 0.6 targets Komari's v2 JSON-RPC protocol, carried over WebSocket or HTTP POST. See the [support matrix](docs/COMPARISON.md) for supported features, platform coverage and deliberate differences from the official Go agent. This project does not claim complete feature equivalence or a fixed RSS on every host.

## Run

Download the asset for your OS and architecture from the [release page](https://github.com/DeliciousBuding/komari-agent-rs/releases), along with `checksums.txt`. Verify the checksum before executing it.

Create a private UTF-8 JSON file without a BOM:

```json
{
  "endpoint": "https://komari.example.com",
  "token": "replace-with-node-token",
  "interval": 3,
  "disable_exec": true,
  "disable_web_ssh": true,
  "disable_auto_update": true
}
```

Restrict file access to the service account, then run:

```sh
./komari-agent-rs --config ./config.json
./komari-agent-rs --version
```

Keep the token in the config file rather than shell history or process arguments. Register the node in Komari first. Replacing this executable does not require changing its node token or registration.

For service installation, download and inspect `install.sh` (Linux/macOS/FreeBSD) or `install.ps1` (Windows) from the same release revision. Unix supports system or `--user` systemd installation. The installers verify release checksums and preserve existing config; the Windows installer accepts optional explicit token/endpoint overrides and stores credentials only in the config. Windows releases currently target x86_64.

## Build

Use current stable Rust, a C toolchain for `ring`, and the platform SDK/linker. A separate fixed MSRV is not maintained.

```sh
cargo build --locked --release
cargo build --locked --release --features full
cargo build --locked --release --no-default-features
cargo check --locked --features terminal
```

| Feature | Includes |
|---|---|
| default | Metrics and ping |
| gpu-detection | GPU tooling/sysfs collection; runtime `enable_gpu` must also be enabled |
| terminal | Optional interactive PTY support |
| self-update | Verified GitHub release installation, off by default at runtime |
| full | All features above |

Linux release assets have `-default` and `-full` variants; both include ping. Release CI checks the binary-size budget for supported matrix entries. RSS and peak memory depend on TLS roots, platform, optional collectors and workload.

## Configuration

Load order follows the Go agent: CLI flags, then environment, then the JSON config file. JSON is highest priority. `--help` lists all flags.

| Setting | Default | Meaning |
|---|---|---|
| endpoint / token | required | Server origin and node credential |
| interval | 3 seconds | Metrics reporting cadence; explicit existing values remain effective |
| info_report_interval | 5 minutes | Static basic-info refresh |
| reconnect_interval | 5 seconds | Initial exponential reconnect backoff |
| max_retries | 10 | Warning threshold; reconnecting continues after it is exhausted |
| protocol_version | 2 | Only 2 is accepted in 0.6; v1 settings produce a migration error |
| http_only | false | Use v2 HTTP report/pull only, with no WebSocket or terminal |
| disable_exec | true | Independent one-shot command gate |
| disable_web_ssh | true | Independent interactive terminal gate |
| disable_auto_update | true | Leave upgrades to the service supervisor/operator |
| ignore_unsafe_cert | false | Explicitly bypasses monitoring TLS authentication; keep false |
| include_nics / exclude_nics | empty | NIC selection; use actual platform interface names |
| include_mountpoints / exclude_mountpoints | empty | Filesystem selection |

Each setting also has an `AGENT_*` environment name, shown in help. Never post a raw effective configuration or proxy-authentication value in an issue.

NIC selection follows the Go agent: built-in excluded prefixes take precedence,
then include patterns, then exclude patterns. Patterns match the whole interface
name using platform-aware glob syntax (`*`, `?`, character classes). On Windows
the name is the adapter alias (`Get-NetAdapter` → `Name`), not its description.

TLS uses `rustls` with `ring` and `rustls-webpki`; roots are loaded from the operating system, not from `webpki-roots`. Restart after changing the trust store. Optional executable downloads always verify TLS, even if monitoring certificate checks were explicitly disabled.

## Upgrade from 0.5

Keep your existing config and verified old binary. Replace the executable with the matching verified asset, restart its supervisor, and check `--version` and fresh server-side metrics.

The compatibility change is v2-only transport: Komari 1.5+ exposes `/api/clients/v2/rpc`; removed v1 routes are no longer attempted. An explicit `protocol_version=1` must be removed or changed to 2 for a v2 server. Pre-v2 servers require the previous agent release or a separate server migration.

Komari 1.5.1 closes an idle WebSocket after 11 seconds; Ping does not extend its
deadline. Keep a shorter reporting interval for sustained WS, or use HTTP. Komari
1.6.0 supports Ping-based keepalive with long report intervals. A 1.5.1 long-interval
connection can fall back to HTTP without changing the configured report cadence.

Windows NIC filters previously matched description substrings; migrate them to
whole-alias glob patterns. Include wins over exclude when both match. Interfaces
such as `tun`/`vnet`/`utun` and inactive NIC cumulative counters are no longer
implicitly removed beyond Go's built-in prefix rules; exclude them explicitly
if they do not belong in your totals.

No update enables remote execution, WebSSH or automatic upgrades in an existing installation. Startup config responses redact token and discovery credentials. Server-requested version switching and file management are unsupported and are not advertised.

## Development and documentation

- [Architecture](docs/architecture.md): module boundaries, scheduling, resource limits and security decisions.
- [Support matrix](docs/COMPARISON.md): feature and platform coverage.
- [Testing](docs/TESTING.md): reproducible checks and verification limits.
- [Roadmap](ROADMAP.md): upstream references and remaining work.
- [Contributing](CONTRIBUTING.md) and [Security policy](SECURITY.md).

## License

[MIT](LICENSE).
