<div align="center">

# komari-agent-rs

**极致轻量的 Komari 监控 Agent**

Rust · 同步单线程 · ~1.5 MB 二进制 · &lt;3 MB RSS · **内存占用仅为 Go Agent 的 1/10**

[![CI](https://github.com/DeliciousBuding/komari-agent-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/DeliciousBuding/komari-agent-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/DeliciousBuding/komari-agent-rs?color=green&label=release)](https://github.com/DeliciousBuding/komari-agent-rs/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platforms](https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows%20%7C%20freebsd-blueviolet)](#安装)
[![Stars](https://img.shields.io/github/stars/DeliciousBuding/komari-agent-rs?style=social)](https://github.com/DeliciousBuding/komari-agent-rs/stargazers)

`komari` · `rust` · `rustls` · `监控` · `轻量` · `单线程`

📦 **[下载](https://github.com/DeliciousBuding/komari-agent-rs/releases)** · 📖 **[文档](#文档)** · 💬 **[讨论区](https://github.com/DeliciousBuding/komari-agent-rs/discussions)** · 📋 **[更新日志](CHANGELOG.md)**

**[English](README.md)** · **[简体中文](README.zh-CN.md)**

</div>

---

> [`komari-monitor/komari-agent`](https://github.com/komari-monitor/komari-agent)(Go)的 Rust 重写,服务于 [`komari-monitor/komari`](https://github.com/komari-monitor/komari) 服务器监控面板。与官方 Go Agent 线缆兼容 —— 在 Komari 服务端注册一个节点,把 Agent 指向它即可。

## 目录

- [快速开始](#快速开始)
- [功能](#功能)
- [与 Go / Zig Agent 对比](#与-go--zig-agent-对比)
- [安装](#安装)— Linux / macOS / Windows / FreeBSD
- [从源码构建](#从源码构建)
- [配置](#配置)— CLI / 环境变量 / JSON
- [架构](#架构)
- [文档](#文档)
- [贡献](#贡献)
- [许可证](#许可证)

---

## 快速开始

```bash
# 下载并运行(Linux)
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-rs-linux-x86_64 -o komari-agent
chmod +x komari-agent
./komari-agent --token 你的TOKEN --endpoint https://你的komari服务端
```

单个二进制,无运行时依赖,不依赖 OpenSSL。

## 功能

- **CPU** —— 每核利用率、型号、核心数
- **内存** —— 总量 / 已用 / 可用 / swap,三模式上报(raw / 含缓存 / 仅已用)
- **磁盘** —— 每分区总量 / 已用 / 空闲,过滤物理设备
- **网络** —— 每网卡 RX/TX 速率增量、TCP/UDP 连接数
- **GPU** —— 型号 / 利用率 / 显存 / 温度(NVIDIA、AMD ROCm、Intel DRM、Apple Metal、DXGI)
- **负载** —— 1/5/15 分钟平均负载
- **连接** —— TCP4/TCP6/UDP socket 计数
- **进程** —— 总进程数
- **运行时间** —— 系统运行秒数
- **IP** —— 公网 IP 自动探测
- **系统信息** —— OS 名称、内核版本、虚拟化检测
- **v1/v2 协议** —— JSON-RPC 2.0 over WebSocket,带 HTTP POST 回退(JSON-RPC 1.0 兼容)
- **远程执行** —— 通过服务端在 Agent 上执行命令
- **ICMP/TCP/HTTP ping** —— 三级 ping 自动降级
- **交互式终端** —— PTY(Linux/macOS/FreeBSD)+ ConPTY(Windows)
- **自我更新** —— 拉取并应用最新 GitHub Release
- **跨平台** —— Linux、macOS、Windows、FreeBSD,功能完全对齐

## 与 Go / Zig Agent 对比

| | Go Agent | Zig Agent | **komari-agent-rs** |
|---|---|---|---|
| 二进制大小 | ~14 MB | ~1.3 MB | **~1.5 MB**(196 KB 自研代码 + ~1 MB TLS 栈) |
| 稳态 RSS | ~18-32 MB | ~8.5 MB | **~3 MB** |
| 并发模型 | goroutines | async | **同步单线程** |
| TLS | crypto/tls | OS 原生 | **rustls + ring** |
| JSON | encoding/json | std.json | **手写零分配** |
| Gzip | compress/gzip | std.compress | **定长 Huffman 编码器** |
| 异步运行时 | 内置 | 内置 | **无 —— 不用 tokio,不用 async-std** |
| 构建依赖 | Go 工具链 | Zig 编译器 | **仅 Rust(stable)** |

**二进制体积去向**(cargo-bloat,Linux musl release,stripped):

| 组件 | 大小 | 占比 |
|---|---:|---:|
| rustls(TLS 实现) | ~1.1 MB | 40% |
| ring(加密原语) | ~528 KB | 19% |
| webpki(根证书包) | ~262 KB | 10% |
| std(Rust 标准库) | ~471 KB | 17% |
| **我们自己的 Agent 代码** | **~196 KB** | **7%** |
| 其他(杂项 crate) | ~余量 | 7% |

二进制是 **TLS-bound**:rustls + ring + webpki(~70%)是不依赖 OpenSSL 对接 HTTPS/WSS 的不可压缩成本。我们实际写的监控 Agent 只有 **196 KB** —— 这才是"极致轻量"的成就。在小 VPS 上真正有意义的是 **稳态 RSS ~3 MB vs Go Agent 的 18-32 MB**:同样的工作,常驻内存少了一个数量级。

## 安装

### Linux

```bash
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-rs-linux-x86_64 -o komari-agent
chmod +x komari-agent
sudo mv komari-agent /usr/local/bin/
komari-agent --token 你的TOKEN --endpoint https://你的komari服务端
```

### macOS

```bash
curl -L https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-rs-macos-x86_64 -o komari-agent
chmod +x komari-agent
sudo mv komari-agent /usr/local/bin/
komari-agent --token 你的TOKEN --endpoint https://你的komari服务端
```

### Windows

```powershell
Invoke-WebRequest -Uri "https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-rs-windows-x86_64.exe" -OutFile "komari-agent.exe"
.\komari-agent.exe --token 你的TOKEN --endpoint https://你的komari服务端
```

### FreeBSD

```bash
fetch https://github.com/DeliciousBuding/komari-agent-rs/releases/latest/download/komari-agent-rs-freebsd-x86_64 -o komari-agent
chmod +x komari-agent
mv komari-agent /usr/local/bin/
komari-agent --token 你的TOKEN --endpoint https://你的komari服务端
```

> **DPI / 中间盒网络**:若你的网络环境阻断 WebSocket 握手(WS 返回 200 而非 101),加 `--http-only` 走纯 HTTP POST 上报,绕开干扰。详见 [CHANGELOG](CHANGELOG.md) v0.1.4。

> **Agent 与服务端同机**:`--endpoint http://127.0.0.1:25774` 可本地直连(v0.1.6 起支持 `http://`,无需 TLS 公网回环)。

## 从源码构建

需要 Rust stable(1.75+)。

```bash
git clone https://github.com/DeliciousBuding/komari-agent-rs.git
cd komari-agent-rs

# 核心构建 —— 监控 + v1/v2 协议 + HTTP 回退
cargo build --release

# 完整构建 —— 全部 feature
cargo build --release --features full

# 按需 feature
cargo build --release --features gpu-detection     # +GPU 监控
cargo build --release --features terminal           # +交互式终端
cargo build --release --features ping               # +ICMP/TCP/HTTP ping
cargo build --release --features self-update        # +自我更新
```

## 配置

每个选项都可以通过 CLI flag、`AGENT_*` 环境变量、或 JSON 配置文件(`--config /path/to/config.json`)传入。优先级:CLI flag > 环境变量 > 配置文件。

核心选项:

| Flag | 环境变量 | 默认 | 说明 |
|---|---|---|---|
| `--endpoint` / `-e` | `AGENT_ENDPOINT` | — | Komari 服务端 URL,如 `https://komari.example.com`(必填) |
| `--token` / `-t` | `AGENT_TOKEN` | — | Agent 认证 token(必填) |
| `--config` | `AGENT_CONFIG_FILE` | — | JSON 配置文件路径 |
| `--interval` / `-i` | `AGENT_INTERVAL` | `1` | 指标上报间隔(秒) |
| `--protocol-version` | `AGENT_PROTOCOL_VERSION` | `2` | `2`=JSON-RPC v2,`1`=v1(被拒自动降级) |
| `--http-only` | `AGENT_HTTP_ONLY` | `false` | DPI 逃生:仅 HTTP POST,不用 WS |
| `--gpu` | `AGENT_ENABLE_GPU` | `false` | 启用 GPU 指标 |
| `--disable-web-ssh` | `AGENT_DISABLE_WEB_SSH` | `true` | 禁用 Web 终端 |
| `--disable-auto-update` | `AGENT_DISABLE_AUTO_UPDATE` | `true` | 禁用 GitHub Release 自更新 |
| `--ignore-unsafe-cert` / `-u` | `AGENT_IGNORE_UNSAFE_CERT` | `false` | 跳过 TLS 证书校验(不安全) |
| `--custom-dns` | `AGENT_CUSTOM_DNS` | — | 自定义 DNS(逗号分隔) |
| `--prefer-ip-version` | `AGENT_PREFER_IP_VERSION` | auto | `4` 或 `6` |

出站代理通过标准 `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` 环境变量(HTTP CONNECT 和 SOCKS5/SOCKS5h),`NO_PROXY` 支持域名/通配符/CIDR/IP 绕过。完整选项见 `komari-agent-rs --help`。

## 架构

Agent 通过 WebSocket(JSON-RPC 2.0)连接 Komari 服务端,WebSocket 不可用时回退到 HTTP POST(JSON-RPC 1.0 兼容)。1 秒 tick 循环把系统指标收集到栈上 scratch arena,**热路径零堆分配**。

```
┌──────────────┐   WebSocket/HTTP   ┌──────────────┐
│ komari-agent │ ◄──────────────────► │ Komari 服务端 │
│  (sync,      │   JSON-RPC 2.0      │              │
│   单线程)    │   (TLS 1.3 via      │              │
│              │    rustls + ring)   │              │
└──────┬───────┘                     └──────────────┘
       │
       │ 1s tick(零分配)
       │
  ┌────┴────────────────────────────┐
  │  CPU / 内存 / 磁盘 / 网络 / GPU  │
  │  负载 / 连接 / 进程 / 运行时间   │
  │  IP / OS / 虚拟化               │
  └─────────────────────────────────┘
```

## 设计原则

- **热路径零依赖** —— 不用 serde、clap、flate2、tokio
- **手写 JSON 编码器**(~300 行)—— 线缆输出一致,零分配
- **手写 gzip 编码器**(~200 行)—— 定长 Huffman DEFLATE,合法 gzip,无需解码
- **手写 SHA-1 + Base64**(~160 行)—— WebSocket 握手不依赖加密 crate
- **OS 原生 TLS 根证书** —— Linux `/etc/ssl/certs`、Windows CryptoAPI、macOS Security.framework
- **cfg-gated 平台分发** —— 编译期类型选择,无 vtable 开销
- **显式 config 传递** —— 无全局变量,完全可测

## 文档

- 📐 **[docs/plan/spec.md](docs/plan/spec.md)** —— 完整设计规格(DD1–DD6 设计决策)
- 🏛️ **[docs/plan/architecture-reference.md](docs/plan/architecture-reference.md)** —— 13 份并行设计文档,架构蓝图
- ⚖️ **[docs/COMPARISON.md](docs/COMPARISON.md)** —— Go / Zig / Rust Agent 对比 + 基准
- 🧪 **[docs/TESTING.md](docs/TESTING.md)** —— 测试策略 + 三平台验证
- 📋 **[CHANGELOG.md](CHANGELOG.md)** —— 版本历史(v0.1.0 → v0.1.6)

## 贡献

欢迎贡献 —— 见 **[CONTRIBUTING.md](CONTRIBUTING.md)**(开发环境、轻量哲学、提交规范、测试要求)。

- 🐛 **[Issues](https://github.com/DeliciousBuding/komari-agent-rs/issues)** —— Bug 报告 & 功能请求
- 💬 **[讨论区](https://github.com/DeliciousBuding/komari-agent-rs/discussions)** —— 问答与想法
- 🔒 **[SECURITY.md](SECURITY.md)** —— 漏洞披露

## 许可证

MIT —— 详见 [LICENSE](LICENSE)。
