# Chat Extensibility Prerequisites — Design

> Increment 0 of the "file sharing inside chat" arc. Two defects in **already-shipped**
> code (chat 1a/1b + the FFI receive path) that the file-in-chat design review
> surfaced. Both are independently valuable and both must be fixed before a second
> CHAT message type exists. No new capability, no new wire message — this is
> compliance + correctness work.

## Why this exists

Scoping "file sharing inside chat" turned up two problems in code that is already
pushed. Neither is caused by the new feature; both would be *inherited* by it, and
one is user-visible today.

1. **The CHAT handler violates MESSAGE_REGISTRY.md §6.** The registry (derived,
   stability-critical, conforming to invariant I11 "fail-safe, never fail-crash")
   specifies: *"Unknown MessageType within a known channel → governed by the
   frame's `OPTIONAL` flag: `OPTIONAL` set → the receiver **ignores** the message
   and continues (forward compatibility for additive messages); `OPTIONAL` unset
   (i.e. required) → the receiver closes **that channel** with a typed
   `Unsupported` error."* The shipped code ignores the flag entirely and always
   takes the fatal path: `ChatMessage::from_frame` rejects any
   `message_type != MSG_TEXT` with `ChatError::WrongType`
   (`peerbeam-chat/src/message.rs`), `ChatHandler::handle` propagates it
   (`handler.rs`), and the channel actor treats **any** handler `Err` as fatal —
   it emits `ActorEvent::Errored` and breaks its loop
   (`peerbeam-transfer/src/session/channel.rs`), so the manager removes the
   channel. Net today: the forward-compatibility mechanism the registry promises
   does not work. Net the moment a second chat message type ships: a peer that
   does not understand it has its **chat channel torn down**, and because 1b's
   `flush_to_session` is fire-and-forget over one channel (it calls `record_sent`
   + `outbox_remove` after each `send_on_channel` returns `Ok`, which it does for
   at least a round trip after the peer's side died), every queued message behind
   the unknown one is marked **Sent**, dequeued, and **permanently lost**.

2. **Every chat-only dial raises a phantom "incoming file" approval prompt.**
   `Manager::handle_incoming` (`peerbeam-ffi/src/transfer.rs`) unconditionally
   registers a receiving transfer named `"(incoming)"`, emits `transfer_queued`,
   and blocks on `wait_for_accept` (180 s) — **before** `session.next_incoming()`,
   i.e. before it knows whether any stream channel is coming at all. Since chat
   1b, peers dial purely to deliver chat (`chat_flush_peer`, the drain, and
   flush-on-connect all dial), so every inbound chat delivery now presents the
   receiving user with an approval prompt for a file that does not exist, with no
   name and no size. With `auto_accept_trusted` **off** (the default) it sits for
   180 s and resolves as cancelled; with it **on**, `next_incoming()` returns
   `None` and the receiver records a *failed transfer in history* for what was
   just a chat message.

Fixing these first means the file-in-chat increment builds on a receive path that
tells the truth, and on a chat channel whose documented forward-compatibility
actually holds.

## Goal

- Make the CHAT channel behave as MESSAGE_REGISTRY.md §6 specifies: an unknown
  `MessageType` flagged `OPTIONAL` is ignored and the channel survives; an unknown
  *required* type still fails that channel only (current behavior, which is
  already correct for the required case).
- Stop the FFI receive path from inventing a transfer, and prompting for it, when
  no stream channel ever arrives.

Non-goals (explicitly deferred, each with an owner):
- **CHAT capability feature-bit negotiation.** Deferred to the file-in-chat
  increment's first task. Rationale: `Capability { channel, features: u32 }` is
  already on the wire, already documented "additive; unknown bits are ignored",
  and `CapabilitySet::intersect` already ANDs the bits — so the mechanism exists
  and needs no change here. But it has **no consumer until a second message type
  exists**, and nothing today can send one, so building it now would be
  speculative code with no test that could fail. It is a prerequisite *of
  FileRef*, not of these fixes.
- Any FileRef / file-in-chat behavior.
- Reworking approval to be per-file rather than per-connection (needed by
  file-in-chat; out of scope here — this increment only stops the *phantom*
  prompt, it does not change the gate for real transfers).

## Fix 1 — Honor the `OPTIONAL` flag on unknown chat message types

**Where:** `rust/crates/peerbeam-chat/src/handler.rs` (`ChatHandler::handle`).

`ChatMessage::from_frame`'s contract is "decode a text chat message", and
rejecting a non-TEXT type is correct *for that function* — it stays as is. The
registry rule is a **dispatch** concern, so it belongs in the handler, before
decode:

- If `frame.message_type` is a known chat type (today: only `MSG_TEXT = 1`) →
  decode and process exactly as now.
- If it is **unknown** and `frame.flags.contains(MessageFlags::OPTIONAL)` →
  return `Ok(())`. The message is ignored, the channel lives. (Log at debug.)
- If it is **unknown** and `OPTIONAL` is not set → return the current error, so
  the channel fails and only that channel. This preserves the registry's
  "required" branch and keeps the existing
  `handle_rejects_malformed_frame_without_panicking` test valid (its frame uses
  `END_OF_MESSAGE` only, i.e. required).

`MessageFlags::{OPTIONAL, contains}` already exist
(`peerbeam-domain/src/session/frame.rs`); no domain change is needed.

**Consequence for future senders (documented, not built here):** because an
`OPTIONAL` message is *silently* ignored by a peer that does not understand it,
a sender must not treat "sent" as "delivered" for an extension message. That is
precisely what the deferred feature-bit negotiation solves, and why FileRef must
not ship without it.

## Fix 2 — Do not register or prompt for a transfer that never arrives

**Where:** `rust/crates/peerbeam-ffi/src/transfer.rs` (`Manager::handle_incoming`).

Reorder so the transfer bookkeeping is driven by an actual stream channel:

1. Accept the session and run flush-on-connect (unchanged).
2. **Await `session.next_incoming()` first.** If it resolves to `None` — the peer
   opened no stream channel, i.e. this was a chat-only dial — close the session
   and return **without** registering an `active` entry, without emitting
   `transfer_queued`, without prompting, and without writing a history row.
3. Only once a stream channel *is* in hand: register the transfer, emit
   `transfer_queued`, run the approval gate, and proceed exactly as now.

Two ordering details this must respect:

- **The 180 s `ACCEPT_TIMEOUT` must not become an indefinite wait on
  `next_incoming()`.** A peer that establishes a session and opens nothing would
  otherwise hold the handler forever. Bound the wait for the first stream channel
  with a named constant **`STREAM_GRACE = 3 s`** and treat expiry as "chat-only
  dial": close and return quietly. 3 s is chosen because a real sender opens its
  transfer stream immediately after the handshake completes (`open_send_retry` →
  `send_file_on_session`), so the window only has to absorb scheduling jitter, not
  user or network latency — and it is the same order as the existing
  `PEER_PROGRESS_GRACE` (3 s) already used in this file for an analogous
  "did the peer do the thing we expect" wait.
- **Chat frames must keep flowing during that wait.** Chat is dispatched by the
  `ChatHandler` inside the session's own pump task, independently of
  `next_incoming()`, so awaiting the stream channel does not block chat delivery —
  but the implementation must not move chat handling behind this wait.

**Deliberately unchanged:** the approval gate itself (`pending` map,
`accept`/`accept_trust`/`reject`, `AcceptDecision`, `auto_accept_trusted`,
trust-only-on-explicit-trust). Real transfers keep the exact same I6 gate; this
fix only stops fabricating a transfer when there is none.

## Data flow (after)

Peer dials solely to deliver chat → session established → flush-on-connect runs →
chat frames dispatch through `ChatHandler` and land in the thread → no stream
channel appears → grace expires → session closed. **No prompt, no `active` entry,
no history row.** Peer dials to send a file → same path, but a stream channel
arrives → register + `transfer_queued` + approval gate + receive, exactly as
today.

## Error handling

- Unknown `OPTIONAL` chat message → ignored, `Ok(())`, channel unaffected.
- Unknown required chat message → that channel errors; session and all other
  channels unaffected (existing behavior).
- Chat-only dial → quiet close; not an error, not surfaced to the user.
- No `unwrap`/`expect`/`panic!`/`unsafe` in library code.

## Testing

**Fix 1 (`peerbeam-chat`):**
- An unknown `MessageType` with `OPTIONAL` set → `handle` returns `Ok(())`, no
  record persisted, sink not fired.
- An unknown `MessageType` **without** `OPTIONAL` → still `Err` (regression guard
  for the registry's required branch; the existing malformed-frame test covers
  this shape and must keep passing).
- A known TEXT frame is unaffected (persist + dedup + sink, as now).
- **Channel-survival test:** over a real two-`PeerSession` pair, send an unknown
  `OPTIONAL` frame followed by a normal text message on the same channel; assert
  the text message still arrives — i.e. the channel was not torn down. This is the
  test that would have caught the shipped defect.

**Fix 2 (`peerbeam-ffi`):**
- A chat-only dial (peer establishes a session, delivers chat, opens no stream
  channel) emits **no** `transfer_queued`, leaves `active` empty, writes no
  history row, and the chat message still arrives. Verified over real QUIC in the
  existing `chat_ffi` harness.
- A real file send still emits `transfer_queued`, still requires approval, and
  still completes — the existing `transfer_ffi` tests must pass unchanged.

## Files

- Modify: `rust/crates/peerbeam-chat/src/handler.rs` (dispatch rule + tests).
- Modify: `rust/crates/peerbeam-chat/tests/roundtrip.rs` (channel-survival test).
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`handle_incoming` reorder).
- Modify: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` (no-phantom-transfer test).
- Modify: `docs/MESSAGE_REGISTRY.md` — note that the Chat channel honors the §6
  `OPTIONAL` rule as of this increment (derived doc; records reality).

## Risks

- **Reordering `handle_incoming` touches the shipped receive path.** Mitigated by
  keeping the approval gate and every post-stream step byte-identical, and by the
  existing `transfer_ffi` + `transfer_e2e` suites as the regression net.
- **The stream-channel grace window is a new timing constant.** Too short would
  drop a slow sender's transfer; too long would hold a chat-only session open.
  A real sender opens its stream immediately after the handshake, so a short
  grace is safe; the value is stated in the plan and covered by the real-QUIC
  tests both ways.
- **`OPTIONAL`-ignored messages are invisible to the sender.** Called out above;
  this is exactly why FileRef must not ship before feature-bit negotiation, and
  the deferral is recorded here so that ordering is not lost.
