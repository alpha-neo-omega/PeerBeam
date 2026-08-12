# P2P Chat — Increment 1b (Offline store-and-forward) — Design

> Second increment of the chat capability. Extends the approved parent design
> (`docs/superpowers/specs/2026-08-10-p2p-chat-design.md`) and the shipped 1a
> (online chat, all four surfaces). Builds on the existing `peerbeam-chat` crate,
> the `CHAT` channel, and the encrypted AppStore — no parallel systems (I2), no
> wire-protocol change (reuse the `CHAT` channel + `Message = 1`), sender identity
> still from the authenticated session (I6), encrypted at rest (I11).

## Problem

1a delivers chat only when both peers are online at the same instant — a send
fails if the peer isn't reachable. Real chat is fire-and-forget: a message you
send now should reach the peer whenever it next becomes reachable, even if that's
hours later. 1b adds that: an on-disk outbox plus a background drain that delivers
queued messages when the peer returns.

## Reality (P2P, no relay)

PeerBeam has no server, so offline delivery **requires a running host**. A message
queued for an offline peer is delivered only when that peer next (a) comes online,
(b) is discovered (so the host learns its current address), and (c) is dialed by a
long-running process — the running app, or a CLI `daemon`/`watch`. A one-shot
`peerbeam chat send` to an offline peer enqueues the message but cannot itself
deliver it later; a host must be running to drain the outbox. This is inherent to
serverless P2P and is documented for the user, not worked around.

## Decisions (resolved)

- **Outbox: a dedicated `chat-outbox` AppStore namespace.** Entries keyed by a
  time-ordered id (the same `{millis}{rand}` scheme as messages) so the drain
  flushes FIFO. Value: `{peer_id, message_id, body, timestamp}`. Encrypted at rest
  like every AppStore value.
- **Keep queued messages forever (retry indefinitely).** No attempt cap, no TTL —
  a queued message is never dropped; every drain tick retries the undelivered
  entries until they land. Accepted tradeoff: the outbox grows if a peer never
  returns (chat messages are precious; silent loss is worse than unbounded
  retry). A future manual "clear pending" is possible, out of scope here.
- **Send path: always enqueue, then opportunistically flush.** `chat_send`/CLI
  send now: persist the `ChatRecord` as **Pending** + enqueue the outbox, **return
  immediately**, then attempt one opportunistic dial+send; on success mark **Sent**
  and remove the outbox entry; on failure it stays queued for the drain. Online
  sends still deliver at once; offline sends queue. Bonus: this **fixes 1a's
  blocking-send** — the call returns without waiting on the dial+handshake.
