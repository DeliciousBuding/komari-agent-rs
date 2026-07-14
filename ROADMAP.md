# komari-agent-rs ROADMAP
最后更新：2026-07-14

> 基于 Go upstream (komari-monitor/komari-agent v1.2.13) 5 轮深度对比分析。

## v0.2 ✅ 已完成（2026-07-14）

| Issue | 功能 | 状态 |
|:-----:|------|:----:|
| #62 | GPU 详细指标 (utilization/temp/DXGI metadata) | ✅ |
| #63 | 高延迟重试 + TCP 重传检测 | ✅ |
| #64 | 虚拟 GPU 过滤 (virtio/vmware/qxl等) | ✅ |
| #65 | 容器检测增强 (podman/CRI-O/LXC) | ✅ |
| #66 | GPU 驱动名映射 (i915→"Intel"等) | ✅ |
| #67 | nvidia-smi/rocm-smi 路径检测 | ✅ |

## v0.3 📋 待开发（18 issues, Milestone #8）

### P0 — 数据正确性

| Issue | 问题 | 类型 |
|:-----:|------|:----:|
| #68 | 内存 htop 模式缺 shmem | bug |
| #69 | 网络计数器溢出用 wrapping_sub | bug |
| #70 | 连接数只读 /proc/net 无回退 | bug |
| #71 | IP 检测不支持 HTTPS | bug |
| #80 | Self-update 资产名不匹配+缺平台 | bug |
| #84 | 12+ subprocess 调用无超时 | bug |
| #85 | i64→u64 负数回绕破坏 ping hashmap | bug |
| #86 | Windows 非原子二进制替换可能 brick | bug |

### P1 — 功能缺口

| Issue | 问题 | 类型 |
|:-----:|------|:----:|
| #72 | 磁盘缺挂载点前缀过滤 | bug |
| #73 | 网卡过滤缺 7 个前缀 | bug |
| #74 | Swap 不减 SwapCached | bug |
| #75 | Windows/FreeBSD 物理核心数不准 | bug |
| #81 | FreeBSD TLS 缺标准 CA 路径 | bug |
| #82 | DNS 自定义+内置列表始终合并 | bug |
| #87 | JSON 数字提取匹配到字符串内部 | bug |

### P2 — 运维完善

| Issue | 问题 | 类型 |
|:-----:|------|:----:|
| #76 | 无 CI/CD pipeline | enhancement |
| #77 | IP fallback 链偏少 | enhancement |
| #78 | Self-update 缺快照轨道+轮询 | enhancement |
| #79 | Netstatic 无逐接口追踪 | enhancement |
| #83 | CI 只测 default+all-features | bug |
| #88 | Permessage-deflate 无输出上限 | bug |
| #89 | /dev/urandom expect() panic | bug |

## 不追

- Auto-discovery（舰队手动部署）
- SoC/嵌入式 GPU Device Tree（非服务器场景）
- Windows Service via nssm（Scheduled Task 够用）
- 终端 Ctrl+C 优雅关闭（直接关 PTY 更确定）
- Windows toast 安全通知（非 Windows 为主的舰队）

## Rust 已领先项（保持）

- v2 协议 4 阶段 FSM（WsV2→WsV1→HttpV2→HttpV1）
- HTTP 代理 SOCKS5/SOCKS5h/CIDR bypass
- GPU sysfs DRM 后备层 + macOS VRAM 提取
- HTTP ping 主动轮询
- subprocess 超时+kill（但仅限 task.rs，monitor 模块未覆盖）
- 结构化错误类型（GpuDetectErr, WsErr, ProtocolFsm）
