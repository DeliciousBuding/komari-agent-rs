# CLAUDE.md

最后更新：2026-06-20

## 项目

komari-agent-rs — Komari 监控 Agent 极致轻量 Rust 重写

## 硬约束

- 二进制 <1 MB（Linux stripped），稳态 RSS <3 MB
- 外部依赖仅 `rustls` + `ring`（OS 原生 TLS 根证书，不捆绑 webpki-roots）
- sync 单线程，无 tokio/async
- 手写 JSON（无 serde）、手写 CLI（无 clap）、固定 Huffman gzip（无 flate2）
- 14 个设计决策见 `docs/plan/spec.md`

## 构建

```bash
cargo build --release                              # 核心 (~600KB)
cargo build --release --features full              # 完整功能 (~876KB)
cargo test
cargo fmt --check
cargo clippy -- -D clippy::all
```

## 工程管线

spec-driven develop — `docs/plan/` 下是 SSOT，`docs/analysis/` 下是分析文档。

## 参考代码

- Go 原版：`D:/Code/Projects/external/komari-agent-go`
- Zig 版：`D:/Code/Projects/external/komari-zig-agent`
- Rust 现有版：`D:/Code/Projects/external/komari-monitor-rs`
