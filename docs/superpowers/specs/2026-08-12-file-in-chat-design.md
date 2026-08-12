# File Sharing Inside Chat — Design

> Phase B capability (FEATURE_CATALOG.md: *"unify 'send a file' and 'talk about it'
> on one channel"*). Builds on chat 1a (online text), 1b (offline
> store-and-forward), and increment 0 (chat extensibility prerequisites) — all
> shipped. Conforms to the constitutional set: rides PeerSession as a negotiated
> capability (I9), file bytes stay on the existing TRANSFER machinery (no parallel
> system, I2), the receiver's approval gate is reused unchanged (I6), records are
> encrypted at rest (I11), and every wire/schema addition is additive and
> backward-compatible.

## Problem

Chat carries text; files go through a separate "Send files" flow. A user who
attaches a file in a conversation has no way to do it, and a file sent alongside a
conversation leaves no trace in that conversation. The catalog's framing is exact:
*unify sending a file and talking about it on one channel.*

## What this is NOT

File **bytes** do not move to the CHAT channel. CHAT is a *message* channel
(discrete frames dispatched to a `MessageHandler`); TRANSFER is a *stream* channel
with chunking, resume, integrity, pause/cancel and progress already solved.
Re-implementing streaming on CHAT would be a parallel system (I2). This feature is
a **control-plane correlation**: a small CHAT message places a row in the
conversation and binds it to a transfer that rides the existing TRANSFER path.

## Design review findings that shaped this

A five-lens review (reuse audit, roadmap overlap, UX completeness, adversarial gap
hunt, completeness critic) produced 117 findings against an earlier sketch. Three
of that sketch's load-bearing assumptions were false, and they are corrected here:

1. **"Reuse the FileRef id as the transfer id — one id, no mapping table"** was
   false. Three ids exist: the sender's FFI mints `tx-<pid>-<n>`, the receiver
   mints *another*, and the sender's id — which does travel on the wire in
   `TransferMeta` — is decoded and discarded (`Received` has no `transfer_id`).
   Correlation needs real plumbing (see *Correlation*).
2. **"An old peer ignores an unknown message type"** was false — it tore the chat
   channel down, and 1b's fire-and-forget flush then marked everything queued
   behind it Sent and dequeued it. **Fixed in increment 0**; this design adds the
   second half (feature negotiation) so a sender never emits a FileRef a peer
   cannot read.
3. **"Reuse the existing approval gate"** was true but insufficient: the gate is
   per-*connection* and fired before any file metadata existed, so the prompt said
   `"(incoming)"` with no name or size. Increment 0 moved the gate after the
   stream channel is in hand, which makes the fix here cheap.

## Decisions (resolved)

- **Wire message: `FileRef`, chat MessageType `2`**, sent on the CHAT channel with
  the `OPTIONAL` flag set (so a peer that somehow receives it without negotiating
  skips it rather than failing the channel — increment 0 made that safe).
- **Feature negotiation (the deferred prerequisite, now built).**
  `Capability { channel, features: u32 }` is already on the wire, already
  documented "additive; unknown bits are ignored", and `CapabilitySet::intersect`
  already ANDs the bits — no wire change. Define `CHAT_FEAT_FILEREF = 1 << 0`;
  advertise `Capability::with_features(CHAT, CHAT_FEAT_FILEREF)`; a sender emits a
  FileRef **only** when the negotiated set carries that bit. Peers already in the
  wild advertise `features: 0`, so they are never sent one. This is what makes
  "sent" mean "delivered" for an extension message.
- **Correlation: the sender supplies the transfer id, and the receiver reads it
  from the wire instead of minting one.** `TransferMeta.transfer_id` already
  crosses the wire; today it is dropped. Changes: `Manager::send` accepts an
  optional caller-supplied id; `Received`/`FolderReceived` carry `transfer_id`;
  and the receiver — which since increment 0 holds the stream channel *before*
  registering — **peeks the transfer meta first**, then registers with the
  sender's id, real name and real size. One id on both ends, no mapping table,
  and the approval prompt gains a name and size as a side effect.
- **Ordering rule: the FileRef is sent first, on the CHAT channel, before the
  transfer stream is opened.** Both directions are handled explicitly (see
  *Ordering and orphans*) — neither may produce a ghost row or a duplicate.
