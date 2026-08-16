# File sharing inside chat — increment 2b: offline queueing

Status: approved design, not yet implemented.
Predecessor: `2026-08-12-file-in-chat-design.md` (the 2a/2b split is defined there
and remains authoritative on scope).

2a shipped the online path: negotiation, correlation, the record schema,
approval-with-a-name, terminal states, boot reconciliation, and all four
surfaces. Attaching to an unreachable peer fails with a clear message.

2b is what makes it queue.

## What changes for the user

Attaching a file to an offline peer stops failing and starts queueing. The
bubble reads *Queued*, and the file delivers when the peer next appears — the
same keep-forever promise text has had since 1b. A file the peer declines reads
*Declined* and stops, instead of re-offering forever. A thread is reachable
whether or not discovery can currently see the peer, so there is somewhere to
queue into.

## Decisions

Four forks were settled during design. Each is recorded with the reason, because
the reason is what a later reader needs:

1. **Stage the bytes, uniformly on every platform.** Not reference-and-detect,
   not a hybrid.
2. **One queue, `OutboxEntry` extended additively**, with a one-file-in-flight
   guard rather than two lanes.
3. **An explicit `FileDecline` chat message**, plus a bounded backstop for peers
   too old to send one.
4. **A conversations list backed by a new `AppStore::namespaces` enumeration**,
   not a chat-owned index and not a saved-device re-key.

## Staging

`prepare_file_send` gains a staging step: the picked file is stream-copied into
outbox-owned storage through the existing `StorageProvider` port
(`open_write` / `open_read`), never `read_to_end`. **I10** holds by construction.

The queue then owns bytes nobody else can move, delete, or edit. This *deletes*
the source-changed problem class rather than detecting it: there is no mtime
comparison, no "the file you queued is not the file we sent", and no Android
content-URI instability, because the picker's cache copy is no longer load-bearing
once staging has run.

The cost is disk, and it must be bounded:

- **Size cap.** `DeviceConfig::max_queued_file_bytes`, default 16 GiB. Above it,
  the attach is refused at pick time with a message naming the limit. A silent
  failure later is worse than an honest refusal now.

  The cap is deliberately high, because staging is uniform: it applies to every
  chat send, not only to sends that queue. A low cap would therefore be a
  *capability regression* against 2a, which streams a file of any size straight
  from the source when the peer is online. 16 GiB is set to be a backstop
  against the absurd, not a product limit — the free-space check below is what
  actually protects the disk in practice.
- **Free-space check.** Staging that would leave less than 512 MiB free is
  refused with the same shape of message. Filling the user's disk to queue a
  file they may never send is not an acceptable trade.
- **Staging failure is immediate failure.** A write error at queue time fails the
  attach then and there. Nothing is enqueued, no row is persisted, and the user
  learns immediately.
- **Eviction.** The staged blob is deleted on every terminal outcome — sent,
  declined, cancelled, or backstop-expired. At startup a sweep deletes orphans:
  staged blobs with no matching queue entry, which is what a crash between
  staging and enqueue leaves behind.

