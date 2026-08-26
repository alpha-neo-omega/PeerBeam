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
| Per-channel keys | `peerbeam-transfer::session` (`ChannelCrypto`) | HKDF-Expand of the session master |
| Transport | `peerbeam-transfer-quic` | QUIC (quinn), TLS with no PKI |

## What carries this on the wire

The transport that ships is QUIC (`peerbeam-transfer-quic`, built on quinn), and
both frontends run over it: the Flutter app through `peerbeam-ffi`'s runtime, and
every CLI command that reaches another device (`send`, `receive`, `daemon`,
`chat`, `browse`, `pipe`, `pair`, `presence`) by building a `QuicTransport` of
its own. One QUIC connection carries one authenticated `PeerSession`, and each of
that session's channels is one QUIC bidirectional stream — there is no
PeerBeam-level multiplexing protocol, because the transport already multiplexes.

**QUIC's TLS authenticates nobody, deliberately.** QUIC mandates TLS and this
project has no PKI to satisfy it with, so each node presents a freshly generated
self-signed certificate and the client accepts whatever certificate it is shown.
A bare QUIC connection is therefore an encrypted pipe to an unidentified party,
and everything else on this page is what makes it a pipe to a known one: the
handshake in the next section runs over the first stream on the connection, the
peer's fingerprint is pinned on first contact, and every stream after that is
sealed under a key of its own. Reading the TLS handshake as identity — "the
certificate was accepted, so this is the right device" — is the one misreading
that would make the rest of this document decorative.

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
send something outward on the user's behalf, without asking again, gate on — and
what a device's [permissions](#what-an-approved-device-may-do) then narrow:

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
whole record — permissions included — rather than just the flag, so the next
connection is a fresh first contact: re-pinned, and unapproved until someone says
otherwise. Both the CLI and the app mean the same thing by the word. To keep a
device but take one power away, withhold that permission instead
(`peerbeam trust revoke-permission`).

Approval had been reachable only from the app's accept-and-trust prompt, which
left a headless or CLI-only machine — a first-class target for this project —
unable to use any of the three features at all. `peerbeam trust` closes that;
see [CLI](CLI.md).

## What an approved device may do

Approval answers *whether* the user chose a device. Permissions answer **what**
that choice left it. `VISION.md` permits remote capabilities only as "explicit,
**permissioned**, narrowly-scoped actions", and I6 requires "explicit,
revocable, **per-capability** consent"; one `approved` bit could express neither.
Before this, approving a laptop so it could receive files also granted it this
machine's clipboard, its status heartbeat, and an accepted pipe, with no way to
say otherwise.

A `TrustRecord` therefore carries a **permission set** alongside `approved`,
covering the features that exist today:

| Permission | Governs | Enforced in |
|---|---|---|
| `files` | inbound file transfers | `peerbeam_ffi::transfer::admit_transfer` |
| `chat` | outbound chat messages and in-chat file shares | `peerbeam_chat::gate::may_exchange_chat` |
| `clipboard` | this machine's clipboard leaving for that peer | `peerbeam_clipboard::gate::may_share_clip` |
| `presence` | this machine's status heartbeat leaving for that peer | `peerbeam_presence::gate::may_share_status` |
| `pipe` | an inbound `pipe` reaching a listening terminal | `peerbeam_transfer::pipe::gate::may_accept_pipe` |

There is deliberately **no permission for a feature that does not exist**. One
could not be tested, and would be the wrong shape by the time its feature was
built.

### One predicate

`TrustStore::may(device, Permission)` is the only thing in the workspace that
reads the set. Every gate calls it as one more named leg in the pure function it
already was, so there is one place to read, one place to test, and no way for
two features to disagree about what a grant means.

