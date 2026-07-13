# Security Report (M5)

A code-level review of the security-relevant paths. Not an external audit.

## Model (recap)
QUIC (TLS 1.3) gives the encrypted pipe; **identity is the app layer**: mutual
X25519 handshake + HMAC key confirmation, then `SecureLink` (per-frame
AES-256-GCM, monotonic-nonce). Trust is TOFU-pinned. See [Security](SECURITY.md).

## Findings

| Area | Result |
|---|---|
| Authentication | ✅ Mutual X25519 + key confirmation; directional keys by pubkey ordering; transcript binds both pubkeys + fresh nonces. |
| SecureLink | ✅ AEAD per frame; monotonic counter → replay/reorder rejected before decrypt (tested). |
| TLS cert validation | ⚠️ **By design** accept-any server cert (no PKI); identity via the app-layer handshake. QUIC alone is encrypted-but-unauthenticated — documented. |
| TOFU | ✅ Fingerprint pinned on first contact; changed key rejected (tested). Ephemeral per-run identity on CLI/FFI → re-pins each run (known limitation). |
| MITM | ✅ Resisted once a peer is pinned; first contact is trust-on-first-use (compare fingerprints out-of-band for stronger assurance). |
| Path traversal | ✅ Receive sanitizes to a base name (single file) / `sanitize_rel` rejects `..`+absolute (folders). Tested. |
| Symlink attacks | ✅ Folder walk uses `entry.file_type()` (lstat-like) → symlinks are neither file nor dir → **skipped, never followed** (no exfiltration). Verified by test. |
| Unsafe writes | ✅ Single-file receive: `.part` → verify SHA-256 → atomic no-clobber finalize, `0600` (Unix). |
| Temporary files | ✅ `.part` in the destination dir, promoted atomically; left resumable on failure. |
| Permissions | ✅ `0600` on finalized files (Unix). Windows uses default ACLs (documented gap). |
| Replay protection | ✅ SecureLink counter (tested: replayed + tampered frames rejected). |
| Clipboard | ✅ FFI clipboard is a local slot; images are metadata-only (no buffers cross). No network clipboard receive yet. |
| Daemon | ✅ Receive server authenticates every inbound connection before any transfer; approval gate (accept/reject) for incoming. |
| Logging | ✅ Structured; no keys/payloads logged. |
| Crash recovery | ✅ Interrupted transfers resume from the `.part` offset with a fresh SHA-256 verify. |

## Low-risk items (no fix required for beta)
- **Folder receive writes to `dest_path` directly** (no `.part` there): an
  attacker with pre-existing write access to the *receiver's* save dir could
  plant a symlink to redirect a write. Requires local access to the victim's
  save dir → low risk; hardening (O_NOFOLLOW / `.part` for folders) is a
  follow-up.
- **Windows file permissions** are not restricted (`0600` is Unix-only).
- **Ephemeral identity** weakens cross-session TOFU continuity (per-session
  security is intact).

## Verified fixes / no critical issues
No critical vulnerabilities found. Path traversal, symlink exfiltration, replay,
and tamper are all handled and covered by tests.
