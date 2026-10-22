# AGENTS.md

最后更新：2026-10-22

## 身份

- 项目：komari-agent-rs — Komari 监控 Agent 的极致轻量 Rust 实现
- 仓库：`DeliciousBuding/komari-agent-rs`
- 行为参考（仅在本地环境，不在本仓库内）：官方 Go agent 与 Komari server

## 项目约束

1. **发布预算**：每个受支持 release 资产的二进制必须 <2 MiB；RSS 必须按同一平台、feature、根证书和负载实测，不发布跨机器固定承诺
2. **并发模型**：同步采集/控制主循环；仅 HTTP pull 与交互终端使用受限辅助线程。禁止引入 tokio/async runtime
3. **兼容原则**：以 Komari 当前 v2 wire contract 和 Go agent 行为为默认；安全/可靠性偏离必须有明确理由、文档和回归测试
4. **依赖原则**：保持 rustls + ring 与现有同步架构，不为假想需求引入框架、重依赖或并行实现

## 核心文档

| 文档 | 内容 |
|------|------|
| `docs/architecture.md` | 当前模块边界、调度、安全与资源约束 |
| `docs/COMPARISON.md` | 功能/平台支持和与 Go agent 的差异 |
| `docs/TESTING.md` | 本地、CI 与真实 Komari 验收边界 |
| `ROADMAP.md` | 已审核上游基线、取舍和剩余范围 |
| `CHANGELOG.md` | 发布事实和升级说明 |

`docs/plan/` 只保留历史规划入口，不构成当前实现或部署契约。

## 生产约束（WebSSH / 控制面）

1. **默认关**：`disable_web_ssh=true`、`disable_exec=true`、`disable_auto_update=true`；默认构建不含 terminal，也不自行开启远程控制
2. **终端启用**需 terminal feature、非 `http_only`、`disable_web_ssh=false` 和可升级的 WebSocket 入口
3. **文件控制**受 `disable_web_ssh` 控制；**远程切版**还要求 self-update/full 构建与 `disable_auto_update=false`
4. **`disable_exec` 与 `disable_web_ssh` 完全独立**，开 WebSSH 不等于开 one-shot exec
5. **WS 压缩**必须兼容 Gorilla trailer（`00 00 FF FF` + `01 00 00 FF FF`），压缩失败只回退压缩，不降级认证或权限
6. **终端资源边界**：最多 2 个 detached PTY，空闲 30 分钟关闭
7. 生产主机、版本、token 与 supervisor 细节只写内部运维 SSOT，不进入公开仓

## Git 规则

1. 默认分支 `main`（主线）；功能/修复走 `feature/*` 或 `fix/*` 分支，完成后 PR 合并回 `main`
2. 发布：`main` 上打 `v*` tag 触发 release workflow（matrix 并行出全平台二进制）
3. 小范围提交，及时 push；commit message 英文，`feat:` / `fix:` / `docs:` / `refactor:` / `test:` / `chore:`
4. **CI 为主**：发布前必须通过 GitHub Actions 的 check/test/lint/build/coverage/audit；本地按风险和故障复现补充格式化、clippy、定向回归与真实隔离验收