`may` **implies approval**: an unapproved device may nothing, whatever its bits
say. That rule lives in `TrustRecord::effective_permissions`, so the predicate
and every listing agree by construction — a stranger the handshake pinned is
never even *shown* holding a permission it cannot use. It fails **closed** on a
store error, exactly as `is_approved` does. This is the same lesson as
[Pinned is not approved](#pinned-is-not-approved) one level down: a predicate
that skipped approval would answer `true` for a peer nobody chose.

The gates keep their own `is_approved` leg alongside `may` rather than
collapsing into it, for the same reason `is_approved` and `is_trusted` are two
legs and not one: a gate must not lean on another predicate's internal
implication, which a later "simplification" could quietly remove.

### Revoking applies to the next operation

Every gate re-reads the trust store **per operation** — per message, per clip,
per heartbeat, per accept — so withholding a permission stops the next one
rather than the next reconnect. Revoking `presence` additionally drops what the
live presence registry already holds, so a dashboard stops displaying a status
the peer may no longer be sent.

Two features needed more than a narrowing, because neither required approval
before permissions existed. The rule for both, stated once:

> A device the user took a decision about is governed by that decision. A device
> they did not is governed by the feature's pre-existing policy.

- **Transfers.** An approved device whose `files` permission was revoked is
  refused **outright**, without a prompt: the user already answered, and
  re-asking on the sender's schedule is how a permission becomes a nuisance. A
  revoked permission also beats a resume — the decision is newer than the
  checkpoint. A merely *pinned* peer is prompted exactly as before, so first
  contact is unchanged.
- **Chat.** Chat has never required approval; a peer that completes the
  handshake can exchange messages. Gating it on `may` alone would not narrow
  anything — it would revoke chat from every device nobody explicitly approved,
  which is the silent breakage this model exists to avoid. So an approved device
  is governed by its `chat` permission and an unapproved one behaves as it always
  has. I6 lists the sensitive actions needing per-capability consent — auto-
  accept, remote commands, live clipboard, remote browse — and a text message to
  a peer already talking to you is not one of them.

Chat is gated on **sending** only, as presence and clipboard are: a message that
has already arrived is in hand, and refusing to persist it would lose the user's
data to enforce a policy about what this machine says.

### The upgrade rule

A `trust.json` written before this field has no `permissions` key, and how that
absence is read is the part a user would experience as the app breaking.

- Reading it as **no permissions** would silently revoke every working device the
  moment they upgraded: chat stops, transfers stop, and nothing says why.
- Reading it as **all permissions** is the mirror danger: a permission added in
  some later release would be auto-granted to devices nobody ever reviewed.

Neither. **A record written before this field means the permissions that existed
when it was written** — exactly the five above — **and any permission introduced
later is denied by default, for legacy and new records alike.**

The mechanism is what makes that true rather than merely intended. Each
permission owns a **slot**: a small integer assigned once and never reused, even
if a permission is retired. `PermissionSet::granted_on_approval()` is a **frozen**
constant enumerating the five slots that existed at introduction, and it is both
the `serde` default for a missing field and what `FsTrust::approve` writes. A
slot allocated later is clear in it *by construction*, so:

- a pre-upgrade record keeps everything it had, and gains nothing;
- a newly approved device gets the five, and gains nothing added afterwards;
- a permission added in a later release is **opt-in, always** — granted only by
  an explicit `peerbeam trust permit` or the app's switch, which is precisely the
  "explicit, permissioned, narrowly-scoped" consent `VISION.md` requires.

`PermissionSet::grants_slot` exists so that "a permission added later is denied"
can be *asserted* against a slot no `Permission` variant occupies yet, without
inventing a fake sixth permission to prove it with — and without a version stamp
on the record. `peerbeam-trust-fs` pins it with a test that loads a **literal**
pre-upgrade `trust.json` (not a constructed record, which this build's own
serializer would always write the field into) and asserts all five are permitted
while every later slot is not; `peerbeam-cli/tests/trust_cli.rs` repeats it
through the real binary.

Note this is the **opposite** default to `approved`, deliberately. `approved` was
a new power no prior record could have consented to, so it defaults off.
Permissions are a *narrowing* of a power a legacy record already held in full, so
defaulting them off would take away what the user already granted.

Two smaller consequences, recorded because they are choices:

- Permissions are stored as an **array of names**, so a `trust.json` says what it
  grants to whoever opens it and a renumbering can never silently re-point a
  grant. A name this build does not know — a store written by a newer release —
  is ignored rather than rejected: refusing to parse would take every pin on the
  machine with it, and an unknown grant is not honoured by a build that cannot
  enforce it. It is therefore also **not preserved** if an older build rewrites
  the file, so downgrading and re-upgrading requires re-granting anything new.
- Permissions apply only to an approved device, so `peerbeam trust permit` on a
  pinned-but-unapproved one is refused with the next step (`trust approve`)
  rather than writing a bit that would grant nothing.

### Trust that runs out

An approval can carry a deadline: `peerbeam trust approve <device> --for 30m`
stores an absolute instant in the record's `expires_at`. At and after it, the
record is worth exactly what it was before anyone approved it — a pin.
`is_trusted`, `is_approved` and `may` all answer `false`, and every permission
reads as withheld.

**Enforced where it is read, never by a sweeper.** The three predicates above
consult the deadline themselves, and every gate in the workspace already asks
them per operation, so the window shuts on time whether or not anything has run
since. A background cleaner would be a second source of truth and the slower
one: the interval between sweeps is exactly the interval in which a device is
still trusted after its window closed. A store reopened tomorrow reaches the same
verdict, and a machine asleep through the window wakes with it shut.

**The pin outlives the grant.** Expiry ends what the *user* granted; it does not
forget the key. The record stays on disk and `TrustStore::lookup` keeps returning
it, which is what `auth.rs` compares a presented fingerprint against. Deleting it
on expiry would turn a 30-minute window into a TOFU reset — the device's next
handshake would pin whatever key answered, and a key change would no longer be
detectable. Forgetting a device is what `revoke` is for; `auth.rs` therefore
reads `lookup`, deliberately, and never `is_trusted`.

Renewing an expired device restores the permissions it was actually left rather
than the five approval starts with, so a lapsed window cannot silently undo a
revoke. With `encryption.require_pin_pairing` on, renewing an expired approval
needs `peerbeam pair` exactly as a first approval does — the grant has ended, so
granting it again is a new decision.

A `trust.json` written before this existed has no `expires_at` and is trusted
**indefinitely**, which is what those records meant when they were written. That
is deliberately the opposite of `approved`'s upgrade rule: reading a missing
deadline as "expired" would revoke every device on the machine on upgrade.

### Disappearing messages delete *this* device's copy

A conversation can be given a window (`peerbeam chat retention <peer> --after
30m`), after which its messages stop being readable here and are deleted.

**Be precise about what that guarantees.** The window is local. PeerBeam sends no
frame asking the peer to delete anything, and there is no honest way for it to:
the peer's copy is on the peer's disk, under the peer's control, and a "delete on
both sides" claim would be a promise this architecture cannot keep. What it does
guarantee is narrower and true — a message is readable on this device for at most
the window, and is then removed from this device.

Two further limits, stated rather than discovered:

- **Received files are not deleted.** Only the conversation row goes. A file the
  user accepted is theirs, in their save directory, and destroying it because a
  chat window closed would be data loss dressed as privacy.
- **A row this build cannot decode is not pruned.** Its age cannot be read, and
  it is already invisible; leaking disk beats destroying data whose contents are
  unknown.

Enforcement is at **read**, not by a sweeper. A window shuts on the next read
whether or not anything has run — the same reasoning as *Trust that runs out*
above, and for the same reason: the interval between sweeps is exactly the
interval an attacker would want.

Off by default, and off for every conversation that already exists. An upgrade
never starts deleting history.

### "My devices" is a label, not a grant

A record also carries `mine`: the user's own note that this is one of their
machines, so a surface can offer *"send to my laptop"* and filter a device list
down to the handful that matter.

It is **local and inert**. Marking a device sends it nothing, tells it nothing,
and asks it nothing — the peer neither reads nor sets the flag, and the same
device may be marked on one machine and not on the next. It grants nothing
either: a device marked mine that nobody approved is still approved for nothing,
one whose permissions withhold `browse` still may not browse, and one whose
window has closed is still expired. `TrustRecord::effective_permissions_at` —
which is what every gate's answer is computed from — reads `approved`,
`permissions` and `expires_at`, and does not read `mine`. Nothing that sends,
accepts or opens anything may branch on it; if a gate ever did, "mine" would
become a way to grant a device powers nobody approved it for, settable by
anything that can write one bool into the local trust file.

For the same reason the label is **not gated by the window**: a laptop is still
the user's laptop at 10:31, so it stays in "My devices" while answering `false`
to every question about what may leave this machine. `my_devices()` likewise
lists marked records approved or not — it answers *which of these are mine*, not
*which may I use*; a caller must still ask `may`.

A `trust.json` written before this existed has no `mine` and loads with `false`.
Unlike `approved` that is not the fail-closed direction — there is no permission
here to fail closed about — it is the only truthful one: nobody who wrote those
records said any device was theirs, and defaulting to `true` would sweep every
stranger the TOFU handshake ever pinned into the one list a user taps "send" on
without reading it.

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

**Every channel has its own keys.** The handshake runs once per session, and its
directional keys are used as a master secret rather than as frame keys: each
channel — the control channel included — derives its own send and receive key and
nonce prefix from that master with HKDF-Expand over the channel id, the protocol
version, the direction and a reconnect epoch, so no two channels, and neither
direction of one channel, share a key or a nonce space. A channel's stream is
wrapped in a `SealedLink`: `SecureLink`'s sealing scheme and frame codec over a
stream it owns rather than borrows, so the transfer code runs over it unchanged.
The epoch is what makes resume safe — a resumed session re-derives every key at a
bumped epoch, so a channel whose counter restarts at zero is never reusing a
nonce under a key that has already seen it.

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
absolute paths), then reduced to what the receiving OS can actually hold. On
Windows that last step matters more than it sounds: `File::create("nul.txt")`
opens the NUL *device*, so every write succeeds, the checksum verifies, the
transfer reports success — and no file exists. The reserved device stems
(`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`), the characters
`< > : " | ? *`, and a trailing run of dots or spaces are therefore each
**renamed** — `nul.txt` becomes `nul_.txt` — never refused: the peer that sent
`aux.h` is usually a Linux box with a perfectly legal filename, and refusing
costs the user the file. Traversal is the opposite case, with nothing worth
preserving, and is still rejected. Unix receivers pass names through untouched
rather than inventing rules their filesystem does not have.

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

