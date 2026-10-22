# Implementation contract
Last updated: 2026-10-22

The current agent uses a synchronous collection loop, v2 JSON-RPC over WS/HTTP,
OS-native TLS roots and explicit feature/runtime permission gates. Exec and
interactive terminals are independent permissions; startup credentials are
redacted, while server-requested version switching additionally requires the
self-update feature and enabled automatic-update permission.

Current implementation and design decisions are maintained in
[Architecture](../architecture.md), with the
[support matrix](../COMPARISON.md), [tests](../TESTING.md) and
[upstream tracking](../../ROADMAP.md).

This path is retained for existing references. The original completed planning
material is historical, not an implementation or deployment contract. Its source
history remains available in Git; private operational records are maintained
outside this public repository.