Staged blobs are written as plaintext, consistent with the existing `.part`
precedent in the receive path. This is stated rather than assumed: **I11** forbids
"unencrypted local stores of user content", and a staging area is a closer call
than a receive directory. The justification is that a staged blob is a transient
copy of a file the user already holds in plaintext on the same disk, with the
same lifetime as the queue entry that owns it, and that encrypting multi-GB blobs
would force either a full-file buffer (violating I10) or a streaming-crypto layer
that no other path needs (violating I2's preference against parallel systems).
If that trade is judged wrong, the fix is a streaming encrypted blob store used
by both `.part` and staging — not a special case for one of them.

## The queue

`OutboxEntry` gains optional file fields, all `#[serde(default)]` so every entry
1b and 2a already persisted still decodes:

```
OutboxEntry {
    peer_id, message_id, body, timestamp,     // unchanged, 1b
    kind: Kind,                               // default Text
    file: Option<StagedFile>,                 // None for text
    offers_refused: u32,                      // default 0, see Backstop
}

StagedFile { name, size, staged_path }
```

One namespace (`chat.outbox`), one lane. The drain does not await a transfer;
it starts one and moves on. A **one-file-in-flight-per-peer guard** makes the
drain skip file entries while a transfer for that peer is running, so queueing
five videos does not start five transfers competing for the link. Text entries
are never skipped and never wait behind bytes — a file's bytes travel on the
TRANSFER stream channel, and only the small `FileRef` uses CHAT.

### The send path becomes uniform

Sending is now **stage → enqueue → drain immediately if reachable**, with no
online/offline fork. The online case is the queue draining without delay.

This is the riskiest choice in 2b and is made deliberately: two paths would mean
two sets of terminal-state handling, and 2a's terminal states were already the
source of the branch's hardest defects. It re-routes 2a's proven online path
through the queue, so the existing regression nets around it
(`peerbeam-ffi tests/chat_ffi.rs`, `peerbeam-cli tests/transfer_e2e.rs`) are
load-bearing for this increment and must pass unchanged.

### The cost of uniformity, stated plainly

Because staging is uniform, **every** chat file send copies the file before any
bytes move — including a send to a peer who is online and reachable. Two
consequences follow, and both are accepted rather than overlooked:

- **Disk.** Sending a 5 GiB file needs 5 GiB free on top of the original, and
  pays a full extra write the online path did not previously need. The
  free-space check refuses the send rather than filling the disk.
- **Time, and therefore UI.** Copying multi-GB takes long enough to be visible.
  An attach that appears to hang is a bug report. Staging must therefore report
  progress on every surface: the bubble shows a *Staging* state before *Queued*
  or *Transferring*, the CLI prints a staging line, and staging is cancellable —
  a user who picked the wrong 8 GiB file must not have to wait it out.

The alternative — staging lazily, only once an entry has to wait — was
considered and rejected in favour of a single code path with no unstaged/staged
entry state to reason about.

## Decline, retry, and terminal states

A new chat message type, additive exactly as `FileRef` was:

```
MessageType FileDecline = 3        flags OPTIONAL | END_OF_MESSAGE
FileDecline { id, timestamp }      the id is the FileRef's id
CHAT_FEAT_FILEDECLINE = 1 << 1     new bit on the CHAT capability
```

Because a decline is an ordinary chat message, a decline made while the *sender*
is offline queues in the receiver's existing text outbox and delivers later. No
new delivery machinery is needed for it.

The receiver sends `FileDecline` only when the negotiated set carries
`CHAT_FEAT_FILEDECLINE`; a 2a-era sender never receives one.

### What is terminal

| outcome | behaviour |
| --- | --- |
| `FileDecline` received | terminal — row `Declined`, dequeue, delete the staged blob |
| offer reached the peer and was refused or timed out at the approval gate | counted; terminal after 3 (see Backstop) |
| could not reach the peer at all | retryable forever, exactly like text |
| peer lacks `CHAT_FEAT_FILEREF` | retryable forever; does **not** count toward the backstop, because no offer is ever sent and nobody is prompted. They may upgrade |
| transfer failed mid-stream | retryable forever |
| user cancels a queued file | terminal — dequeue, delete the staged blob, row `Failed` with reason `cancelled` |

### Backstop

A 2a-era peer cannot signal a decline, so a refused file would re-offer forever
and re-prompt its receiver every time. The backstop counts only offers that
*reached* the peer and were refused or timed out at the approval gate —
`offers_refused` — and goes terminal at 3.

A connection failure is deliberately **not** counted. The peer never saw the
offer, nobody was nagged, and keep-forever is the promise text already makes.
Counting attempts rather than refusals would burn the budget during a flapping
link and drop a file nobody ever declined.

### Retry and the settle guard

2a's `ChatRecord::is_settleable_file_row` admits a write only when the row is
`Transferring` or `PendingApproval`, which makes every terminal write once-only.
That is exactly what a retry needs to undo, and it is a deliberate security
guard — it exists because a peer can reuse a known chat message id as its
`transfer_id`.

The guard is **not** relaxed. Re-opening a `Failed` row to `Transferring` for a
retry is a *local, sender-initiated* write on a separate path that no peer input
can reach — the same shape as `fail_chat_file`, which already writes unguarded
for exactly this reason. The guard stays as strict as it is today against
anything arriving from the wire.

Queued rows use the existing `Status::Pending`, matching text's meaning of
"queued, not yet sent", and a cancelled file lands on `Failed` with reason
`cancelled`, per 2a's precedent — no `Cancelled` variant is added.

One variant *is* added: `Status::Staging`, for the window in which the file is
being copied and no bytes have yet been offered to anyone. It earns its place
because uniform staging makes that window user-visible and multi-GB long;
folding it into `Pending` would show "Queued" for a file that is not yet safe to
queue, and folding it into `Transferring` would claim a transfer that has not
started. Being a new variant, it is exactly the case the forward-compat
containment below exists for: a 2a binary cannot decode it, and must skip that
row rather than blank the conversation.

The full outgoing lifecycle is therefore:

```
Staging -> Pending -> Transferring -> Sent
   |          |            |
   |          |            +-> Failed (retryable, or terminal at the backstop)
   |          +-> Declined (on FileDecline)
   +-> Failed/cancelled (staging cancelled or refused)
```

## Reachability and reconciliation

`AppStore` gains one method:

```
fn namespaces(&self, prefix: &str) -> Result<Vec<String>>;
```

Home gains a **Conversations** list built from the existing `chat-<device_id>`
namespaces, keyed by the peer's real authenticated device id — the same namespace
both halves of a conversation already agree on. A peer you have ever exchanged a
message with has a thread you can open, online or not.

The same method fixes a gap 2a left open: `runtime::reconcile_chat` enumerates
only peers with queued *text*, because enumeration did not exist, so a thread
whose only unsettled rows are files is never reconciled. With `namespaces` it
reconciles every conversation.

This supersedes 2a's removal of saved-device chat. That entry point was removed
because `SavedDevice.id` is a locally minted timestamp, so the thread was keyed
by an id the peer never agreed to and replies were unreachable. A conversations
list keyed by the authenticated device id has no such problem, and does not
require re-keying anything.

## Compatibility

- **Backward.** Every new field is `serde(default)`; every 1b/2a `OutboxEntry`
  and `ChatRecord` still decodes. `FileDecline` ships `OPTIONAL`, so a peer that
  does not know it ignores it per `MESSAGE_REGISTRY.md` §6. A peer advertising
  `features: 0` is still never sent a `FileRef`.
- **Forward.** 2b-written rows would otherwise be undecodable by a 2a binary the
  way 2a rows were by 1b. The 2a final review recommended landing containment
  before 2b and called this the cheapest place: `history()` skips an undecodable
  row and continues, instead of failing the whole namespace. A future increment's
  schema can then no longer blank an entire conversation on an older build.

## Testing

Beyond unit coverage, these are the behaviours that must be pinned, because each
is a defect this feature has already produced once or is newly reachable:

- A queued file survives a restart and delivers when the peer appears.
- A staged blob is deleted on every terminal outcome, and the startup sweep
  removes an orphan left by a crash between staging and enqueue.
- Queueing a file and then deleting, moving, or editing the source still
  delivers the bytes the user chose.
- A 4 GB file at the head of the queue does not delay a text message.
- Five queued files start one transfer, not five.
- A declined file goes terminal on the signal and never re-offers.
- A 2a-era peer that refuses three times goes terminal; a peer that is merely
  unreachable never does, no matter how many attempts elapse.
- A retry re-opens a `Failed` row, while a wire-driven settle still cannot.
- A conversation is reachable for a peer discovery cannot see.
- An undecodable row does not blank the conversation around it.
- Staging reports progress and can be cancelled mid-copy, leaving no orphan
  blob and no queue entry behind.
- A send is refused, with a message naming the reason, when staging would breach
  the free-space floor — and the original file is untouched.

## Non-goals

Carried forward from 2a and still out: thumbnails and previews, folder attach,
pause/resume/cancel from the bubble (the Transfers screen still offers them),
receipts, reactions, drag-and-drop onto a thread, deep-linked notifications,
grouped multi-file bubbles, and a Retry *button* — automatic retry covers the
recoverable cases, and when a file is genuinely refused the honest answer is not
to offer it again.

New to 2b: no queue reordering, no per-file priority, no resumable staging (a
staging copy interrupted by a crash is discarded and re-staged, not resumed), and
no cross-device queue sync.

## Constitutional conformance

- **I2** — reuses `StorageProvider`, the existing outbox, the existing approval
  path, and the existing transfer engine. The one new port method replaces a
  chat-owned index that would have been a second source of truth.
- **I6** — a queued file still requires the same explicit approval on arrival;
  queueing changes when bytes are offered, never whether consent is asked.
- **I9** — `FileDecline` is additive, `OPTIONAL`, and feature-gated; no silent
  wire drift.
- **I10** — staging streams in and out; no whole file is buffered.
- **I11** — the feature functions offline by definition. The plaintext staging
  decision is argued above rather than assumed.
- **I12** — staging is uniform across platforms, so there is no platform-divergent
  behaviour to document.