## The one request that is not to a peer

Everything else PeerBeam sends goes to a peer, or onto the local network looking
for one — the Tailscale discovery source asks `tailscaled` on this machine, over
its Unix socket or the `tailscale` binary, and does not leave it. There is one
exception, and it is written down here because a privacy claim with an
unmentioned exception in it is worth nothing.

`peerbeam check-updates`, and the **Check for updates** button in the app's About
section, make one HTTPS GET to the release feed
(`api.github.com/repos/alpha-neo-omega/PeerBeam/releases`) and report what came
back.

**Only when a person asks.** There is no timer, no check at launch, and no check
as a side effect of anything else: `peerbeam_update::check` has exactly two
callers, the CLI command and the FFI entry point behind that button, and pressing
the button is the opt-in each time. Using PeerBeam therefore never tells a server
that PeerBeam is being used.

**What the request unavoidably discloses.** GitHub sees the connecting IP
address, and so an approximate location, and the time of the request — which is a
moment somebody was at this machine running this app. Any HTTPS request discloses
that much; it is the honest cost of the feature.

**What it does not carry.** No device id, no install id, nothing derived from the
identity keypair, no cookie or persistent client state, and no custom headers.
The one header naming this product is the `User-Agent`, which GitHub's API will
not serve a request without: it is the bare word `PeerBeam`, with no version in
it, and there is no query string, so the request does not say which build is
asking either: the feed is asked what the newest release is, and
the comparison against the running version happens here. The answer is inert
as well. A `Release` is a version string and a URL; nothing downloads, installs
or changes behaviour on the strength of what the server said, and there is no
retry — a caller that wants to ask again asks again.