- **Record schema grows additively.** `ChatRecord` gains
  `#[serde(default)] kind: Kind` (`Text` | `File`, defaults `Text` so every
  already-persisted 1a/1b record still decodes) and
  `#[serde(default)] file: Option<FileMeta>`. `Status` gains file-lifecycle
  variants. **The outbox round-trip must preserve them** — 1b's `record_sent`
  rebuilds a `ChatRecord` from `OutboxEntry`'s own fields, which would silently
  drop `kind`/`file`; that is a real defect this feature must fix, not inherit.
- **Two distinct types, never one.** Wire `FileRef { id, timestamp, name, size }`
  and record-side `FileMeta { name, size, local_path }`. Sharing one struct — the
  obvious way to satisfy "additive" — would serialize the sender's absolute path
  into every frame, leaking their username, directory structure and folder names
  to the peer. A test asserts the encoded frame contains no `local_path` key.
- **`FileRef.name` is untrusted input**: capped (255 bytes), and it must
  round-trip through `Path::file_name()` (rejecting `../`, separators, empty). The
  displayed name after the transfer lands is the receiver-side **sanitized** name,
  so the bubble can never disagree with the file on disk.
- **Approval is the existing gate, unchanged** (`pending` map,
  `accept`/`accept_trust`/`reject`, `AcceptDecision`, `auto_accept_trusted`,
  trust-only-on-explicit-trust). It simply now carries a name and size, and the
  chat thread can render it inline because the ids match.
- **Folders are refused in this feature** with a clear message. `FileRef` cannot
  describe a tree, and folders use a manifest on the wire. `Manager::send` already
  validates "exists, not a directory"; this only needs a chat-side message saying
  why. (Desktop drag-and-drop and the picker both surface directories, so this is
  the most likely first failure.)
- **Multi-select fans out to N FileRefs and N transfers over one session**, not a
  silent "sent one of five".
- **Scope split: 2a online, 2b offline** — see below.

Non-goals (explicitly out; state them in the docs so their absence reads as a
decision): thumbnails and previews, folder attach, pause/resume/cancel from the
bubble (the Transfers screen still offers them), receipts, reactions,
drag-and-drop onto a thread, deep-linked notifications, grouped multi-file
bubbles, and a Retry button (automatic retry covers the recoverable cases; when
the source is gone the honest answer is re-pick).

## Scope split

The review's *minimum coherent feature* is eleven behaviors. Delivering them plus
the offline queue in one milestone would exceed chat 1a and 1b combined, and the
review found that **most of the severe findings live specifically in the offline
path** (Android's picker cache self-wipes on every pick; a declined file either
vanishes or re-prompts every 15s forever; dequeue-on-start silently drops 1b's
keep-forever guarantee; a multi-GB file at the head of the FIFO blocks every text
message and every other peer behind it).

So this ships in two increments, mirroring the 1a→1b pattern that worked:

- **2a — online file-in-chat (this build).** Negotiation, correlation, the record
  schema, approval-with-a-name, terminal states, boot reconciliation, and all four
  surfaces. Attaching to an offline peer fails with a clear message.
- **2b — offline file queueing (next).** The outbox carries file entries: staging
  out of Android's cache into outbox-owned storage, source-changed detection,
  a file queue separate from text so a large upload cannot block messages, and an
  explicit declined-terminal signal so a refused file neither vanishes nor
  re-offers forever.

**This does not drop the offline commitment** — it sequences it so the
correlation model is proven before the queue is built on top of it.

## Architecture (2a)

