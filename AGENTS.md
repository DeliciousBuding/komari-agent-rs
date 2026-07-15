# AGENTS.md

最后更新：2026-07-15

## 身份

- 项目：komari-agent-rs — Komari 监控 Agent 的极致轻量 Rust 实现
- 仓库：`DeliciousBuding/komari-agent-rs`
- 参考代码（仅在本地 dev 环境，不在本仓库内）：Go 原版、Zig 版、Rust 现有版

## 项目约束

1. **硬约束**：二进制 <1 MB（Linux stripped），稳态 RSS <3 MB，外部依赖仅 rustls + ring
2. **并发模型**：sync 单线程，事件循环驱动。禁止引入 tokio/async
3. **工程方法**：spec-driven develop，6 阶段管线
4. **功能 spec**：Go 原版 100% 复刻，不做子集

## 核心文档

| 文档 | 内容 |
|------|------|
| `docs/plan/spec.md` | 已确认设计决策（14 项） |
| `docs/plan/architecture-reference.md` | 完整架构蓝图（2707 行） |
| `docs/plan/task-breakdown.md` | 51 个实现任务 |
| `docs/plan/dependency-graph.md` | 依赖图 + S.U.P.E.R 评分 |
| `docs/plan/milestones.md` | 6 阶段里程碑 + Gate 标准 |
| `docs/TESTING.md` | 验证矩阵；含 2026-07-15 WebSSH us1 E2E |
| `CHANGELOG.md` | 发布与 Unreleased |

## 生产约束（WebSSH / 控制面）

1. **默认关**：`default=[]` 无 `terminal`；`disable_web_ssh=true`；`disable_exec=true`；舰队多数 `--http-only`
2. **消防斧启用**需：`--features terminal|full` + 非 http-only + `disable_web_ssh=false` + 公网 nginx Upgrade
3. **`disable_exec` 与 `disable_web_ssh` 完全独立**（默认均为 true）；JSON **不得**互相同步。开 WebSSH 不等于开 one-shot exec
4. **WS 压缩**：inflate 必须使用 gorilla trailer（`00 00 FF FF` + `01 00 00 FF FF`）。失败自动关压缩仅作兜底
5. **终端例外**：sync 事件循环是主路径；interactive terminal 允许最多 **2** 个 detached PTY 线程（空闲 30min 关）
6. 运维 SSOT：`~/server/docs/runbooks/komari-webshell.md` + `~/server/projects/komari/STATE.md`

## Git 规则

1. 默认分支 `main`，小范围提交，及时 push
2. commit message 使用英文，格式：`feat:` / `fix:` / `docs:` / `refactor:` / `test:` / `chore:`
3. 每个独立 task 完成后必须 commit
