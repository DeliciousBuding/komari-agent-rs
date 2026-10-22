# komari-agent-rs
最后更新：2026-10-22 13:25

[English](README.md) · [发布版本](https://github.com/DeliciousBuding/komari-agent-rs/releases) · [更新记录](CHANGELOG.md)

面向 [Komari](https://github.com/komari-monitor/komari) 的 Rust 监控 agent。主采集循环同步运行，使用操作系统根证书；默认构建含指标与 ping，GPU、交互终端、自更新按 feature 编译。

0.7 使用 v2 JSON-RPC，并补齐 Komari 1.6 稳定版的认证控制面。[支持矩阵](docs/COMPARISON.md) 列出功能、平台验证范围和与 Go agent 的差异；不承诺所有机器固定 RSS 或未经实机验证的硬件行为。

## 运行与升级

从[发布页](https://github.com/DeliciousBuding/komari-agent-rs/releases)下载匹配 OS/架构的资产及 `checksums.txt`，校验后运行。Linux 有 `-default` 和 `-full` 两种资产，均含 ping；Windows 当前发布 x86_64。

在 Komari 创建节点，将 endpoint/token 写入权限受限的 UTF-8 无 BOM 文件：

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

```sh
./komari-agent-rs --config ./config.json
./komari-agent-rs --version
```

凭据保存在配置中，不放进命令行、截图或 issue。服务安装可下载并审阅同一版本的 `install.sh` / `install.ps1`；Unix 支持 systemd `--user`。安装器校验下载、保留已有配置；Windows 可显式覆盖 token/endpoint，但任务参数只传配置路径。

升级保留旧二进制与原配置，替换已校验资产后重启 supervisor，并核对版本和服务端新指标。0.7 不迁移既有节点身份或 token，也不默认开启远程控制。

## 兼容边界

0.7 仅连接 Komari 1.5+ 的 `/api/clients/v2/rpc`，不调用已失效的 v1 路径。显式配置 `protocol_version=1` 会报迁移错误，应为 v2 服务端改成 2；旧服务端需保留旧 agent 或单独升级。

WebSocket 失败后改用 v2 HTTP report/pull；HTTP 持续重连，HTTP-only 不尝试 WebSocket。默认报告间隔为 3 秒，已有显式配置不变。控制轮询和 30 秒心跳不随较长报告间隔一起休眠。

官方中心 1.5.1 会在 11 秒未收到报告后关闭 WS，Ping 不续期；要持续使用 WS，报告间隔应更短，或直接使用 HTTP。1.6.0 支持 Ping 维持长报告间隔。1.5.1 长间隔连接可回落到 HTTP，不擅自缩短用户配置的报告间隔。

网卡筛选对齐 Go：内置排除前缀优先，其次 include，最后 exclude；使用整串名称 glob（`*`、`?`、字符类）。Windows 使用 `Get-NetAdapter` 的 `Name`/alias，不再使用 description 子串，旧筛选配置需要迁移。include/exclude 同时命中时 include 胜；`tun/vnet/utun` 等非 Go 内置排除项及 inactive NIC 累计计数不再额外隐式去除，需要时显式 exclude。

`disable_exec` 与 `disable_web_ssh` 是独立开关，默认均 true。终端还要求 terminal feature 且非 HTTP-only；文件管理也受 `disable_web_ssh` 控制。远程切版还要求 full/self-update 构建与 `disable_auto_update=false`。配置快照的 token/discovery key 脱敏，默认舰队姿态不会开启这些能力。`auto_discovery_key` 可用于首次注册并在可执行文件旁原子保存节点凭据。

## 配置与构建

配置加载顺序：CLI → 环境变量 → JSON；JSON 优先级最高。完整选项用 `--help`，默认值与主要设置见 [English README](README.md#configuration)。

TLS 使用 rustls/ring 与 rustls-webpki，根来自系统证书库，不依赖 webpki-roots。保持 `ignore_unsafe_cert=false`；系统 CA 变更后必须重启进程。可选自更新的下载始终验证 TLS，不继承监控连接的危险绕过设置。

使用 current stable Rust、ring 所需 C 编译器与平台 SDK/linker。未声明单独验证的固定 MSRV。

```sh
cargo build --locked --release
cargo build --locked --release --features full
cargo check --locked --no-default-features
```

主循环无 async runtime；HTTP pull 和终端使用受限辅助线程。编码的部分缓冲在栈上，整个报告路径仍会分配内存。体积由发布 CI 检查，RSS/峰值以具体平台、feature 和负载测量为准。

## 文档

- [架构与安全边界](docs/architecture.md)
- [支持矩阵](docs/COMPARISON.md)
- [测试方法](docs/TESTING.md)
- [上游与开放项](ROADMAP.md)
- [贡献指南](CONTRIBUTING.md)、[安全报告](SECURITY.md)、[MIT](LICENSE)
