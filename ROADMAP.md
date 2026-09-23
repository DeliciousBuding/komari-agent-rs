# komari-agent-rs ROADMAP
最后更新：2026-09-23

> 基于 Go upstream（komari-monitor/komari-agent）持续跟踪对比。当前上游基线：v2 协议时代（komari server 1.5.0-fix1，v1 端点已移除）；v0.4.0 起本 agent 全量 v2 兼容（report 包络 / pull 长轮询 / 事件 ack）。**协议 spec 事实源 = server 仓 `protocol/v2/jsonrpc.go`**（`komari-monitor/komari-protocol` 契约仓 2026-08-04 起停更，不再代表线上协议；新方法直接在 server 仓落地）

## v0.2 ✅ 已完成（2026-07-14 → 07-15）

### v0.2.0（2026-07-14）— GPU / 数据正确性

| Issue | 功能 | 状态 |
|:-----:|------|:----:|
| #62 | GPU 详细指标（utilization/temp/vendor/device） | ✅ |
| #63 | 高延迟 ping 重试 + TCP 重传检测 | ✅ |
| #64 | 虚拟 GPU 过滤（virtio/vmware/qxl 等） | ✅ |
| #65 | 容器检测增强（podman/CRI-O/LXC） | ✅ |
| #66 | GPU 驱动名映射（i915→Intel 等） | ✅ |
| #67 | nvidia-smi/rocm-smi 路径检测 | ✅ |
| #90-#98 | 内存/网络/磁盘/IP/self-update 准确性、CI/CD、安全加固（原 v0.3 18 issues 全部落地） | ✅ |

### v0.2.1（2026-07-15）— WebSSH / 压缩

| 项 | 功能 | 状态 |
|:--:|------|:----:|
| — | permessage-deflate gorilla trailer 根因修复 | ✅ |
| — | interactive terminal E2E（`--features terminal`） | ✅ |
| — | `disable_exec` 独立于 `disable_web_ssh` | ✅ |

## v0.2.2 ✅（2026-08-15）— 官方对齐 + CI 并行化

| 项 | 功能 | 状态 |
|:--:|------|:----:|
| — | Windows NVIDIA 详细 GPU 监控（nvidia-smi CSV，对齐官方 `e5aefd4f`） | ✅ |
| — | 移除 Cloudflare Access 凭据（对齐官方 `8cd92149`） | ✅ |
| — | `install.sh --user`（非 root systemd user service） | ✅ |
| — | release workflow matrix 并行 + rust-cache + macOS x86_64 架构修复 | ✅ |

## v0.5.0 ✅（2026-09-23）— v2 agent 控制面事件

| 项 | 功能 | 状态 |
|:--:|------|:----:|
| — | `agent.startupConfig`（capability `startup_config`）：启动配置快照回传（含 version/features 戳），**token / auto_discovery_key 脱敏为 `[REDACTED]`**（刻意偏离上游——Go agent 连凭据一起回传） | ✅ |
| — | `agent.switchVersion`：识别后记 policy 日志并忽略；**策略性永久不支持**，不 advertise capability（见「不追」） | ✅ |

## 后续规划

- **持续跟官方**：Go agent 新功能逐项评估吸收（Windows nvidia-smi 多卡细节、非 root 安装增强）；上游 snapshot 轨道的远程换版已评估并拒绝（见「不追」）
- **架构**：`loongarch64` 待 Rust tier 稳定后跟进（当前 tier 3，需 nightly build-std，暂缓）
- **CI**：引入 cargo-nextest 提速、codecov 覆盖率门禁、cargo-audit 安全审计（见 `.github/workflows`）

## 不追

- Auto-discovery（舰队手动部署）
- 远程换版 `agent.switchVersion`（上游 2026-09 新增；与 `disable_auto_update` + SHA-256 手动钉版 SOP 冲突，v0.5.0 起识别+日志+忽略）
- SoC/嵌入式 GPU Device Tree（非服务器场景）
- Windows Service via nssm（Scheduled Task 够用）
- 终端 Ctrl+C 优雅关闭（直接关 PTY 更确定）
- Windows toast 安全通知（非 Windows 为主的舰队）

## Rust 已领先项（保持）

- v2 协议 4 阶段 FSM（WsV2→WsV1→HttpV2→HttpV1）
- HTTP 代理 SOCKS5/SOCKS5h/CIDR bypass
- GPU sysfs DRM 后备层 + macOS VRAM 提取
- HTTP ping 主动轮询
- subprocess 全链路 30s 超时 + kill
- 结构化错误类型（GpuDetectErr, WsErr, ProtocolFsm）
