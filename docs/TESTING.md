# Testing
Last updated: 2026-10-22

CI is the primary validation path. Local development uses `cargo check --locked` and focused regressions; full release validation runs on the workflow matrix.

```sh
cargo fmt --check
cargo check --locked --all-features
cargo test --locked --all-features
cargo clippy --locked --all-features -- -D warnings
```

CI checks individual feature builds, Linux and Windows tests, coverage, platform builds, binary size and RustSec advisories. Windows additionally runs `tests/installers.ps1` offline; it mocks download/task operations rather than changing a real installation.

## Regression coverage

| Area | Cases |
|---|---|
| Protocol | v2 report envelope, nested event parameters, RPC errors, WS/HTTP fallback, HTTP-only, long intervals and permission gates |
| JSON/config | UTF-8, escapes/surrogates, malformed fields, duplicate keys, bounded input/nesting and redacted diagnostics |
| Processes | Timeout, inherited pipes, large stdin/both output streams, output cap, exit code, Unicode, spawn errors and descendant cleanup |
| NIC selection | Go prefix/include/exclude priority, whole-name globs, Unicode/classes; Windows aliases, inactive counters, NDIS/LUID de-duplication and SDK offsets |
| Linux CPU | ARM IDs/heterogeneous names, topology, guest accounting and successful-read caching |
| Linux disk/GPU | Synthetic mount/stat data and sysfs trees, no production mounts or GPU requirement |
| Updates | API formatting, exact assets/checksums, HTTPS redirects, authentication stripping, framing/caps and rollback |
| Persistence | Atomic replacement, failed replacement/locked Windows target, strict month/u64, Unix symlink referent safety |
| WS reads | Partial header/length/payload across real socket timeouts, bounded frames, reset/EOF/truncation classification |
| TLS | Normal chain, wrong name, expiry, unknown issuer and all eight distrusted public keys |

The subprocess test executable contains an ignored child-process fixture invoked by the parent tests. Its ignored marker is intentional, not a skipped behavior assertion.

## TLS fixtures

[Fixture sources and generation](../tests/fixtures/tls/README.md) describe public SPKI provenance and synthetic certificates. Tests use a fixed validation time, contain no private keys and assert the exact distrust error. An independent old-library control demonstrates the regression; generic self-signed rejection does not establish explicit public-key distrust.

These tests verify certificate validation. Runtime deployment must also restart the process and validate a normal TLS connection because roots are loaded at startup.

## Integration acceptance

Use isolated fresh Komari servers with synthetic admin/node credentials and local temporary data. Test each claimed server version and transport independently:

1. Register a node and verify basicInfo plus fresh ingested metrics, not merely HTTP 200.
2. Check ping results, nested parameters, event acknowledgement and permitted/denied control behavior.
3. Test WebSocket compression both ways and recovery after a server restart. For 1.5.1 use the normal 3-second cadence for sustained WS, and verify long intervals fall back to HTTP; its 11-second deadline is not Ping-aware. For 1.6.0 use a 120-second report interval and observe actual Ping/Pong maintaining WS beyond its 75-second idle deadline without a new report.
4. Verify startup-config responses redact synthetic credentials and version-switch requests cannot change the binary.
5. Stop only the processes/resources created by that run and keep a report naming exact server/agent digests.

Do not run production replacement recipes as tests or post raw config/tokens in CI logs. Compile success and unit tests alone are not a claim of complete Go-agent feature parity, native hardware coverage or deployment success.
