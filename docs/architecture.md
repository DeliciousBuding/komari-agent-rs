# Agent architecture
Last updated: 2026-10-22

The agent has one synchronous collection/control loop. HTTP pull and optional interactive terminals use auxiliary threads; there is no async runtime. The report encoder uses reusable stack buffers, while TLS, RPC envelopes, compression and some collectors allocate. Binary and memory measurements must name platform, features and workload.

## Module boundaries

| Module | Responsibility |
|---|---|
| config | CLI/environment/JSON loading and effective settings; explicit migration validation |
| monitor | Platform collectors and metric encoding; previous-sample state belongs to Monitor |
| protocol/v2 | JSON-RPC envelopes and success/error interpretation |
| protocol/fsm | WebSocket-to-HTTP fallback and preferred-transport recovery |
| server | Reporting, pull/event acknowledgement, dispatch and result transport |
| json | Bounded encoder and shared strict cold-path object/string scanning |
| http / ws / tls | Transport framing, proxy dialing, certificate verification and OS roots |
| process | Bounded subprocess input/output/lifecycle shared by collectors and exec |
| update | Feature-gated, verified release installation and restart behavior |

Collectors do not manage node registration, server settings or transport policy. Event handlers enforce runtime permission gates even when a server sends an unadvertised capability.

## Transport and scheduling

WebSocket and HTTP use the same v2 methods at `/api/clients/v2/rpc`. Three consecutive WebSocket failures select HTTP; HTTP failures keep retrying with backoff. A healthy HTTP fallback yields periodically to retry WebSocket, while `http_only` remains HTTP.

Reports follow the configured interval (default 3 seconds). Short control polling and 30-second WebSocket heartbeats remain independent of long report intervals. HTTP pull uses a generation owner: stale responses cannot dispatch events after their session ends. Events are deduplicated and acknowledged; result calls check both HTTP status and JSON-RPC errors.

Server deadlines remain version-specific: 1.5.1 requires application reports
within its 11-second WS deadline and does not refresh it on Ping; 1.6.0 refreshes
its 75-second deadline on Ping. Long-interval 1.5.1 sessions may select HTTP;
the agent does not silently change the requested reporting cadence.

The monitor sends `agent.report` with a `report` wrapper, and periodically sends `agent.basicInfo`. This wrapper is part of the wire contract, not an optional formatting choice.

## Permissions and TLS

One-shot exec and interactive terminals have separate default-deny switches. A terminal additionally needs its compiled feature and a WebSocket-capable configuration. Server-requested version changes remain refused. Startup-config reporting includes build/features and effective settings but redacts token and discovery credentials.

OS roots are loaded at process startup. rustls-webpki validates paths, server names, validity times and explicit public-key distrust; removing a root from a file does not refresh an existing process. Monitoring's explicit unsafe-cert override defeats authentication and is not an acceptable deployment default. Executable downloads always use normal verification.

## Resource boundaries

| Input or operation | Bound |
|---|---|
| HTTP control response | 1 MiB |
| JSON configuration file | 1 MiB, 32-level shared validation bound, no duplicate keys |
| WebSocket frame, reassembled message and inflated control message | 1 MiB each |
| Subprocess captured stdout + stderr | 256 KiB |
| Exec | 30-second execution/input/output deadline |
| Interactive terminals | At most 2 sessions; idle timeout |
| Self-update | JSON 1 MiB, checksums 64 KiB, binary 8 MiB; 5 HTTPS redirects |

The subprocess runner uses nonblocking pipes and a process group on Unix, and a suspended launch followed by Job Object assignment on Windows. It creates no reader/writer threads. Ordinary descendants are terminated when the root exits or a limit is hit; truncated collector output is rejected. A Unix command that deliberately escapes its process group is outside this lifecycle boundary: this is not a sandbox.

The release profile uses panic-abort. Input validation and supervisor restart are the recovery boundary; catching unwinds is not a substitute for checking parser and resource limits. Kernel-uninterruptible I/O is outside hard real-time termination guarantees.

## Updates and integrity

Installers stage and verify downloads before replacing an existing binary, preserve configuration, and keep a rollback binary. Credentials are not embedded in a scheduled task action. SHA-256 checksums detect corruption; a manifest from the same publisher is not an independent signature.

Optional self-update is disabled by default. It follows bounded HTTPS redirects and strips authentication after an origin change. Unix installs atomically then re-execs with original arguments; failure restores the previous path. Windows installs for the next supervisor launch and explicitly reports restart pending.

## Compression

HTTP reports use gzip when enabled. WebSocket compression follows negotiated permessage-deflate and never compresses control frames. The decoder supports stored, fixed and dynamic DEFLATE blocks with bounded expansion. Gorilla's non-final deflate trailer requires both the sync-flush bytes and a final empty stored block; fixture tests preserve this interoperability detail.

The WS reader retains partial header/payload progress across read timeouts.
Continuation frames are reassembled before decoding/decompression, with Ping/Pong
handled between fragments. Invalid sequences, truncated frames and resets cause
reconnection rather than interpreting remaining bytes as a new header.

## Upstream decisions

Current upstream references and feature decisions are in [ROADMAP](../ROADMAP.md); [support coverage](COMPARISON.md) and [tests](TESTING.md) describe what is verified. Historical implementation plans are not the current architecture contract.
