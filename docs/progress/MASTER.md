# MASTER.md — komari-agent-rs

最后更新：2026-06-20

## 任务标识

- **任务名**：komari-agent-rs — Komari 监控 Agent 极致轻量 Rust 重写
- **描述**：从 Go 原版 100% 复刻功能到 Rust，sync 单线程，二进制 <1MB，RSS <3MB
- **追踪模式**：`GITHUB_STANDARD`（Issues + Milestones + Labels，无 Project board）
- **仓库**：`DeliciousBuding/komari-agent-rs`
- **工程管线**：spec-driven develop（6 阶段）

## 当前状态

- **当前阶段**：Phase 4 — Progress Tracking
- **上一阶段**：Phase 3 ✅ Task Decomposition
- **下一阶段**：Phase 5 — Execution

## 文档索引

### 分析（Phase 1）

| 文档 | 内容 |
|------|------|
| `docs/analysis/project-overview.md` | Go 原版架构全景 |
| `docs/analysis/module-inventory.md` | Go 原版 10 模块逐文件分析 |
| `docs/analysis/risk-assessment.md` | 重写风险矩阵 + S.U.P.E.R 评估 |
| `docs/analysis/chatgpt-architecture-advice.md` | ChatGPT 架构建议（29K 字符） |

### 规划（Phase 2-3）

| 文档 | 内容 |
|------|------|
| `docs/plan/spec.md` | 14 项已确认设计决策 + 硬约束 |
| `docs/plan/architecture-reference.md` | 完整架构蓝图（2707 行） |
| `docs/plan/task-breakdown.md` | 51 个实现任务 |
| `docs/plan/dependency-graph.md` | 依赖图 + S.U.P.E.R 评分卡 |
| `docs/plan/milestones.md` | 6 阶段里程碑 + Gate 标准 |

## 里程碑

| Phase | Milestone | GitHub | 任务数 | 状态 |
|------|------|------|:-----:|:----:|
| P1 | Foundation + Handshake | [M1](https://github.com/DeliciousBuding/komari-agent-rs/milestone/1) | 10 (#1-#10) | ⬜ |
| P2 | Linux Metrics + Zero-Alloc | [M2](https://github.com/DeliciousBuding/komari-agent-rs/milestone/2) | 12 (#11-#22) | ⬜ |
| P3 | Protocol FSM + Fallback | [M3](https://github.com/DeliciousBuding/komari-agent-rs/milestone/3) | 7 (#23-#29) | ⬜ |
| P4 | Cross-Platform + GPU | [M4](https://github.com/DeliciousBuding/komari-agent-rs/milestone/4) | 7 (#30-#36) | ⬜ |
| P5 | Terminal + Ping + Tools | [M5](https://github.com/DeliciousBuding/komari-agent-rs/milestone/5) | 9 (#37-#45) | ⬜ |
| P6 | Polish + Packaging | [M6](https://github.com/DeliciousBuding/komari-agent-rs/milestone/6) | 6 (#46-#51) | ⬜ |
| **Total** | | | **51** | |

## 快速状态命令

```bash
gh issue list --repo DeliciousBuding/komari-agent-rs --limit 60
gh milestone list --repo DeliciousBuding/komari-agent-rs
```

## Governance 状态

| 表面 | 路径 | 状态 |
|------|------|:--:|
| 共享指令 | `AGENTS.md` | ✅ 已创建 |
| Claude Code 指令 | `CLAUDE.md` | ✅ 已创建 |
| 记忆表面 | 由编码 agent 原生管理 | ✅ |

## 下一步

1. 创建 GitHub Labels + Milestones + Issues
2. 进入 Phase 5 — 开始执行 P1 任务