Failing is not a problem anybody has to solve. An unreachable feed reports
`reachable: false` and exits 0, because a machine with no route out is a normal
machine for this app, and nothing here may become a precondition for using it.

**This is an amendment, not an exception somebody took quietly.** Invariant I4
forbids phone-home without qualification, so a release check conflicts with it as
written, and the narrow reading — that the clause is aimed at unattended, ongoing
disclosure rather than at a single request a person makes deliberately — was not
something to adopt silently. It is recorded as **A1**, the first amendment to
[the invariants](ARCHITECTURAL_INVARIANTS.md#amendments): dated, approved, and
stated as six binding conditions that hold together, so a build that drops any
one of them is outside A1 and back in conflict with I4. The matching non-goal in
[VISION.md](VISION.md) is narrowed in the same change, because leaving a
published claim that the shipped build makes false would be worse than the check
itself.

## Threat notes / scope

- The handshake authenticates *keys*; binding a key to a human-meaningful
  identity relies on TOFU + optional out-of-band fingerprint check.
- Discovery is untrusted input by design; authentication happens here, at
  transfer time, not in discovery.
- Folder receive does not yet use the `.part`/finalize path (single-file
  does); adopting it there is a follow-up.
- QUIC's own TLS authenticates nobody: certificates are self-signed and accepted
  unseen, so a connection with no `PeerSession` on it is an encrypted pipe to an
  unidentified party. See
  [What carries this on the wire](#what-carries-this-on-the-wire).

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

**An unanswered prompt gives the pin back — while the check is on.** A prompt
that times out (`ACCEPT_TIMEOUT`, 180 s) or whose sender drops is *not* the user
refusing, and this codebase does not convert absence into a decision: the
transfer stays `Unanswered`, and nothing is reported to the peer as a decline
(see `AcceptOutcome`, where the same distinction is enforced).

But the pin is a different question from the decision. Left in place, it would
mean the *next* connection is not first contact — no code shown, no gate — so a
stranger connecting while the machine is unattended would consume the single
verification opportunity by doing nothing at all. That is the same trap the
failed-un-pin case above describes, reached by absence instead of by a swallowed
error. So while `require_pairing_confirmation` is on, an unanswered first
contact is un-pinned and the device stays genuinely new until somebody looks at
it.

With the check **off** the pin stays, and that is not an inconsistency: nothing
was going to be verified, so there is no opportunity to give back, and
un-pinning would only churn ordinary TOFU state. Either way the pin alone grants
nothing — auto-accept requires `approved`, which only an explicit
accept-and-trust sets (I6). Pinned devices are listed with their fingerprints in
Trusted Devices and `peerbeam trust list`.

### What each permission actually stops

A permission is checked where the code can enforce it, and the two file-shaped
ones are not symmetric. Worth stating plainly, because a switch whose scope is
guessed at is a switch people trust wrongly:

- **Files** — enforced in **both** directions. Inbound, `admit_transfer` refuses
  an approved device whose `files` was turned off. Outbound, `permit_send_files`
  refuses the same device before a path is validated. Neither refuses a
  *merely pinned* peer: sending to a device you have just discovered has never
  required approval, and gating it on `may` (which implies approval) would break
  the app's primary flow.
- **Messages** — enforced on **sending only**. Revoking it means this device
  will not message that one; it does not stop that device messaging here. Chat
  has never required approval to receive, and the inbound path would need the
  chat handler to carry the trust store. The switch is worded "Send messages to
  this device" so it does not promise the half that is not there.
- **Clipboard**, **Device status** — send-side by nature: they gate what leaves.
- **Pipes** — receive-side by nature: it gates what a listening terminal accepts.

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
