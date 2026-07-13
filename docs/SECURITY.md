# Security

The secure-transfer layer sits between a raw `Link` (any transport) and the
file/folder/clipboard transfer logic. It provides mutual authentication,
per-frame keyed integrity + confidentiality, replay protection, and safe
file writing — all transport-agnostic.

## Building blocks

| Concern | Where | Primitive |
|---|---|---|
| Key agreement | `peerbeam-crypto` (`EncryptionProvider`) | X25519 ECDH → directional session keys |
| Sealing | `peerbeam-crypto` | AES-256-GCM (`nonce ‖ ciphertext+tag`) |
| Fingerprints | `peerbeam-crypto` | SHA-256 of public key (hex) |
| Trust store | `peerbeam-trust-fs` (`TrustStore`) | TOFU fingerprint pinning (JSON) |
| Handshake | `peerbeam-transfer::authenticate` | authenticated ECDH + HMAC key confirmation |
| Secure framing | `peerbeam-transfer::SecureLink` | sealed frames, monotonic-counter nonce |

## Mutual authentication

Run once per connection, symmetric on both ends:

```
A→B  Hello{ device_id, name, pubkey_A, nonce_A }
B→A  Hello{ device_id, name, pubkey_B, nonce_B }
A→B  Confirm{ HMAC(send_key, transcript) }
B→A  Confirm{ HMAC(send_key, transcript) }
```

Both derive the same ECDH shared secret and split it into **directional**
keys (assignment fixed by comparing the two public keys, so no negotiated
role). The `Confirm` MAC is **key confirmation**: verifying the peer's MAC
with our receive key proves the peer computed the same secret — i.e. holds
the private key for the public key it presented. The transcript binds both
public keys and both fresh nonces.

**Trust-on-first-use.** The peer's fingerprint is pinned on first contact.
On later connections a changed fingerprint (a new device reusing an id, or a
man-in-the-middle) is rejected. Fingerprints are meant to be compared
out-of-band for stronger assurance.

## Integrity, confidentiality, replay protection

`SecureLink` wraps the authenticated session. Every frame is sealed with
AES-256-GCM under the session send key and a nonce = `4-byte per-session
prefix ‖ 8-byte monotonic counter`:

- **Integrity** — the GCM tag authenticates each frame; a flipped bit fails
  to open.
- **Confidentiality** — frame contents are encrypted on the wire.
- **Replay / reorder** — the receiver requires the exact next counter; a
  duplicated or out-of-order frame is rejected before decryption.

Session keys are derived from the handshake transcript (fresh nonces), so
ciphertext captured from one session cannot be replayed into another.

Independently, each file transfer still verifies a **whole-file SHA-256** at
completion (defence in depth + detects on-disk corruption).

## Safe file writing

Received data is streamed to a `<name>.part` file. Only on a verified,
complete transfer is it **atomically** promoted:

- **No overwrite** — if the destination name exists, a non-colliding name is
  chosen (`file (1).ext`); existing files are never clobbered.
- **Atomic** — `rename` within the directory; readers never see a partial
  final file.
- **Restrictive permissions** — `0600` on Unix before the file becomes
  visible.
- **Failure/cancel** — the `.part` remains (resumable); the final file is
  never created.

Path names from peers are sanitized to a single base component (no `..`, no
absolute paths).

## Threat notes / scope

- The handshake authenticates *keys*; binding a key to a human-meaningful
  identity relies on TOFU + optional out-of-band fingerprint check.
- Discovery is untrusted input by design; authentication happens here, at
  transfer time, not in discovery.
- Folder receive does not yet use the `.part`/finalize path (single-file
  does); adopting it there is a follow-up.
- No real network transport (`TransferProvider`) ships yet; this layer is the
  prerequisite that must wrap any future QUIC/TCP link.

## Testing

- **Unit**: crypto (ECDH agreement + directionality, seal/open round-trip,
  tamper/wrong-key/short-input rejection, fingerprint stability); trust store
  (pin/lookup/trust, persistence, overwrite); `finalize` (rename, no-clobber,
  `0600`).
- **Integration**: mutual auth + real transfer over `SecureLink`; TOFU
  pin → trust → reject-on-key-change; `SecureLink` rejects replayed and
  tampered frames; safe write refuses to overwrite and leaves `.part` on
  integrity failure.
