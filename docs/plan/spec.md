# komari-agent-rs 正式规格

最后更新：2026-06-20
来源：spec-driven-develop Phase 2 精炼输出

## 硬约束

| 约束 | 目标 |
|------|:----:|
| 二进制 | <1 MB（Linux stripped） |
| 稳态 RSS | <3 MB |
| 热路径分配 | 0 |
| 外部依赖 | rustls（+ webpki-roots ~80KB 豁免）、ring crypto |
| 并发模型 | sync 单线程 |
| License | MIT |
| 仓库 | DeliciousBuding/komari-agent-rs |

## 已确认设计决策

| # | 决策 | 选择 | 拒绝的方案 |
|---|------|------|-----------|
| 1 | CLI 解析 | **手写** ~150 行 | clap（+15KB，unicode 依赖链） |
| 2 | JSON 序列化 | **JsonBuf + Field 枚举 + EncodeJson trait** ~300 行 | serde（+300KB） |
| 3 | WebSocket | **手动帧 codec + SHA-1/Base64 自实现** ~350 行 | tungstenite（+200KB） |
| 4 | HTTP 客户端 | **手动 HTTP/1.1 POST** ~70 行 | reqwest（+500KB+） |
| 5 | TLS 证书 | **OS 原生根证书**（Linux 读 `/etc/ssl/certs`，Windows CryptoAPI，macOS Security.framework） | webpki-roots 全量 Mozilla bundle（+80KB） |
| 6 | TLS crypto | **ring**（纯 Rust，无 C 编译器依赖） | aws-lc-rs（需要 C 编译器） |
| 7 | GPU 检测 | **裸 FFI + 子进程调用**，nvidia-smi CSV 模式，Linux 优先 sysfs | windows crate（+100KB+） |
| 8 | GPU Windows DXGI | **裸 COM FFI** ~300 行 | windows-rs（+80-150KB） |
| 9 | Gzip 压缩 | **固定 Huffman 纯编码** ~200 行（不解码） | flate2/miniz_oxide（+20-50KB），完整 DEFLATE（+250 行换 30% 额外压缩率） |
| 10 | ICMP Ping | **三层降级**：ICMP → TCP → HTTP | 仅 TCP/HTTP（功能不完整） |
| 11 | Terminal | **feature-gated**（`terminal` feature），默认不编 | 不做（丢掉 Web SSH） |
| 12 | 自更新 | **Feature-gated**，最小实现 ~80 行 | 无（丢掉自更新） |
| 13 | Feature gate | `default=[]`（最瘦 ~600KB），`full` 开启全部（~876KB） | 全部编进默认（二进制超标） |
| 14 | 跨平台 | Linux/Windows/macOS/FreeBSD 全功能对等 | 阉割边缘平台 |

## Feature 矩阵

| Feature | 二进制增量 | 默认 | 说明 |
|---------|:---------:|:----:|------|
| (none) | ~600 KB | ✅ | 核心监控 + v1/v2 协议 + HTTP fallback |
| `gpu-detection` | +80 KB | ❌ | GPU 名称/利用率/显存/温度 |
| `terminal` | +60 KB | ❌ | PTY/ConPTY 交互式 Shell |
| `ping` | +30 KB | ❌ | ICMP/TCP/HTTP ping 三层降级 |
| `self-update` | +15 KB | ❌ | GitHub Release 自动更新 |
| **`full`（全部）** | **~876 KB** | — | 完整功能 <1MB ✅ |

## 参考仓库

| 仓库 | 路径 | 用途 |
|------|------|------|
| Go 原版 | `D:/Code/Projects/external/komari-agent-go` | 功能 spec + 协议参考 |
| Zig 版 | `D:/Code/Projects/external/komari-zig-agent` | 轻量实现参考 |
| Rust 现有版 | `D:/Code/Projects/external/komari-monitor-rs` | Rust 参考 |

## 架构蓝图

详见 `docs/plan/architecture-reference.md`（2707 行，113KB，14 agent 并行产出）。
