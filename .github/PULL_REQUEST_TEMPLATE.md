## Summary

Brief description of what this PR changes and why.

## Motivation

Link the issue this closes (e.g. `Closes #123`), or describe the problem.

## Checklist

- [ ] `cargo fmt`
- [ ] `cargo test` passes
- [ ] `cargo test --all-features` passes
- [ ] `cargo clippy --release -- -D warnings` is clean
- [ ] Release binary stays under **2 MB** (release.yml enforces this)
- [ ] **No new heavy dependency** — if one is added, justify why std can't do it
- [ ] [CHANGELOG.md](../CHANGELOG.md) updated for user-visible changes

## Notes for review

Anything reviewers should pay attention to (edge cases, perf, platform-specific behavior).
