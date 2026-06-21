---
name: Feature request
about: Suggest a new feature or improvement
title: "[feat] "
labels: enhancement
---

## Problem

What problem does this solve? Who benefits?

## Proposed solution

What would you like the agent to do?

## Alternatives considered

What other approaches did you consider?

## Fits the featherweight philosophy?

This project is deliberately dependency-free in the hot path (no serde/clap/tokio), sync single-threaded, binary < 2 MB. Does your proposal:

- [ ] Avoid adding a heavy dependency?
- [ ] Keep the sync single-threaded model?
- [ ] Stay within the binary-size budget?
