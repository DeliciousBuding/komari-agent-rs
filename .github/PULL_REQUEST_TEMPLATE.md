## Summary

Brief description of what this PR changes and why.

## Motivation

Link the issue this closes (e.g. `Closes #123`), or describe the problem.

## Checklist

- [ ] `cargo fmt --check`
- [ ] `cargo check --locked --all-features` passes
- [ ] `cargo test --locked --all-features` passes in CI
- [ ] `cargo clippy --locked --all-features -- -D warnings` is clean
- [ ] Release binary stays under **2 MB** (release.yml enforces this)
- [ ] **No new heavy dependency** — if one is added, justify why std can't do it
- [ ] [CHANGELOG.md](../CHANGELOG.md) updated for user-visible changes
- [ ] Upstream contract, permission gates and migration notes reviewed
- [ ] Tests/logs/attachments contain no credentials or private host information

## Notes for review

Anything reviewers should pay attention to (edge cases, perf, platform-specific behavior).
