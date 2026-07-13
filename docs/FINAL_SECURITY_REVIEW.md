# Final Security Review — M8

Independent security review against the current implementation. Complements
[Security](SECURITY.md) and [Security Report](SECURITY_REPORT.md); this is the
release-gate pass.

Legend: ✓ Verified here · 🟡 Code-reviewed · ⚪ Environment-limited.

## Summary

No critical or high-severity issue found. The security model is sound and
consistently implemented. Two items are release-relevant and both are
**environment-limited**, not defects: a dependency vulnerability scan could not
be run here, and the multi-host attack surface (real MITM/replay across a live
network) was not exercised beyond one prior live transfer.

## Authentication & identity

- Mutual authentication is at the **application layer**: X25519 ECDH →
  directional session keys → HMAC-SHA256 **key confirmation** binding both
  public keys and both fresh nonces (`peerbeam-transfer/auth.rs`). Verifying the
  peer's MAC proves possession of the private key it presented. 🟡 Roundtrip +
  tamper unit tests pass ✓.
- **TOFU trust**: peer fingerprint pinned on first contact via the trust store;
  a changed fingerprint on reconnect is rejected. 🟡
- **Identity persistence** — device identity is not yet persisted across
  restarts (tracked in [Known Issues](KNOWN_ISSUES.md)); a restarted peer
  re-pins. Acceptable for Beta/RC, noted for 1.0 follow-up.

## Encryption / transport

- **Data**: every frame after the handshake is sealed with AES-256-GCM under a
  monotonic-counter nonce (4-byte direction prefix ‖ 8-byte BE counter). GCM
  tag = integrity; counter = replay/reorder protection; contents encrypted.
  🟡 Seal/open + replay-rejection unit tests pass ✓.
- **Transport TLS**: QUIC uses an **accept-any** certificate verifier
  (`transfer-quic/tls.rs`) **by design** — the TLS layer provides an encrypted
  pipe, not peer authentication; authentication is the app-layer handshake
  above. This is a deliberate, documented choice (comparable to app-layer-auth
  designs). It is **not** a vulnerability *provided* the app-layer handshake is
  always run before data — which the transfer/example code enforces. 🟡
  Reviewed; call-site enforcement confirmed in `send_file`/`receive_file` paths.

## Filesystem, permissions, temp files

- Received files are written to a temporary path, `chmod 0o600` (owner-only),
  then **atomically renamed** into place with clobber-avoidance
  (`storage-fs/lib.rs`). ✓ Asserted by unit test (`0o600` finalized perms).
- **Path traversal**: folder transfer sanitizes every relative path
  (`sanitize_rel`/`sanitize_name` — no `..`, no absolute) before writing under
  `dest_dir/<root>/`; symlinks are skipped, never followed. 🟡 Edge-case test
  (`folder_edge.rs`) covers unicode/long/hidden/deep + a symlink-escape attempt.
  ✓

## Replay protection

Counter-nonce scheme refuses duplicated/out-of-order frames (see Encryption).
🟡 unit-tested ✓.

## Clipboard

Current clipboard adapter is **in-memory** (`peerbeam-clipboard-mem`) — no
disk persistence, so no clipboard-at-rest exposure. 🟡

## Logging

No secret material logged: grep of all `tracing` macros for
key/secret/token/password/pubkey returned **nothing**. ✓ Matches the "never log
sensitive data" requirement.

## FFI boundary

- Panic-safe: every `extern "C"` function runs inside `catch_unwind` (`guard()`
  in `lib.rs`); an internal panic becomes an error envelope, never UB across the
  boundary. 🟡 verified by inspection.
- String ownership is uniform (Rust allocates, caller frees via
  `pb_free_string`). No double-free path observed.

## Configuration

Settings and trust are stored under the platform app dir / FFI-provided base
path; no cloud, no telemetry, no analytics (confirmed by absence of any network
telemetry client). ✓

## Dependency vulnerabilities

⚪ **Environment-limited.** `cargo-audit` is not installed and network access
for the advisory DB is not available in this environment, so a CVE scan could
**not** be run. `cargo generate-lockfile` flagged newer majors for `socket2`
and `x25519-dalek` (availability, not advisories). **Action for release:** run
`cargo audit` (and `cargo deny`) in CI before tagging — `ci.yml` is the place to
add it.

## Findings

| # | Area | Finding | Severity | Status |
|---|---|---|---|---|
| 1 | Deps | No vuln scan run (no scanner/network) | Medium | ⚪ Add `cargo audit` to CI before tag |
| 2 | Identity | No persistent device identity | Low | Tracked, 1.0 follow-up |
| 3 | Transport | Accept-any TLS (by design; app-layer auth) | Info | 🟡 Documented, not a defect |

## Verdict

Security posture is **release-grade** for the verified surface. The one true
gap is process (run a dependency scanner in CI) — not a code vulnerability. No
security issue blocks a Release Candidate.
