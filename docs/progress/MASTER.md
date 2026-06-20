# MASTER.md — komari-agent-rs

最后更新：2026-06-20（P1–P6 全部完成，据 cargo-bloat 实测修订指标）

## 任务标识

- **任务名**：komari-agent-rs — Komari 监控 Agent 极致轻量 Rust 重写
- **描述**：从 Go 原版 100% 复刻功能到 Rust，sync 单线程，二进制 ~1.5 MB（自身代码 196 KB + TLS 栈 ~1 MB），RSS ~3 MB
- **追踪模式**：`GITHUB_STANDARD`（Issues + Milestones + Labels，无 Project board）
- **仓库**：`DeliciousBuding/komari-agent-rs`
- **工程管线**：spec-driven develop（6 阶段）

## 当前状态

- **当前阶段**：全部完成 ✅（P1–P6）
- **上一里程碑**：Phase 6 ✅ Polish + Packaging
- **工程状态**：6 阶段全部交付；80 源文件，162 单元测试通过；4 平台（Linux/Windows/macOS/FreeBSD）全功能对等
- **实测指标**（cargo-bloat，Linux musl release stripped）：
  - 二进制 ~1.5 MB（rustls 1.1MB + ring 528KB + webpki 262KB + std 471KB + **自身代码 196KB**）
  - RSS ~3 MB（达标，对比 Go 原版 18-32 MB）
  - 原 <1 MB 二进制目标被 TLS 栈（~1 MB 不可压）否决，已据实测修正
- **下一步**：无（任务完结；可选后续：长期稳定性观察、用户反馈收集）

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
| P1 | Foundation + Handshake | [M1](https://github.com/DeliciousBuding/komari-agent-rs/milestone/1) | 10 (#1-#10) | ✅ |
| P2 | Linux Metrics + Zero-Alloc | [M2](https://github.com/DeliciousBuding/komari-agent-rs/milestone/2) | 12 (#11-#22) | ✅ |
| P3 | Protocol FSM + Fallback | [M3](https://github.com/DeliciousBuding/komari-agent-rs/milestone/3) | 7 (#23-#29) | ✅ |
| P4 | Cross-Platform + GPU | [M4](https://github.com/DeliciousBuding/komari-agent-rs/milestone/4) | 7 (#30-#36) | ✅ |
| P5 | Terminal + Ping + Tools | [M5](https://github.com/DeliciousBuding/komari-agent-rs/milestone/5) | 9 (#37-#45) | ✅ |
| P6 | Polish + Packaging | [M6](https://github.com/DeliciousBuding/komari-agent-rs/milestone/6) | 6 (#46-#51) | ✅ |
| **Total** | | | **51** | **✅ 6/6** |

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

任务完结。可选后续：
1. 长期稳定性与内存占用观察
2. 收集用户反馈，规划下一轮迭代
3. 视需要补 cargo-bloat / 测试覆盖率自动报告到 CI
