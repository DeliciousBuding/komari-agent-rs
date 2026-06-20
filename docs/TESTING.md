# Testing & Performance Matrix

Komari-agent-rs 验证矩阵与性能基准。spec-driven develop Phase 5/6 产出。

最后更新：2026-06-21

---

## 1. 三平台端到端测试

| 平台 | target | 二进制 | 稳态 RSS | 连接方式 | report 接受 | 状态 |
|------|--------|:------:|:--------:|----------|:-----------:|:----:|
| WSL Linux | x86_64-unknown-linux-gnu (glibc) | 1.5 MB | 3.3 MB | HTTPS_PROXY（mihomo TUN） | ✅ | ✅ |
| Windows 本机 | x86_64-pc-windows-msvc | 1.2 MB | 3.1 MB（私有内存） | 直连（TUN 透明） | ✅ | ✅ |
| us3 真实 Linux | x86_64 (GCP 生产) | 1.5 MB | 4.65 MB | 直连（公网） | ✅ | ✅ |

- **report**：三平台均被 Komari server 接受（0 Invalid JSON / Invalid report format）
- **WS 心跳**：每 30s ping，稳定
- **监控数据**：CPU/RAM/Swap/Load/Disk/Network/Connections/Process/Uptime 每秒上报

### us3 真实 Linux 测试方法

us3 已运行 Go agent（systemd）。测试不替换，临时借用：

```bash
# scp Rust 二进制（WSL 编译的 Linux ELF）
scp target/release/komari-agent-rs us3:/tmp/komari-agent-rs
ssh us3 'bash -s' <<'REMOTE'
CFG=/etc/komari-agent/config.json
EP=$(sudo python3 -c "import json;print(json.load(open('$CFG'))['endpoint'])")
TOKEN=$(sudo python3 -c "import json;print(json.load(open('$CFG'))['token'])")  # token 不离开 us3
sudo systemctl stop komari-agent          # 临时停 Go agent
sudo timeout 14 /tmp/komari-agent-rs --endpoint "$EP" --token "$TOKEN" \
  --disable-web-ssh --disable-auto-update --protocol-version 1
sudo systemctl start komari-agent         # 恢复 Go agent
REMOTE
```

---

## 2. 性能基准

### 二进制大小

| 构建 | 大小 | 说明 |
|------|:----:|------|
| Linux glibc (stripped) | 1.5 MB | TLS 栈 ~70%（rustls + ring + webpki），自身代码 ~200 KB |
| Linux musl (CI) | ~1.5 MB | 静态链接 |
| Windows MSVC (stripped) | 1.2 MB | MSVC 链接更紧凑 |

> 原 spec 目标 <1 MB。被 TLS 顶层否决（无 OpenSSL 的 HTTPS/WSS 不可压缩 ~1 MB）。自身代码 200 KB 已达极致。

### release profile（已极致优化）

```toml
[profile.release]
opt-level = "z"        # 最小体积
lto = "fat"            # 全链接时优化
codegen-units = 1      # 单代码生成单元（最大优化空间）
panic = "abort"        # 去掉 unwind 表
strip = "symbols"      # 剥离符号
debug = false
incremental = false
[profile.release.package."*"]
opt-level = "z"        # 依赖也最小体积
```

### RSS 对比

| 实现 | 稳态 RSS | 倍数 |
|------|:--------:|:----:|
| **komari-agent-rs**（us3 生产） | **4.65 MB** | 1× |
| komari-agent-rs（WSL/Windows） | 3.1-3.3 MB | — |
| komari-zig-agent | 8.5 MB | 1.8× |
| komari-agent（Go） | 18-32 MB | 4-7× |

全舰队 8 台从 Go 换 Rust 可省 ~150 MB。

---

## 3. 功能对齐矩阵（Go → Rust）

| 功能 | Go | Rust | 说明 |
|------|:--:|:--:|------|
| WebSocket v1/v2 | ✅ | ✅ | 4 级 FSM: WsV2→WsV1→HttpV2→HttpV1 |
| HTTP POST fallback | ✅ | ✅ | 3-strike 降级 |
| exec 远程命令 | ✅ | ✅ | sh -s (Unix) / powershell (Windows) + task/result |
| ping (ICMP/TCP/HTTP) | ✅ | ✅ | 3-tier 降级，feature-gated `ping` |
| terminal/WebSSH | ✅ | ✅ | WS<->PTY 双向循环 + ConPTY resize，feature-gated `terminal` |
| gzip 压缩 | ✅ | ✅ | v2 HTTP POST（permessage-deflate 未实现） |
| JSON config | ✅ | ✅ | `--config` 文件分层 |
| self-update | ✅ | ✅ | GitHub Releases + SHA256，feature-gated `self-update` |
| month-rotate 流量 | ✅ | ✅ | netstatic 持久化 |
| NO_PROXY | ✅ | ✅ | 域名/通配符/CIDR/IP |
| 代理 auth | ✅ | ✅ | Basic Auth |
| SOCKS5 | ❌ | ✅ | **超越 Go**（SOCKS5/SOCKS5h） |
| custom-dns | ✅ | ✅ | dns.rs 接线 |
| GPU 监控 | ✅ | ✅ | 裸 FFI，feature-gated `gpu-detection` |
| 跨平台 | Linux/Win/macOS/BSD | Linux/Win/macOS/BSD | 4 平台对等 |

---

## 4. 网络适配矩阵

`Dialer` 统一所有出站连接，适配任何网络环境：

| 场景 | 支持 | 机制 |
|------|:----:|------|
| 直连 | ✅ | 默认（custom-dns + prefer-ip） |
| HTTP/HTTPS 代理 | ✅ | HTTPS_PROXY/HTTP_PROXY/ALL_PROXY |
| SOCKS5 代理 | ✅ | socks5://（本地 DNS） |
| SOCKS5h 代理 | ✅ | socks5h://（远程 DNS，防污染） |
| 代理认证 | ✅ | user:pass@ + Proxy-Authorization |
| NO_PROXY 旁路 | ✅ | 域名/通配符/CIDR/IP 匹配 |
| Cloudflare Access | ✅ | CF-Access headers |
| mihomo TUN | ✅ | HTTP_PROXY CONNECT 隧道 |

---

## 5. 已知局限

- **basicInfo fork 兼容（已解决）**：v1.1.9 fork 拒绝 `kernel_version`/`cpu_physical_cores` 字段（HTTP 500）。agent 用 Go 兼容回退（先完整 payload，500 则删字段重试）→ us3 验证 200 成功。
- **v2 API 不支持**：fork 的 `/api/clients/v2/rpc` 返回 SPA HTML。FSM 正确回退 v1。
- **WS permessage-deflate 未实现**：HTTP POST gzip 已实现；WS 压缩（RFC 7692）需扩展协商 + raw deflate/inflate，复杂度高。当前 fork 用 v1 WS（不协商 deflate），report 仅 ~356B，不压缩可接受。标记为后续优化。

---

## 6. 测试命令速查

```bash
# 单元测试
cargo test --release                    # 213 tests
cargo test --release --features full    # 全 feature

# WSL（Linux 测试，需 proxy 穿 TUN）
wsl -e bash -c 'source ~/.cargo/env && cd /mnt/d/Code/Projects/edgehub/komari-agent-rs && cargo build --release'
HTTPS_PROXY=http://127.0.0.1:7897 ./target/release/komari-agent-rs --endpoint ... --token ... --protocol-version 1

# Windows 本机（直连 TUN）
cargo build --release
./target/release/komari-agent-rs.exe --endpoint ... --token ... --protocol-version 1

# CI
gh run list --limit 3
```
