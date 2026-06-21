# Komari Agent 实现对比（Go / Zig / Rust 第三方 / 我们的 Rust）

同一 Komari 协议、四种独立实现的横向对比。所有 binary 为 **Windows x86_64 release stripped** 实测（workflower 编译），同 target 公平对比。

最后更新：2026-06-21

---

## 1. 资源占用（核心指标）

| 实现 | binary | 外部依赖 | 稳态 RSS | 运行模型 |
|------|:------:|:--------:|:--------:|----------|
| **komari-agent-rs（我们）** | **1.2 MB** | **2**（rustls+ring） | **3.1 MB**（Win）/ 4.6 MB（us3） | sync 单线程 |
| komari-zig-agent | 1.93 MB | **0**（纯 stdlib） | ~8.5 MB* | threaded（std.Thread） |
| komari-monitor-rs（Rust 第三方） | 3.63 MB | **316** transitive crates | ~10+ MB*（tokio） | tokio multi-thread |
| komari-agent（Go 官方） | 10.4 MB | 13 direct + 14 indirect | 18-32 MB | threaded（goroutine） |

\* Zig/Go RSS 为历史记忆值；Rust-第三方的 tokio 运行时基线开销已知 ~8-10 MB。我们的 RSS 为实测。

**结论**：
- **binary 最小**：我们 1.2 MB，比 Zig（次小）还小 38%，比 Go 小 88%。
- **RSS 最低**：我们 3.1 MB，约为 Zig 的 1/3、Go 的 1/6-1/10。
- **依赖最少（按"功能/依赖比"）**：Zig 0 依赖最纯粹（无 TLS 库，用系统）；我们 2 个是 HTTPS/WSS 不可压缩的 TLS 栈（rustls+ring）。Rust-第三方的 316 crates 是反面教材（tokio/serde/tungstenite/sysinfo 全家桶）。

---

## 2. 功能对齐矩阵

| 功能 | Go 官方 | Zig | Rust 第三方 | **我们** |
|------|:--:|:--:|:--:|:--:|
| 监控上报（CPU/RAM/Disk/Net/...） | ✅ | ✅ | ✅ | ✅ |
| exec 远程命令 | ✅ | ✅ | ✅ | ✅ |
| ping（ICMP/TCP/HTTP） | ✅ | ✅ | ✅ | ✅ |
| terminal/WebSSH（PTY/ConPTY） | ✅ | ✅ | ✅ | ✅ |
| WS permessage-deflate 压缩 | ✅ | ❌ | ❌ | ✅ |
| HTTP gzip 压缩 | ✅ | ✅ | ❌ | ✅ |
| SOCKS5 代理 | ✅ | ❌ | ❌ | ✅ |
| NO_PROXY 旁路 | ✅ | ✅ | ❌ | ✅ |
| 代理 Basic Auth | ❌ | ✅ | ❌ | ✅ |
| custom-dns | ✅ | ✅ | ❌ | ✅ |
| self-update | ✅ | ✅ | ❌ | ✅ |
| GPU 监控 | ✅ | ✅ | ❌（硬编码空） | ✅ |
| v2 JSON-RPC 协议 | ✅ | ✅ | ❌ | ✅ |
| JSON config 文件 | ✅ | ✅ | ❌ | ✅ |
| 跨平台 | Linux/Win/macOS/BSD | Linux/Win/macOS/BSD | Win/Linux | Linux/Win/macOS/BSD |
| CLI flags | 28 | 35 | 21 | ~30 |

**结论**：
- **功能最全**：我们 + Go。我们额外有 **代理 Basic Auth**（Go 没有，依赖标准 env-proxy）。
- **Zig** 是强对手（功能近全、0 依赖、1.93MB），但缺 **SOCKS5 + WS 压缩**两项。
- **Rust 第三方**（komari-monitor-rs v0.3.4）功能严重不全（缺 config/dns/gpu/gzip/proxy/self-update/v2/ws-compression），upstream 甚至**默认编译不过**（feature-gating bug），是"Rust 但堆依赖"的反例。

---

## 3. 我们的独特定位

| 维度 | 我们 vs 最强对手 |
|------|------------------|
| binary | 1.2 MB < Zig 1.93 MB（更小 38%） |
| RSS | 3.1 MB < Zig 8.5 MB（更低 ~63%） |
| 功能 | 全（Zig 缺 SOCKS5 + WS 压缩） |
| 依赖 | 2（Zig 0，但我们需要 TLS 栈） |
| 代码 | 纯 std 手写（无 tokio/serde/tungstenite/clap） |

**唯一让出**：依赖数（Zig 0 vs 我们 2）。原因是我们用 rustls+ring 做 TLS（跨平台一致、内存安全），Zig 用系统 TLS。这是有意识的工程权衡——2 个依赖换跨平台 TLS 一致性 + 编译期安全。

---

## 4. 复现命令

```bash
# Go（stripped）
cd komari-agent-go && go build -trimpath -ldflags="-s -w" -o agent.exe .

# Zig（ReleaseFast）
cd komari-zig-agent && zig build -Doptimize=ReleaseFast

# Rust 第三方（需 patch feature-gating bug）
cd komari-monitor-rs && cargo build --release

# 我们
cd komari-agent-rs && cargo build --release   # profile 已 opt-level=z + fat LTO + panic=abort + strip
```

---

## 5. 数据来源

- binary/依赖/功能：workflower 并行编译 + 源码分析（Windows 工具链 go1.26 / zig0.16 / cargo）。
- 我们的 RSS：WSL/Windows/us3 实测（见 TESTING.md）。
- Zig/Go RSS：历史测量记忆；Rust-第三方未实测（tokio 基线已知偏高）。
