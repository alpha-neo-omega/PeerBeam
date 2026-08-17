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

## Device identity

Each device has a long-term X25519 identity keypair, generated on first run and
stored at `<data_directory>/identity.json` with owner-only permissions (`0600`
on Unix). The `device_id` is derived from the key's fingerprint (`pb-…`). Peers
pin it via TOFU, so **deleting this file resets the device's identity** (peers
will see a new, untrusted device and must trust it again). It never leaves the
device.

## Application data (AppStore)

Capability data (chat log, clipboard history, notes) is stored under
`<data_directory>/appstore/<namespace>/`, one file per record. Each record's
**value** is encrypted at rest with AES-256-GCM under a key derived from the
device identity's secret (`peerbeam-appstore-v1`); files are `0600` on Unix.
Namespace and record-key names are stored in the clear (the directory is
`0600`-protected). Because the key derives from the device identity, **deleting
`identity.json` makes existing AppStore data unreadable**. Clearing a namespace
deletes its records.

## Staged outbox blobs

Sharing a file in chat with a peer that is currently offline **queues** it, and
queueing copies the file's bytes into a store the outbox owns:
`<data_directory>/outbox-blobs/<id>`, one blob per queued file. The copy is what
makes the queue honest — between queueing and delivery the user may delete, move
or rewrite what they picked — but it means a second copy of that content sits in
application storage for as long as the entry stays queued, which under
keep-forever retry can be indefinitely. Each blob is bounded by
`device.max_queued_file_bytes`; nothing here writes to, moves or deletes the
user's own file, which is opened for reading and nothing else.

**These blobs are plaintext.** Unlike an AppStore record, whose value is
encrypted at rest, a staged blob is stored exactly as it was read. It is created
owner-only — `0600` on Unix, applied *before* the copy starts, so even a partial
blob left by a `SIGKILL` mid-copy is protected — and on Windows, where there is
no comparable one-liner, it inherits the directory's default ACL rather than
being restricted explicitly.

Plaintext here is a deliberate trade, not an oversight. A staged blob is a
transient copy of a file the user already holds in plaintext on the same disk,
under the same account, so encrypting the copy withholds nothing an attacker with
that access does not already have from the original. It would cost something
real: attachments can be many gigabytes, and sealing one would force either a
whole-file buffer — which the streaming invariant forbids (no file is ever fully
loaded into memory) — or a streaming-crypto layer no other path in the project
needs.

A blob is deleted automatically as soon as its entry reaches a terminal outcome
(delivered, declined, or given up on), and `peerbeam chat cancel <peer> <id>`
drops a still-queued entry and frees its bytes on demand. Blobs no queue entry
owns — staged by a run that died between the copy and the enqueue — are swept
when the app next starts.

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

## Clipboard sync sends passwords, and nothing detects them

This is the one honest limit of clipboard sync (ChannelType `0x0102`), and it is
stated here, on the Settings toggle, and in `peerbeam_clipboard`'s crate docs
because a user cannot make a sound decision without it.

**While the opt-in is on, everything copied is sent to trusted devices —
passwords included.** PeerBeam does not attempt to detect secrets, and this is a
decision rather than an omission. A clipboard read returns plain text and
nothing else: Flutter's `Clipboard.getData` carries no sensitivity flag, X11 and
Wayland define no standard one, and password managers on these platforms have no
portable way to mark a paste buffer as confidential. There is simply no signal
to branch on.

A heuristic would therefore be a guess, and it would be wrong in both directions:

- **Guessing "secret" on ordinary text** silently drops clips the user expected
  to arrive, teaching them the feature is unreliable.
- **Guessing "safe" on a real credential** ships it *while the UI implies
  something was checked*. That is strictly worse than never claiming to check,
  because the user relaxes on the strength of a promise nothing is keeping.

So the mitigations are the ones that are actually enforceable, and they are the
same three the presence feature uses:

1. **Opt-in, default off** (I11). Nothing is synced until the user says so, and
   a settings document written before the feature existed loads as off — an
   upgrade never silently opts anyone in.
2. **Trusted-only, and not configurable.** A clip goes to devices in the trust
   store or nowhere. Revoking trust stops the next clip, not the next reconnect,
   because the gate re-reads the trust store per push and asks about the
   **authenticated** peer rather than the id discovery advertised.
3. **One tap to stop.** Turning the setting off stops the next clip and the
   watcher immediately, with no restart.

All three are decided in one place, `peerbeam_clipboard::gate::may_share_clip`,
alongside the peer's negotiated `CLIPBOARD_FEAT_CLIP`. Each leg is
mutation-proved over a real two-PeerSession round trip in
`peerbeam-clipboard/tests/gates.rs` — deleting any one of them makes a clipboard
arrive on the other side and fails the suite.

Nothing is persisted (I4): there is no clipboard history anywhere in PeerBeam,
because a durable log of everything a user ever copied is exactly the artefact
this feature must not create. The CLI's arrival notice prints the sender and the
byte count and never the contents, for the same reason.

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

## Pairing code (optional first-contact verification)

On first contact PeerBeam pins a peer's public-key fingerprint (trust on first
use). To let a user confirm that pin is the intended peer and not a
man-in-the-middle, each device can display a **pairing code**: a 128-bit
"safety number" derived from both devices' public keys
(`SHA-256("peerbeam-pairing-v1" ‖ lo ‖ hi)`, first 16 bytes, shown as eight
groups of four uppercase hex digits). Both honest peers compute the **same**
code; under a man-in-the-middle each side computes a **different** code.

The 128-bit width resists an offline grind (a short 6-digit code would not —
an attacker could grind substituted keys until the two sides' codes collide).

It is **optional and off by default** (`device.require_pairing_confirmation`).
When enabled, the receiver must confirm the codes match before accepting a
transfer from a newly pinned peer; a mismatch (or a decline) **un-pins** the
peer (treated as a suspected MITM) and aborts. The code is stable across
sessions, so it can be re-verified later. Revoking trust later is available in
the app (Trusted Devices).

## Bulk approval is accept-once, never trust

The Transfers screen offers **Accept all** / **Decline all** when two or more
inbound transfers are waiting, plus **Select**, which switches the banner to a
checkbox per card and answers only the ones the user picked. Both are the same
decision: they call the engine's per-id `pb_transfer_accept` /
`pb_transfer_reject` once per transfer and **never** `acceptTrust`. Trusting a
device grants it persistent auto-accept for everything it sends from then on,
which is a materially stronger and longer-lived act than approving the batch
currently on screen. That stays a deliberate, per-device choice on the card
("Trust"): there is no "Trust all" and no "Trust selected", and while selecting,
the per-card Trust is hidden rather than left as a second path into a batch.

Adding a second route to a batch accept adds no second consent rule. Every route
goes through one loop (`TransferRepository._decideMany`), so the scope cannot
drift between them: **inbound** transfers in `pending` only. An outbound send and
an already-running, paused, completed or failed transfer cannot be reached from
either. A selection is not trusted on faith either — ids are re-checked against
what is still awaiting approval before anything is asked of the engine, so a
stale pick is counted as "no longer waiting" rather than sent as a decision, and
the selection is cleared whenever the banner goes away (inbound transfer ids come
from the sender, so a stale set could otherwise pre-check a later transfer that
reused one). Nothing about the decision is remembered: the next batch asks again
(invariant I6 — explicit, per-act consent, never inferred).

## Settings & trust over FFI

The FFI `pb_settings_get` exposes the TOFU trusted-devices list (read from the
trust store) and `auto_accept`; `pb_settings_set` persists changes (applied on
next init). Trust pinning/verification itself is unchanged — see above.
