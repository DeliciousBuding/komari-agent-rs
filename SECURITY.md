# Security Policy

## Supported versions

Only the latest release line receives security fixes.

| Version | Supported |
|---------|:---------:|
| 0.1.x   | ✅        |
| < 0.1   | ❌        |

## Reporting a vulnerability

**Please do NOT open a public GitHub issue for security vulnerabilities.**

Instead, email **delicious233@hnu.edu.cn** with the subject `[komari-agent-rs security]` and include:

- A description of the issue and its impact
- Steps to reproduce (proof-of-concept if possible)
- Affected versions, if known

You will receive an acknowledgment within **48 hours** and a timeline for a fix. Please allow time for coordinated disclosure before publishing details.

## Disclosure policy

- We investigate and confirm the report.
- A fix is prepared on a private branch and released in a new version.
- The vulnerability is disclosed publicly in [CHANGELOG.md](CHANGELOG.md) and a GitHub Security Advisory **after** the fixed release is available, crediting the reporter (unless they prefer to remain anonymous).

## Scope

This policy covers the `komari-agent-rs` agent binary and its source in this repository. For vulnerabilities in the upstream Komari **server** ([`komari-monitor/komari`](https://github.com/komari-monitor/komari)), report to the upstream maintainers.
