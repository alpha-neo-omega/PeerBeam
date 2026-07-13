# Transfer Protocol

The on-the-wire format PeerBeam speaks once a link is established. This is the
byte-level companion to [Transfer](TRANSFER.md) (engine behaviour) and
[Security](SECURITY.md) (crypto rationale). It is drawn directly from the
`peerbeam-transfer` crate — `auth.rs`, `secure.rs`, `protocol.rs`, `folder.rs`.

Verification legend: ✓ Verified by test · 🟡 Code-reviewed.

## Layers

A transfer rides three stacked layers over a transport (QUIC today):

```
send_file / receive_file / folder     application  (framed messages)
        ↓
SecureLink (AES-256-GCM, counter nonce)   session   (per-frame seal/open)
        ↓
Link (ordered, reliable frame stream)     transport (QUIC bi-stream)
```

Everything is expressed as **frames**. A `Frame` is a `kind` tag plus a raw
byte `payload`:

```rust
enum FrameKind { Handshake, Meta, Chunk, Ack, Control }
struct Frame { kind: FrameKind, payload: Bytes }
```

A `Link` preserves order and reliability, so frames arrive in send order. That
is why data chunks carry **no index** — the receiver appends them as they land.

## 1. Authentication handshake (once per connection)

Run before any transfer, over the raw link, in `Handshake` frames. Mutual
X25519 key agreement with trust-on-first-use (🟡; roundtrip + tamper tests in
`auth.rs`, live-verified Android→Linux):

```text
A→B: Hello{ device_id, name, pubkey_A, nonce_A }
B→A: Hello{ device_id, name, pubkey_B, nonce_B }
A→B: Confirm{ HMAC(send_key, transcript) }
B→A: Confirm{ HMAC(send_key, transcript) }
```

- Both sides compute the same ECDH shared secret and derive **directional**
  session keys (distinct send/recv keys per direction).
- `Confirm` is HMAC-SHA256 **key confirmation**: verifying the peer's MAC with
  our receive key proves the peer derived the same secret — i.e. holds the
  private key for the public key it presented. That is the mutual-auth step.
- The transcript binds both public keys and both fresh nonces, so a replayed
  handshake yields different keys.
- **TOFU**: the peer's public-key fingerprint is pinned on first contact; a
  changed fingerprint on a later connection (id reuse or MITM) is rejected.

The handshake yields a `Session` consumed by `SecureLink`.

## 2. Secure framing (every subsequent frame)

`SecureLink` wraps the link + session. Each outgoing frame is sealed with
**AES-256-GCM** under the session send key and a **monotonic-counter nonce**;
each incoming frame must carry the next expected counter and pass GCM
verification, or it is refused (🟡; seal/open + replay tests in `secure.rs`):

- **Nonce** = 4-byte direction prefix ‖ 8-byte big-endian counter (12 bytes).
- **Integrity** — the GCM tag authenticates each frame; a flipped bit fails to
  open.
- **Replay / reorder protection** — a duplicated or out-of-order frame carries
  the wrong counter and is rejected.
- **Confidentiality** — frame contents are encrypted on the wire.

Everything below (Meta/Chunk/Control) travels **inside** this sealed channel.

## 3. Single-file transfer

A strictly-ordered frame sequence on one link (✓ roundtrip tests in
`protocol.rs`):

```text
Meta(transfer_id, name, size, chunk_size)   S→R   FrameKind::Meta   (JSON)
[ Control::ResumeAck{ offset } ]            R→S   FrameKind::Control (JSON)
Chunk … Chunk                               S→R   FrameKind::Chunk   (raw bytes)
Control::Complete{ checksum }               S→R   FrameKind::Control (JSON)
Control::Verify{ ok }                       R→S   FrameKind::Control (JSON)
```

- **Meta** — announced once. `size` is informational (`0` if streamed/unknown).
- **ResumeAck** — receiver reports bytes already on disk; sender resumes from
  `offset`. Fresh transfer ⇒ `offset = 0`.
- **Chunk** — raw file bytes ride directly in `Frame::payload` (no base64, no
  JSON wrapper) so there is no per-chunk bloat. `chunk_size` is the sender's
  preference; the receiver appends in arrival order.
- **Complete** — carries the SHA-256 of the *whole* file.
- **Verify** — receiver recomputes the checksum and reports match; a mismatch
  is a terminal `Integrity` error.
- **Cancel** — either side may send `Control::Cancel` at any point to abort.

No file is ever fully loaded into RAM — chunks stream from disk and to disk.

## 4. Folder transfer

Builds on the single-file core; structure-preserving with resume (🟡;
`folder.rs`). All control/metadata ride in `Control` frames:

```text
Manifest(root, [ (rel_path, size) … ])      S→R
ResumeState([ bytes_already_on_disk … ])    R→S
for each not-yet-complete file:
  FileHeader(index, rel_path, size, offset) S→R
  Chunk … Chunk                             S→R   (from offset)
  FileEnd(index)                            S→R
Complete                                    S→R
```

- Each file keeps its path **relative** to the folder root; the receiver
  recreates the tree under `dest_dir/<root>/…`.
- Relative paths are **sanitized** (no `..`, no absolute) to prevent traversal.
- **Resume** — receiver reports per-file on-disk bytes; the sender skips
  complete files and streams the remainder of partial ones from `offset`.

## 5. Recovery (across links)

`send_file_recover` / `receive_file_recover` retry the transfer across fresh
links (`LinkFactory`) up to `max_attempts` with linear backoff. Because the
per-transfer handshake re-negotiates a resume offset from the receiver's
on-disk bytes, each retry continues where the last stopped. A checkpoint is
persisted via the `ReliabilityStore` so a transfer survives a **process
restart**, and is cleared on success. Terminal outcomes are never retried:
`Cancel` returns immediately, `Integrity` surfaces as an error (🟡; `recover.rs`).

## Control message reference

```rust
enum Control {
    ResumeAck { offset: u64 },     // R→S  bytes already on disk
    Complete  { checksum: String },// S→R  SHA-256 of the whole file
    Verify    { ok: bool },        // R→S  did the file match?
    Cancel,                        // either side aborts
}
```

## Compatibility

The framing has no explicit version byte yet; compatibility is governed by the
release version (pre-1.0, breaking changes allowed — see
[Supported Versions](../SUPPORTED_VERSIONS.md)). The FFI ABI is versioned
independently (`pb_abi_version`). A wire-format version negotiation is tracked
for 1.0 in [Known Issues](KNOWN_ISSUES.md).

## See also

- [Transfer](TRANSFER.md) — engine behaviour, chunking, resume, retry.
- [Security](SECURITY.md) — crypto choices and the trust model.
- [Networking](NETWORKING.md) — how a link is chosen and established.
- A runnable end-to-end example: `rust/bins/peerbeam-cli/examples/quic_transfer.rs`.
