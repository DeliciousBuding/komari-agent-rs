# Risk Assessment — P8 WebSSH Harden

最后更新：2026-07-15 20:30

## Technical Risks

| Risk | Mitigation |
|------|------------|
| Deflate inflate 与 gorilla 不兼容 | 检测 Protocol 错误 → `disable_compression=true` 重连 |
| 开 terminal 即开 root shell | 默认 feature off；并发 ≤2；空闲 30min 关 |
| exec 与 WebSSH 混门 | 新增独立 `disable_exec`；JSON 缺省跟 `disable_web_ssh` |
| 2FA 强制阻断未绑用户 | `KOMARI_SENSITIVE_2FA_OPTIONAL` 应急；admin 应 enroll |
| Release 只出 full | CI 增加 `linux-x86_64-default` 资产 |

## S.U.P.E.R Health (P8 scope)

| Principle | Score | Notes |
|-----------|:-----:|-------|
| S | 🟢 | deflate 降级在 reconnection；2FA 在 AuthSensitive |
| U | 🟢 | config → runtime_cfg 单向可变 |
| P | 🟡 | capabilities 仅日志广告；server 未消费 |
| E | 🟢 | 门闩 env/JSON/CLI |
| R | 🟢 | feature gate + disable flags |

## Violation Hotspots (deferred)

- 手写 inflate 完整兼容 gorilla（根因修复）
- agent.pull 上报 capabilities 到 server
- Dashboard 按 capability 隐藏终端按钮
