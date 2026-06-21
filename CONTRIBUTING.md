# Contributing to komari-agent-rs

Thanks for your interest! This is a small, focused project — contributions that fit the **featherweight** philosophy are very welcome.

## Development setup

Requires **Rust stable (1.75+)**.

```bash
git clone https://github.com/DeliciousBuding/komari-agent-rs.git
cd komari-agent-rs
cargo build --release
cargo test
```

## The featherweight philosophy (read before changing code)

The agent is **deliberately dependency-free in the hot path**:

- **No `serde`, `clap`, `flate2`, `tokio`** — the only external crates are `rustls` + `ring` (the irreducible TLS floor).
- **Hand-rolled JSON encoder, gzip (fixed-Huffman), SHA-1, Base64** — wire-identical output, zero heap allocation in the tick loop.
- **Sync single-threaded** — no async runtime, ever.

If your change adds a dependency, justify in the PR **why it cannot be done with std**. New deps that bloat the binary past 2 MB or break sync-single-threaded will likely be rejected. The binary is ~1.5 MB today; ~1 MB of that is rustls+ring+webpki — our own agent code is ~196 KB.

## Before submitting a PR

All of these must pass (CI enforces them too):

```bash
cargo fmt                  # formatting
cargo test                 # default features
cargo test --all-features  # every feature gate
cargo clippy --release -- -D warnings   # no warnings
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

Tagged `vX.Y.Z` on `main`. Each tag triggers the release workflow (6-platform build + checksums + GitHub Release). See [CHANGELOG.md](CHANGELOG.md).
