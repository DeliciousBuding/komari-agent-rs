# Task Breakdown — P8 WebSSH Harden

最后更新：2026-07-15 20:30  
Mode: GITHUB_STANDARD | Milestone: [P8](https://github.com/DeliciousBuding/komari-agent-rs/milestone/9)

| ID | Task | Pri | Size | Status | Notes |
|----|------|:---:|:----:|:------:|-------|
| P8.1 | Deflate auto-degrade | P0 | M | done | `is_deflate_failure` + runtime_cfg |
| P8.2 | disable_exec 独立门闩 | P0 | S | done | CLI/env/JSON + legacy follow web_ssh |
| P8.3 | capabilities 广告 | P1 | S | done | 启动日志；terminal 条件化 |
| P8.4 | terminal 并发+空闲超时 | P1 | M | done | max 2 sessions；30min idle |
| P8.5 | Server 强制 2FA enrollment | P0 | S | done | tokendance-komari AuthSensitive |
| P8.6 | Release dual assets | P1 | M | done | default + full linux musl |
| P8.7 | Tests | P0 | S | done | p8_tests + AuthSensitive_test |
| P8.8 | Docs / STATE / runbook | P1 | S | in progress | 同步生产事实 |

## Acceptance (all)

- [x] `cargo test --features terminal` p8_tests 绿
- [x] `go test ./web/api` 绿
- [ ] us1 可选：去掉 `--disable-compression` 后 deflate 失败会自动降级（不阻塞合并）
- [ ] tag 触发 Release 出 `*-default` 与 `*-full`

## Parallel lanes

- Lane A: agent-rs (P8.1–4,6–7)
- Lane B: tokendance-komari 2FA (P8.5)
- Merge: docs (P8.8)