```
peerbeam-domain
  session::CHAT_FEAT_FILEREF: u32 = 1 << 0        (feature bit for the CHAT capability)

peerbeam-chat
  FileRef { id, timestamp, name, size }            wire type, MessageType FILE_REF = 2,
                                                   flags OPTIONAL|END_OF_MESSAGE
  FileMeta { name, size, local_path }              record-side only, never serialized to a frame
  ChatRecord { .., kind: Kind, file: Option<FileMeta> }   additive, serde(default)
  Kind { Text, File }                              Status += PendingApproval|Transferring|Declined|Failed|Interrupted
  ChatHandler                                      dispatches FILE_REF -> persists a File record (PendingApproval)
  prepare_file_send(store, peer, path) -> (FileRef, ChatRecord)
                                                   validate (exists, not a dir, name sane), mint the id,
                                                   persist the Out/File record, return both
  ChatStore::set_status(peer, id, Status)          drive the record from transfer outcomes
  ChatStore::reconcile_on_load()                   Transferring/PendingApproval -> Interrupted at open

peerbeam-transfer
  TransferMeta.transfer_id                         already on the wire; now READ, not dropped
  Received/FolderReceived += transfer_id           so the receiver can use the sender's id

peerbeam-ffi
  Manager::send(.., transfer_id: Option<String>)   caller-supplied id (chat supplies the FileRef id)
  handle_incoming                                  peek meta -> register with the sender's id + real name/size
  events::transfer payload += peer_id              so a surface can route an event to the right thread
  pb_chat_send_file({peer, path}) -> {id}
  chat_received / chat_status                      carry file records too

peerbeam-cli
  peerbeam chat send --file <path>                 attach; chat history/watch render file rows

flutter
  attach button in the composer; file bubble (name, size, progress, inline
  accept/decline, open); thread reachable for offline peers
```

### Correlation, end to end

1. Sender validates the path, mints a `FileRef` id (the time-ordered chat id),
   persists an `Out`/`File` record as `Transferring`, and — **only if the peer
   negotiated `CHAT_FEAT_FILEREF`** — sends the `FileRef` on the CHAT channel.
2. Sender starts a normal transfer, passing that same id as the caller-supplied
   `transfer_id`, so `TransferMeta.transfer_id` on the wire equals the FileRef id.
3. Receiver's `handle_incoming` obtains the stream channel (increment 0), **peeks
   the transfer meta**, and registers the transfer under the *sender's* id with
   the real name and size — so `transfer_queued` now carries `{transfer_id, name,
   size, peer_id}`.
4. Receiver's chat thread already holds a `PendingApproval` File record with that
   id (from step 1), so the approval renders inline on the correct row. Accept /
   decline flow through the unchanged gate.
5. Transfer outcome events (`transfer_completed` / `_failed` / `_cancelled`) drive
   `set_status` on both ends, so the record is never a lie.

### Ordering and orphans

- **FileRef then transfer** (the normal case) — handled above.
- **FileRef with no transfer**: the record stays `PendingApproval` and is swept to
  `Expired` after a bounded lifetime. Without this rule a peer can plant unlimited
  permanent fake "incoming file" rows for free, with no bytes and no approval, and
  the product has no delete-message anywhere.
- **Transfer with no FileRef** (peer didn't negotiate, or the CHAT frame was
  lost): it behaves exactly as a standalone transfer does today — it appears in
  the Transfers screen and history, and **no chat row is invented**. No ghost, no
  duplicate.
- **Boot reconciliation**: transfer ids are process-scoped and no event replays
  after a crash, so any record left `Transferring`/`PendingApproval` at store load
  becomes `Interrupted`. Otherwise a bubble spins forever across every launch.

## Data flow

`chat send --file report.pdf` to a reachable peer → validate → mint id → persist
`{Out, File, Transferring, file:{name,size,local_path}}` → dial → check negotiated
features → send `FileRef{id,name,size}` on CHAT → start the transfer with
`transfer_id = id` → receiver peeks meta, registers under that id with the real
name/size, and its chat thread (already holding the `PendingApproval` row from the
FileRef) renders Accept/Decline inline → on accept, bytes stream over TRANSFER
with the existing integrity/resume → both sides' records move to `Received`/`Sent`
via transfer events → the receiver's bubble offers Open at the saved path.

## Error handling

- Peer offline / unreachable (2a) → send fails with a clear message; nothing is
  left half-persisted. (2b turns this into a queue.)
- Peer has not negotiated `CHAT_FEAT_FILEREF` → do not send a FileRef. Either send
  the file as a plain transfer and say so, or refuse — **refuse**, so the user is
  never told a chat attachment landed in a thread the peer cannot see.
- Path is a directory → refused with an explicit "folders aren't supported in chat
  yet" message.
