# Secure P2P Chat (text/markdown) — Design

> Phase B, capability 1 — the roadmap flagship ("the first productivity wins,
> each a message type on the session"). Conforms to the constitutional set:
> rides PeerSession as a new capability/channel (no parallel system, I2), sender
> bound to the authenticated peer (I6), negotiated via capabilities (I9),
> persisted encrypted at rest (I11). The registry already reserves
> `ChannelType 0x0101 = Chat` (MESSAGE_REGISTRY.md §2) — this design implements
> it. One capability; built in two verified increments (1a online, 1b offline).

## Problem

PeerBeam moves files but has no messaging. Phase B's flagship is secure,
device-to-device chat: send text/markdown to a trusted peer, keep the history
locally and encrypted, and — because devices are not always both online — have a
message reach its peer whenever the peer next becomes reachable. It must reuse
the Phase-A substrate (PeerSession typed messages + encrypted AppStore) rather
than invent a second transport or store.

## Goal

1:1 text/markdown chat between two trusted devices:
- a new `Chat` message channel on PeerSession, negotiated via capabilities;
- messages persisted per-conversation in the encrypted AppStore, dedup-safe;
- **offline store-and-forward** — an outbox holds undelivered messages and the
  engine drains it when the peer becomes reachable;
- exposed on all four surfaces (core, CLI, FFI, a minimal Flutter chat thread).

Built in two increments so a working chat ships and is verified before the
offline machinery lands:
- **1a — online chat, all surfaces** (send fails if the peer is offline);
- **1b — offline delivery** (outbox + periodic drain + flush-on-connect + status).

## Decisions (resolved)

- **Transport: a new `Chat` message channel** (`ChannelType::CHAT = 0x0101`,
  already reserved). Chat is discrete typed messages, so it uses the *message*
  channel path (`open_channel` + `send_on_channel` + a `MessageHandler`), NOT the
  byte-stream path transfers use. This is the intended extension point
  (`handler.rs` names "chat, presence, … in later phases").
- **Message set (this milestone): `Message` = MessageType `1`.** `Receipt`,
  `Reaction`, `Edit` stay reserved (deferred). Chat `Message` is a **required**
  message (OPTIONAL flag clear); a peer lacking the Chat capability never
  advertises it, so the channel is simply never opened (registry §6).
- **Sender identity = the authenticated session peer**, never the payload. Chat is
  only with pinned peers; first contact still pins via TOFU. The wire message
  carries no sender field.
- **Persistence: per-conversation namespace `chat-<peer_device_id>`** in the
  AppStore, key = a lexicographically time-ordered id, so `list()` returns the
  conversation in chronological order. Values are sealed at rest (I11).
- **Time-ordered id without a new dependency:** `format!("{:013}{:016x}", unix_ms,
  rand_u64)` — sortable by time, collision-resistant. (Avoids pulling a ULID
  crate; a real ULID is a possible later swap, not needed now.)
- **Offline store-and-forward via a dedicated `chat-outbox` namespace** (1b). One
  place lists all undelivered outbound across peers (AppStore has no
  list-namespaces op). Drain removes the entry and flips the conversation record
  to `sent`.
- **Delivery signal = `sent` when handed to a live session.** No delivery/read
  receipts in M1 (that needs the `Receipt` message + more state) — deferred.
- **On-demand sessions.** To send, establish (or reuse) a session to the peer and
  open a Chat channel; to receive, the device must be listening (the running
  app/daemon accepts inbound Chat channels, same model as receive). The 1b drain
  loop dials peers that have pending outbox items.

Non-goals (YAGNI, none precluded): file-in-chat (sibling milestone, reuses the
transfer channel), delivery/read receipts, typing indicators, reactions, edit or
delete, group chat, markdown-rendering polish, message search.

## Architecture

```
peerbeam-domain
  session::ChannelType::CHAT (0x0101)                     (add the constant)
  (MessageHandler, SessionFrame, MessageType already exist)

peerbeam-chat  (new crate — the capability's core, transport-agnostic)
  ChatMessage { id, timestamp, body }   wire type; to_frame/from_frame (JSON),
                                        MessageType Message=1, mirrors ControlMessage
  ChatRecord  { id, peer_id, direction, timestamp, body, status }  persisted type
  ChatStore   (thin wrapper over AppStore): append(record), history(peer)->Vec,
              dedup check by id, (1b) outbox enqueue/list/remove, mark_sent
  ChatHandler : MessageHandler (channel_type = CHAT)
              on inbound frame -> decode ChatMessage -> dedup by id ->
              persist ChatRecord{direction:in,status:received} -> emit event
  send(session_handle, store, peer_id, body) -> open/reuse CHAT channel,
              persist out record, send_on_channel(Message); (1b) enqueue+drain

peerbeam-engine / peerbeam-ffi runtime
  register ChatHandler in the session HandlerRegistry; advertise CHAT capability
  in SessionConfig; (1b) periodic outbox drain + flush-on-session-established

surfaces
  CLI  (bins/peerbeam-cli): peerbeam chat send <peer> <text> | history <peer> | watch
  FFI  (crates/peerbeam-ffi): pb_chat_send, pb_chat_history; chat_received event
  Flutter (flutter/lib): ChatRepository + SDK bindings + a minimal chat screen
```

### Wire message (`ChatMessage`, on `ChannelType::CHAT`, MessageType `Message=1`)

```rust
struct ChatMessage { id: String, timestamp: String /*RFC3339*/, body: String }
```
Encoded/decoded exactly like `ControlMessage` (serde_json payload in a
`SessionFrame`, flags `END_OF_MESSAGE`). `body` is raw markdown, capped at **16
KiB** (a message over the cap is rejected before send / dropped-with-error on
receive — never truncated silently). `id` and `timestamp` are the sender's; the
receiver trusts the session for *who* sent it, not the payload.

### Persisted record (`ChatRecord`, AppStore namespace `chat-<peer_device_id>`)

```rust
struct ChatRecord {
  id: String,            // == wire id (dedup key); also the AppStore record key
  peer_id: String,       // the conversation's peer device id
  direction: Direction,  // Out | In
  timestamp: String,     // RFC3339
  body: String,          // markdown
  status: Status,        // Pending | Sent | Received
}
```
Serialized to opaque bytes (serde_json) and sealed by the AppStore. Namespace
`chat-<peer_device_id>` — device ids are `pb-<hex>`, which matches the AppStore
namespace charset. `history(peer)` = `AppStore::list("chat-<peer>")` (already
sorted ascending by key = chronological).

### Handler + dispatch (inbound)

`ChatHandler::handle(frame)` decodes the `ChatMessage`, resolves the sender from
the session (the handler is constructed per session with the authenticated
`peer_id`), **dedups** (skip if `chat-<peer>` already has key = `id`), persists a
`Received` record, and emits a `chat_received` event to the surfaces. A decode/
oversize error is a channel-scoped error (never a session crash), per the
dispatcher contract.

### Increment 1a — online chat (ships first)

Everything above **except** the outbox/drain. Send: resolve/establish a session
to the peer, open a Chat channel, persist an `Out` record (status `Sent` on
success), `send_on_channel`. If no session can be established (peer offline),
**send fails** with a clear error and the record is not marked delivered. Full
CLI/FFI/Flutter. Receiver + dedup + persistence + history all present. This is a
complete, testable chat when both peers are online.

### Increment 1b — offline store-and-forward (layers on 1a)

Add: `chat-outbox` namespace; on send, write the record `Pending` + enqueue
outbox, send-now-if-online else leave queued; a periodic engine **drain loop**
(and a flush triggered whenever a session to a peer is established, inbound or
outbound) that sends queued messages in order, removes the outbox entry, and
flips the record `Pending → Sent`. Receiver dedup (already in 1a) makes re-flush
idempotent. No schema rework — 1a already writes records with a `status`.

## Data flow

Send (1b): `chat send bob "hi"` → mint id → persist `chat-bob/<id>`
{Out,Pending} + enqueue `chat-outbox/<id>` → if session to bob live, send
`Message` frame → on success remove outbox entry + set record `Sent`. Offline:
stays queued; drain loop later dials bob and flushes. Receive: bob's `ChatHandler`
gets the frame → dedup by id → persist `chat-<me>/<id>` {In,Received} → emit
`chat_received` → CLI prints / Flutter appends to the thread.

## Error handling

- Oversize body (>16 KiB) → rejected on send (`Err`) and on receive (channel
  error), never truncated.
- Decode/serialization failure on inbound → channel-scoped error; session
  survives (dispatcher contract).
- Duplicate id on receive → silently ignored (idempotent), not an error.
- Send with no reachable peer → 1a: `Err` surfaced to the caller; 1b: queued
  (`Pending`), not an error.
- AppStore/IO failures → propagate as `Result`; no `unwrap`/`expect`/`panic!`/
  `unsafe` in library code.

## Testing

- **chat core:** `ChatMessage` frame round-trip (encode/decode, like the control
  tests); oversize body rejected; `ChatStore` append→history round-trips in
  chronological order and survives reopen; **dedup** — the same id twice yields
  one record; record values are sealed on disk (not plaintext).
- **channel/handler:** a real two-`PeerSession` round trip (mirroring the
  transfer/session tests) — send a `Message` on the Chat channel, assert the peer
  persists it and emits the event; an unknown/oversize frame errors the channel,
  not the session.
- **1b outbox:** offline send enqueues `Pending`; drain flushes in order, removes
  the outbox entry, flips to `Sent`; a re-flush (duplicate) does not duplicate on
  the receiver; ordering preserved.
- **CLI:** `chat send` then `chat history` shows the message; `watch` prints an
  inbound message; JSON output carries the record fields.
- **FFI:** `pb_chat_send` persists + (online) delivers; a `chat_received` event
  fires on inbound with the message fields; `pb_chat_history` returns the thread.
- **negotiation:** two peers both advertising CHAT open the channel; a peer
  without the capability never opens it (no crash).

## Files (high-level; the plan enumerates exact tasks)

- `rust/crates/peerbeam-domain/src/session/ids.rs` — add `ChannelType::CHAT`.
- `rust/crates/peerbeam-chat/` — new crate: `ChatMessage`, `ChatRecord`,
  `ChatStore`, `ChatHandler`, `send`. Add to workspace members.
- `rust/crates/peerbeam-engine` + `rust/crates/peerbeam-ffi/src/runtime.rs` —
  register the handler, advertise the capability, (1b) the drain loop.
- `rust/crates/peerbeam-ffi/src/lib.rs` — `pb_chat_send`, `pb_chat_history`;
  `chat_received` event via the existing event bridge.
- `rust/bins/peerbeam-cli/` — `chat` subcommand (`send`/`history`/`watch`).
- `flutter/lib` — `ChatRepository`, SDK bindings, a minimal chat screen.
- `docs/MESSAGE_REGISTRY.md` — record the concrete Chat MessageType ids
  (`Message`=1; Receipt/Reaction/Edit reserved). Derived doc; conforms.
- `docs/` — a short chat note in the appropriate guide (CLI.md, and a chat/
  security mention).

## Risks

- **Milestone size.** Chat + offline + 4 surfaces is large — mitigated by the
  1a/1b split: 1a is a complete, reviewable vertical slice; 1b layers on without
  rework.
- **Session lifecycle for chat.** Sending needs a session to the peer; the app
  must be listening to receive. 1a dials on demand; 1b adds a drain loop that
  dials peers with pending items. Holding long-lived per-peer sessions is a later
  optimization, not required for correctness.
- **New crate vs folding into transfer.** Chat is its own capability with its own
  store and handler; a dedicated `peerbeam-chat` crate keeps one responsibility
  per module (avoids growing `peerbeam-transfer`). Follows the existing
  one-capability-per-crate layout.
- **Ordering across reconnects (1b).** Time-ordered ids + in-order outbox drain
  keep a conversation ordered; the receiver sorts by key on read, so minor
  send/receive interleaving still displays chronologically.
- **No delivery receipt in M1.** `Sent` means "handed to a live session," not
  "peer stored it." Acceptable for M1; a `Receipt` message is the natural 1c/2
  follow-up.
