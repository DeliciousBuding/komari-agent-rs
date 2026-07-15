# MASTER.md — komari-agent-rs

最后更新：2026-07-15 21:55

## 任务标识

- **任务名**：P8 WebSSH Harden + Deflate + Release
- **描述**：在 terminal 接线与 us1 E2E 之后，硬化压缩/门闩/限流/2FA/双资产发布
- **追踪模式**：`GITHUB_STANDARD`（Issues + Milestones + Labels；token 无 `project` scope）
- **仓库**：`DeliciousBuding/komari-agent-rs`（server 改动在 `DeliciousBuding/tokendance-komari`）

## 当前状态

- **当前阶段**：P8 ✅ 实现与文档已合入 main（`8ab7ae2` / server `1de0d91`）
- **Milestone**：https://github.com/DeliciousBuding/komari-agent-rs/milestone/9
- **Tests**：p8_tests 3/3；AuthSensitive_test 3/3

## P8 任务

| ID | 状态 | 说明 |
|----|:----:|------|
| P8.1 Deflate 自动降级 | ✅ | runtime disable_compression |
| P8.2 disable_exec | ✅ | 独立于 WebSSH |
| P8.3 capabilities | ✅ | 启动日志 |
| P8.4 并发/空闲 | ✅ | 2 / 30min |
| P8.5 强制 2FA | ✅ | 未 enroll 拒绝 |
| P8.6 Release 双资产 | ✅ | workflow |
| P8.7 测试 | ✅ | unit |
| P8.8 文档 | ✅ | STATE/runbook/本文件 |

## 文档索引

| 路径 | 内容 |
|------|------|
| `docs/analysis/p8-project-overview.md` | P8 概览 |
| `docs/analysis/p8-risk-assessment.md` | 风险 + S.U.P.E.R |
| `docs/plan/p8-task-breakdown.md` | 任务表 |
| `docs/progress/MASTER.md` | 本文件 |
| server `docs/runbooks/komari-webshell.md` | 运维 |

## 历史

P1–P7 已完成（见下文历史表 / GitHub milestones 1–7）。

## Governance

| 表面 | 路径 |
|------|------|
| 共享指令 | `AGENTS.md` |
| Claude | `CLAUDE.md` |
| 运维 SSOT | `~/server/projects/komari/STATE.md` |

## 下一步

1. ~~commit + push agent-rs~~ main@ba68147+
2. ~~更新 server STATE / runbook~~
3. us1 compression ON 已 E2E（deflate trailer 根因已修）
4. **打 tag `v0.2.1` 触发 dual release**（本轮）
5. 舰队仍默认 v0.1.10 HTTP-only；仅 us1 full 试点
