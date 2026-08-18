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

## Pinned is not approved

The trust store holds two states, and conflating them is the mistake this
section exists to prevent.

| | Set by | Answers | Predicate |
|---|---|---|---|
| **Pinned** | the handshake, automatically, on first contact | "is this the same key I saw last time?" | `TrustStore::is_trusted` |
| **Approved** | a person, explicitly | "did the user choose this device?" | `TrustStore::is_approved` |

A pin is a **memory, not a decision**. `auth.rs` records *every* never-seen peer
as it completes the handshake, with `approved: false`, because that recorded
fingerprint is the only thing that makes a later key change detectable. It
follows that every stranger who has ever reached this machine is pinned, and
nobody chose any of them — so `is_trusted`, which is "is there a record", is
**not a permission**. It is the MITM question, and only that.

Approval is set by an act of a person: accept-and-trust in the app, or
`peerbeam trust approve <device>` at a shell. It is what the three features that
send something outward on the user's behalf, without asking again, gate on:

- **presence** — battery level, free disk and network kind, on every heartbeat;
- **clipboard sync** — whatever the user last copied;
- **`pipe --listen`** — raw bytes onto a terminal's stdout.

Each of those is defensible only as "my own devices", and a key pinned by the
handshake is not that. All three ask `is_approved`, which fails **closed**: a
store that cannot answer is not permission. Each has a unit test in which
`is_trusted` is true and the gate is still shut, so a change back to the weaker
predicate cannot pass silently, and `peerbeam-cli/tests/pipe_e2e.rs` proves the
same thing across two real processes.

Revoking (`peerbeam trust revoke`, or Trusted Devices in the app) removes the
whole record rather than just the flag, so the next connection is a fresh first
contact: re-pinned, and unapproved until someone says otherwise. Both the CLI
and the app mean the same thing by the word.

Approval had been reachable only from the app's accept-and-trust prompt, which
left a headless or CLI-only machine — a first-class target for this project —
unable to use any of the three features at all. `peerbeam trust` closes that;
see [CLI](CLI.md).

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