- `FileRef.name` fails validation (too long, not a bare filename) → the frame is
  rejected on receive; no record is written.
- Declined / expired / failed / cancelled → each lands distinctly in the record;
  `declined` must read differently from `failed`.
- Source file missing at send time → refused before anything is persisted.
- No `unwrap`/`expect`/`panic!`/`unsafe` in library code.

## Testing

- **Negotiation:** two peers advertising the bit exchange a FileRef; a peer
  advertising `features: 0` is never sent one (the sender refuses and says why);
  a FileRef arriving at a non-supporting build is skipped without killing the
  channel (increment 0's rule, re-asserted here).
- **Correlation (the crux):** over a real two-`PeerSession` pair, a file sent in
  chat produces the *same* id on the sender's record, the wire meta, and the
  receiver's `transfer_queued` — and the receiver's approval prompt carries the
  real name and size, not `"(incoming)"`.
- **Privacy:** the encoded `FileRef` frame contains no `local_path` key.
- **Name safety:** `../escape`, an absolute path, an over-long name, and an empty
  name are each rejected on receive; the bubble shows the receiver-side sanitized
  name.
- **Ordering/orphans:** a FileRef with no transfer expires rather than spinning
  forever; a transfer with no FileRef creates no chat row; boot reconciliation
  turns a mid-flight record into `Interrupted`.
- **Terminal states:** decline, timeout-expiry, and mid-transfer failure each land
  distinctly in the record on both ends.
- **Schema compatibility:** a 1a/1b-era `ChatRecord` JSON (no `kind`, no `file`)
  still decodes as `Text`; an outbox round-trip preserves `kind`/`file`.
- **Folders + multi-select:** a directory is refused with the specific message;
  selecting five files produces five rows, not one.
- **Regression nets:** the existing `transfer_ffi`, `transfer_e2e`, `chat_ffi` and
  `roundtrip` suites must pass unchanged — the transfer path is shared, and this
  feature must not disturb standalone sends.
- **Flutter:** a file bubble renders name/size/progress and an inline
  accept/decline; a received bubble opens the file (with the Android SAF fallback
  the History screen already uses); the thread is reachable for an offline peer.

## Files (high level; the plan enumerates tasks)

- `rust/crates/peerbeam-domain/src/session/` — `CHAT_FEAT_FILEREF`.
- `rust/crates/peerbeam-chat/src/` — `FileRef`, `FileMeta`, `Kind`, `Status`
  variants, handler dispatch for `FILE_REF`, `prepare_file_send`, `set_status`,
  `reconcile_on_load`, outbox field preservation.
- `rust/crates/peerbeam-transfer/src/` — `transfer_id` on `Received`/
  `FolderReceived`; meta peek exposed for the receiver.
- `rust/crates/peerbeam-ffi/src/` — caller-supplied id on `send`, `handle_incoming`
  meta peek + register-with-real-name, `peer_id` on transfer events,
  `pb_chat_send_file`, chat event payloads.
- `rust/bins/peerbeam-cli/src/` — `chat send --file`, file rows in
  `history`/`watch`.
- `flutter/lib/` — attach button, file bubble, inline approval, open-file,
  offline-reachable thread.
- `docs/MESSAGE_REGISTRY.md` — Chat `FileRef = 2` implemented;
  `CHAT_FEAT_FILEREF` recorded. `docs/CLI.md` — `chat send --file`.

## Risks

- **Two channels, one logical action.** Mitigated by the explicit ordering rule
  and by defining all three non-happy orderings rather than leaving them to chance.
- **Touching the shared transfer path.** The caller-supplied id and the meta peek
  change code every standalone send also runs; the existing `transfer_ffi` /
  `transfer_e2e` suites are the regression net and must pass unchanged.
- **Approval prompt semantics change** (it gains a name/size and a correlated id).
  This is strictly more information for the user; the gate's decision logic is
  untouched.
- **`FileRef.name` is attacker-controlled.** Capped, validated as a bare filename,
  and the post-transfer display uses the receiver-side sanitized name.
- **Scope.** Even split, 2a is large. The plan must keep each task independently
  reviewable, and the correlation work must land before any surface work builds on
  it.
