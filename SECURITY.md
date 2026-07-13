# Security Policy

## Reporting a vulnerability
**Do not open a public issue for security vulnerabilities.** Report privately to
the maintainers (open a GitHub *security advisory* on the repository, or email
the maintainer address in the project profile). Include: affected version, a
description, reproduction steps, and impact.

We aim to acknowledge within a few days and to ship a fix or mitigation for
confirmed issues before public disclosure. Coordinated disclosure is
appreciated.

## Scope
PeerBeam transfers data device-to-device with no cloud. The security model
(mutual X25519 auth, per-frame AES-256-GCM via `SecureLink`, TOFU trust) is
documented in [docs/SECURITY.md](docs/SECURITY.md); the latest review is
[docs/SECURITY_REPORT.md](docs/SECURITY_REPORT.md). Known limitations live in
[docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md).

## Supported versions
See [SUPPORTED_VERSIONS.md](SUPPORTED_VERSIONS.md).