**While the opt-in is on, everything copied is sent to approved devices —
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
2. **Approved devices only, and not configurable.** A clip goes to a device the
   user explicitly approved or nowhere — *not* merely to one the handshake
   pinned, which would be every stranger that ever connected (see [Pinned is
   not approved](#pinned-is-not-approved)). Revoking stops the next clip, not
   the next reconnect, because the gate re-reads the trust store per push and
   asks about the **authenticated** peer rather than the id discovery
   advertised.
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

## The pipe's consent model, and why it is not the transfer prompt

`peerbeam pipe` (ChannelType `0x0107`) writes a peer's bytes straight to a
shell's stdout. That destination is what makes its consent model different from
every other inbound thing in PeerBeam, and the difference is deliberate. It is
written down here because an unexplained inconsistency in a consent model reads
as an oversight and invites someone to "fix" it — and the obvious fix, letting a
daemon accept pipes, is the one thing that must never happen.

**A file transfer prompts. A pipe does not, and must not.**

An inbound file is approved *per transfer*, by name and size, in a prompt raised
by a receiver that was already running. That works because a file lands in a
directory: it can be described before it is accepted, and refusing it costs
nothing. None of that is true of a pipe. There is no name and no size to show —
a pipe is an unbounded stream by definition — so a prompt could only ask "accept
some bytes from this device?", which is a question with no information in it.
And the prompt would have to be answered *on the receiving side*, in a session
whose stdin is either absent (a headless server, an SSH command, a `cron` line)
or, on the sending side, the payload itself. A pipe exists to be used from a
script; a prompt would make it unusable exactly where it is worth having.

So consent is expressed the other way round: **the user starts the receiver
themselves, for this one purpose.** Running `peerbeam pipe --listen` *is* the
approval. It is a stronger act than answering a prompt, not a weaker one — a
prompt is answered by whoever happens to be at the machine when it appears,
while starting a listener is a deliberate command typed by someone who knows
what they are about to receive and where they are pointing it.

Two gates enforce that, both decided in one place
(`peerbeam_transfer::may_accept_pipe`) and reached through one funnel
(`accept_pipe`), which is the only code that can put a peer's bytes into a
process's `out`:

1. **Only a `pipe --listen` accepts a pipe.** There is no background acceptance
   and no setting that grants one. A running `receive`, `daemon start` or `chat
   watch`, and the Flutter app, all advertise the Pipe capability and all refuse
   every pipe offered to them. This is the gate that matters most: a long-lived
   daemon that accepted pipes would be a remote write to whatever terminal it
   was started from, and the user who started it consented to receiving *files*.
2. **Approved devices only**, not configurable — the same rule as clipboard sync
   and presence — and narrowable to one device with `--from`, which matches the
   **authenticated** device id from the handshake and never the human name a
   peer presents (a peer chooses its own name, so a name-based restriction would
   be a suggestion).

**One stream, then exit.** A listener takes a single stream and stops. There is
no `--keep-open`: a listener that stayed up accepting stream after stream is a
much larger surface than the one the user consented to. A *refused* attempt does
not count as that stream, so a stranger cannot end someone's listener by
dialling it.

Every leg is mutation-proved over a real two-PeerSession round trip in
`peerbeam-transfer/tests/pipe_gates.rs`: delete the `listening` leg and a
daemon-shaped session writes the peer's bytes into its sink; delete the trust
leg and an unapproved peer's arrive. The tests fail because the bytes really do
land, not because a predicate returned the wrong bool.
`peerbeam-cli/tests/pipe_e2e.rs` repeats the trust leg across two real
processes: a first-contact sender is pinned by a genuine handshake and still
refused, and only a real `peerbeam trust approve` at the listener lets the
second attempt through.

**What the trust gate does and does not buy.** It asks `is_approved`, not
`is_trusted`, and the difference is exactly what makes it load-bearing against a
stranger. PeerBeam's handshake is trust-on-first-use, so a peer connecting for
the first time is *pinned as it connects* — had this leg asked "is there a
record", it would already have been satisfied by the connection it was supposed
to judge, and would only ever have refused a device the user had explicitly
revoked. Asking for approval means a first-contact peer is refused too, and
someone has to have said yes. See [Pinned is not approved](#pinned-is-not-approved).

Gate 1 still carries most of the load in practice, because a stranger must find
a listener running at all, in the seconds it is up, for one stream. `--from`
narrows it further to a single device and is the right tool when the listener
will be up for a while or the network is not trusted;
`device.require_pairing_confirmation` remains the general answer to
first-contact verification.

**A pipe cannot un-write stdout.** Bytes are written and flushed as they arrive
— that is what makes a 40 GB stream possible — so by the time the stream turns
out to be truncated or corrupt, they are already in the user's file. The stream
therefore ends only on an explicit `Complete{checksum}`, which is verified
against what was actually written: a dropped connection is an **error**, never a
clean end, and a mismatch is an error too. Both exit non-zero. That exit code is
the only signal a script gets, and `peerbeam pipe --listen > f` without checking
it will happily trust a bad `f`.

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
  (pin/lookup/trust, approve, persistence, overwrite); `finalize` (rename, no-clobber,
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
sessions, so it can be re-verified later. Revoking later is available in the app
(Trusted Devices) and at a shell (`peerbeam trust revoke`); `peerbeam trust
list` prints each pinned fingerprint, which is what an out-of-band comparison
needs on a headless box.

### Where the check is enforced

**In the engine, not in a frontend.** `Manager::accept`/`accept_trust` consult
`pairing_gate` — the deliberate twin of the CLI's, with the same three inputs
and the same three outcomes — and refuse an unconfirmed first-contact accept
whoever is asking. The GUI's dialog and the CLI's stdin prompt are two ways of
obtaining the same answer, not two implementations of the same rule (I7).

The answer crosses as `confirmed` on the accept payload
(`pb_transfer_accept {id, confirmed}`). **Only a literal `true` counts.**
Absent, `null`, `false`, `"true"` and `1` all confirm nothing, so a caller that
has never heard of this prompt — a script, an older frontend — cannot satisfy
it by accident. That is the same safe default the CLI's gate applies to a
missing answer, and it is why a non-interactive context refuses rather than
proceeds.

A blocked accept leaves the transfer **pending**, not declined. The user can go
and read the other device's screen and answer properly; being asked to verify
must not cost them the file.

### What a confirmed code proves, and what it does not

It proves that **the two devices that ran this handshake derived the same
number from the keys they actually negotiated**. Under a man-in-the-middle,
each side is negotiating with the attacker, so the two numbers differ and the
comparison fails. That is the whole of it.

It does **not** prove:

- **That anyone compared anything.** The app displays the code; it cannot see
  the other screen and never claims to have checked. `confirmed` records what
  the user said, not what they did. A user who taps through without looking has
  a pinned stranger and an app that never said otherwise.
- **That the comparison used a channel the attacker is not on.** An adversary
  positioned to relay a handshake can usually also relay a screenshot, a chat
  message, or a call. This is why the UI copy says to look at the *device
  itself*, and why a code read back over a channel the attacker controls proves
  nothing.
- **Anything about later sessions.** The code is derived from long-term keys,
  so it is stable — which is what makes re-verification possible — but a
  confirmation is a statement about the pin, not a per-session attestation.

### A refused first contact un-pins; other endings do not

Declining a transfer from a peer **this session pinned** removes the pin. Which
transfers those are is recorded once, from the handshake's `newly_trusted`, and
read from that one record by both the accept gate and the un-pin. It is
deliberately not re-derived from the trust store at decision time: by then the
handshake has pinned the peer, and a lookup can no longer distinguish a
stranger met seconds ago from a device approved last week — which is exactly
what stops a routine "no thanks" from revoking a long-standing trust.

**A failed un-pin is reported as a failure.** Reporting success on a removal
that did not happen would leave the peer trusted on disk while the app said
otherwise, and the *next* connection from it would then not be first contact at
all: no `newly_trusted`, no code, no gate, silently — the user never asked
again about the device they had just refused as a suspected MITM. The refusal
itself stands regardless; the error tells the user the pin is still there and
to remove it in Trusted Devices.

**Known scope: an unanswered prompt un-pins nothing.** A prompt that times out
(`ACCEPT_TIMEOUT`, 180 s) or whose sender drops is not the user refusing, and
this codebase does not convert absence into a decision — see `AcceptOutcome`,
where the same distinction stops a timeout being reported to the peer as a
decline. The consequence is real and worth stating: a peer that connects while
nobody is at the machine stays pinned, so the *next* connection is not first
contact and the code is not shown again. The pin alone grants nothing (auto-
accept requires `approved`, which only an explicit accept-and-trust sets, I6),
but the verification opportunity is not offered a second time. Pinned devices
are listed with their fingerprints in Trusted Devices and `peerbeam trust
list`, which is the way back.

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
