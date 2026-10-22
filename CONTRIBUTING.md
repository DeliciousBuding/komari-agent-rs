# Contributing to komari-agent-rs

Thanks for your interest! This is a small, focused project — contributions that fit the **featherweight** philosophy are very welcome.

## Development setup

Use current stable Rust and a C toolchain/platform SDK for ring. A separate fixed
MSRV is not maintained.

```bash
git clone https://github.com/DeliciousBuding/komari-agent-rs.git
cd komari-agent-rs
cargo check --locked
```

## The featherweight philosophy (read before changing code)

The agent is **deliberately dependency-free in the hot path**:

- **No `serde`, `clap`, `flate2`, `tokio`** — the only external crates are `rustls` + `ring` (the irreducible TLS floor).
- The JSON encoder uses reusable buffers; transport, compression and some collectors allocate.
- The main collection/control loop is synchronous. HTTP pull and optional terminals use auxiliary threads; there is no async runtime.

If your change adds a dependency, justify in the PR why it cannot be done with
std. Keep the synchronous collection model and the release matrix's 2 MiB size
budget. Any size or RSS claim must name the measured build and workload.

## Before submitting a PR

All of these must pass (CI enforces them too):

```bash
cargo fmt --check
cargo check --locked --all-features
cargo test --locked --all-features
cargo clippy --locked --all-features -- -D warnings
```

Keep the release binary **< 2 MB** (release.yml enforces a size check).

## Commit style

[Conventional Commits](https://www.conventionalcommits.org/):

```
feat(http): support http:// endpoints (local server)
fix(windows): correct MIB_IF_ROW2 FFI offsets
docs: rebuild README configuration table
test: add parse_url http:// coverage
refactor(net): unify Dialer NO_PROXY bypass
```

Small, focused commits. One logical change per commit.

## Reporting bugs / requesting features

Open an [issue](https://github.com/DeliciousBuding/komari-agent-rs/issues). For **security** issues, follow [SECURITY.md](SECURITY.md) — do not open a public issue.

## Releases

Review a focused PR against the upstream server contract and add regressions for
the changed behavior. CI must pass for the final revision before merging.
Version changes go in Cargo.toml/Cargo.lock and CHANGELOG; breaking compatibility
changes require migration notes. Preserve contributor credit when integrating
or extending community patches.

Tag `vX.Y.Z` on tested `main` to run the current release matrix. Verify all assets,
checksums and platform results before calling the release complete. Runtime
deployment and rollback are separate operations; a tag or successful build does
not establish that an installed process is using it. See [Testing](docs/TESTING.md).