- **Drain loop: a background task in the engine/FFI runtime** (mirrors the
  existing periodic loops, e.g. `device_manager`'s `interval`+`loop`). Each tick:
  list the outbox, group by `peer_id`, resolve each peer's **current** address via
  the merged discovery list (`engine.devices()`) — never a stale stored address —
  dial the now-reachable peers, flush their pending messages FIFO over that one
  session, mark Sent + remove the entries.
- **Flush-on-connect.** Whenever a session to a peer is established (inbound
  accept, or an outbound dial for any reason), opportunistically flush that peer's
  pending outbox over the existing session — faster and cheaper than the next tick.
- **Idempotency via existing receiver dedup.** The receiver already dedups by
  `message_id`, so a re-flush after a half-failed drain never duplicates.
- **Surfaces: core + FFI + CLI + a minimal Flutter status indicator.** No
  read/delivery receipts (`Sent` = handed to a live session).

Non-goals (YAGNI): read/delivery receipts, a relay, an attempt/TTL cap or manual
outbox management UI, retry backoff tuning beyond a fixed drain interval,
cross-device outbox sync.

## Architecture

```
peerbeam-chat  (ChatStore gains outbox + status ops; a reusable flush helper)
  ChatStore::enqueue(peer, ChatMessage)                -> put chat-outbox/<oid> = {peer_id,message_id,body,timestamp}
  ChatStore::outbox_pending() -> Vec<OutboxEntry>       (all entries, FIFO by key)
  ChatStore::outbox_for(peer) -> Vec<OutboxEntry>       (one peer's entries, FIFO)
  ChatStore::outbox_remove(oid)                         (after a successful send)
  ChatStore::mark_sent(peer, message_id)                (Pending -> Sent on the conversation record)
  OutboxEntry { oid, peer_id, message_id, body, timestamp }

  flush_to_session(handle, store, peer) -> Result<usize>
     for each outbox_for(peer) in order:
        send the ChatMessage over the (already-open) CHAT channel on `handle`
        on success: outbox_remove(oid) + mark_sent(peer, message_id)
     returns count flushed; a send error stops that peer's flush (stays queued)

engine/FFI runtime
  spawn a periodic drain task (interval DRAIN_EVERY):
     peers = distinct peer_ids in outbox_pending()
     for peer in peers:
        addr = engine.devices() lookup by peer id -> Device (skip if not discovered)
        dial(addr) -> session ; flush_to_session(session.handle, store, peer) ; close
  flush-on-connect: after accept()/dial() establishes a session to `peer`,
     call flush_to_session(session.handle, store, peer) opportunistically.
  chat_send: persist Pending + enqueue; return {id} immediately; spawn an
     opportunistic dial+flush (non-blocking).
  emit `chat_status` event { message_id, peer_id, status } when Pending -> Sent.

CLI
  chat send: persist Pending + enqueue; attempt one opportunistic dial+flush; return.
  daemon / serve_loop / chat watch: run the same drain loop (they have discovery).

Flutter
  ChatMessage already carries `status`. Chat bubble shows a small indicator:
  Pending -> clock/○, Sent -> single check. ChatRepository listens for
  `chat_status` and updates the message's status in place.
```

### `ChatStore` outbox ops (peerbeam-chat)

The outbox is a second namespace in the same encrypted store. `OutboxEntry.oid`
is the AppStore key (time-ordered) so `list("chat-outbox")` returns FIFO order.
`outbox_for(peer)` filters `outbox_pending()` by `peer_id`. `mark_sent` re-`put`s
the conversation record (same `chat-<peer>/<message_id>` key) with `status: Sent`.
All ops go through the `AppStore` port; errors map to `ChatError`; no panics.

### Drain task + flush-on-connect

The drain lives in the **host** (FFI runtime; CLI daemon/serve/watch), not in the
`peerbeam-chat` library — the library exposes `flush_to_session` and the outbox
ops; the host owns the periodic tick, discovery lookup, and dialing (it already
holds the engine, `RouteManager`, identity, enc, trust). `DRAIN_EVERY` is a fixed
interval (e.g. 15s). A drain tick that can't reach a peer (not discovered, or dial
fails) leaves that peer's entries queued for the next tick — no state change.

### Send path change

`send_message` (the pure send-over-an-open-session helper) is unchanged. The
CHANGE is at the entry points: FFI `Manager::chat_send` and CLI `chat send` now
(1) persist Pending + enqueue, (2) return immediately, (3) fire an opportunistic
dial+flush (FFI: spawn on the runtime; CLI: best-effort inline, non-fatal on
failure). `flush_to_session` marks Sent + removes the outbox entry on success.

### Flutter status indicator

`ChatMessage.status` is already parsed. The bubble renders a trailing indicator
for outgoing messages: `pending` → a hollow clock/○ glyph, `sent` → a check.
`ChatRepository` handles a new `ChatStatus`/`chat_status` bridge event by updating
the matching message's status and `notifyListeners()`. (Incoming messages show no
indicator.)

## Data flow

Offline send: `chat send bob "hi"` (bob offline) → persist `chat-bob/<mid>`
{Out, **Pending**} + enqueue `chat-outbox/<oid>` {bob, mid, "hi", ts} → return.
Opportunistic dial fails (bob unreachable) → stays queued. Later bob comes online
and is discovered → a drain tick resolves bob's address, dials, `flush_to_session`
sends "hi", bob's handler dedups+persists+notifies, sender removes the outbox
entry + flips `chat-bob/<mid>` to **Sent** + emits `chat_status` → the sender's UI
tick turns from clock to check.

