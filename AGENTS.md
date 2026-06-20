# AGENTS.md

最后更新：2026-06-20

## 身份

- 项目：komari-agent-rs — Komari 监控 Agent 的极致轻量 Rust 实现
- 仓库：`DeliciousBuding/komari-agent-rs`
- 全局治理：`C:\Users\Ding\AGENTS.md`

## 项目约束

1. **硬约束**：二进制 <1 MB（Linux stripped），稳态 RSS <3 MB，外部依赖仅 rustls + ring
2. **并发模型**：sync 单线程，事件循环驱动。禁止引入 tokio/async
3. **工程方法**：spec-driven develop，6 阶段管线
4. **参考代码**：`D:/Code/Projects/external/komari-agent-go`（Go 原版）、`komari-zig-agent`（Zig 版）、`komari-monitor-rs`（Rust 现有版）
5. **功能 spec**：Go 原版 100% 复刻，不做子集

## 核心文档

| 文档 | 内容 |
|------|------|
| `docs/plan/spec.md` | 已确认设计决策（14 项） |
| `docs/plan/architecture-reference.md` | 完整架构蓝图（2707 行） |
| `docs/plan/task-breakdown.md` | 51 个实现任务 |
| `docs/plan/dependency-graph.md` | 依赖图 + S.U.P.E.R 评分 |
| `docs/plan/milestones.md` | 6 阶段里程碑 + Gate 标准 |

## Git 规则

1. 默认分支 `main`，小范围提交，及时 push
2. commit message 使用英文，格式：`feat:` / `fix:` / `docs:` / `refactor:` / `test:` / `chore:`
3. 每个独立 task 完成后必须 commit