## Error handling

- Peer not discovered / dial fails during drain → entry stays queued (ret, next
  tick). Not an error surfaced to the user.
- A send error mid-flush stops that peer's flush; already-sent entries are removed,
  the failing one and the rest stay queued (FIFO preserved).
- Re-flush of an already-received message → receiver dedups (no duplicate).
- AppStore/IO errors → `Result`/`ChatError`; no `unwrap`/`expect`/`panic!`/
  `unsafe` in library code.
- `chat_send` returning before delivery is **success = queued**, not delivered;
  the UI reflects Pending until a `chat_status`/history refresh shows Sent.

## Testing

- **ChatStore outbox:** enqueue→outbox_pending/outbox_for returns FIFO; remove
  deletes; mark_sent flips the conversation record Pending→Sent; entries encrypted
  at rest; survives reopen.
- **flush_to_session:** over a real two-`PeerSession` pair, flush delivers all of a
  peer's queued messages in order, marks Sent, empties that peer's outbox; a
  mid-flush send error leaves the remainder queued; the receiver dedups a re-flush
  (send the same queued entry twice → one stored record on the receiver).
- **send-path (offline queue):** a send with no reachable peer persists Pending +
  enqueues and returns; a later flush marks it Sent.
- **drain (host):** with a seeded outbox and a discoverable peer, a drain tick
  delivers + marks Sent + empties the outbox; an undiscoverable peer's entries
  remain. (FFI: an e2e test over real QUIC — enqueue while the peer is down, bring
  a peer up, assert delivery + a `chat_status` event; mirror the 1a `chat_ffi`
  harness.)
- **flush-on-connect:** establishing a session to a peer with pending entries
  flushes them.
- **CLI:** `chat send` to an unreachable `--addr` enqueues (Pending in history);
  a running `watch`/`daemon` drains it when the peer is up.
- **Flutter:** a `chat_status` event flips a message's bubble indicator
  Pending→Sent; ChatRepository updates in place.

## Files (high-level; the plan enumerates exact tasks)

- `rust/crates/peerbeam-chat/src/store.rs` — outbox ops + `mark_sent`.
- `rust/crates/peerbeam-chat/src/{outbox.rs|send.rs}` — `OutboxEntry` +
  `flush_to_session`.
- `rust/crates/peerbeam-ffi/src/{runtime.rs,transfer.rs,events.rs}` — drain task,
  `chat_send` enqueue-and-return + opportunistic flush, flush-on-connect in
  `handle_incoming`, `chat_status` event.
- `rust/bins/peerbeam-cli/src/{chat.rs,commands.rs}` — `chat send` enqueue +
  opportunistic flush; drain loop in `serve_loop`/`daemon`/`chat watch`.
- `flutter/lib/sdk/events.dart` (`ChatStatus`/`chat_status`), `sdk/models.dart`
  (already has status), `data/chat_repository.dart` (handle status),
  `features/chat/chat_screen.dart` (bubble indicator).
- `docs/CLI.md` + a chat note: 1b enables offline delivery; requires a running
  host to drain; `Sent` = handed to a live session (not a read receipt).

## Risks

- **Unbounded outbox** if a peer never returns (keep-forever policy). Accepted;
  documented; a manual clear is a possible later addition.
- **Duplicate delivery windows** — mitigated entirely by the existing receiver
  dedup (idempotent by `message_id`); the drain never assumes a send that errored
  was delivered.
- **Drain thundering / dial churn** — bounded by a single fixed `DRAIN_EVERY`
  interval and one dial per reachable peer per tick, flushing that peer's whole
  queue over one session; flush-on-connect reduces reliance on the tick.
- **Host requirement** — offline delivery only progresses while a host runs;
  surfaced to the user (not a silent limitation).
- **Ordering across a mixed online/offline burst** — FIFO outbox key + in-order
  flush + receiver sort-by-key on read keep a conversation ordered.
