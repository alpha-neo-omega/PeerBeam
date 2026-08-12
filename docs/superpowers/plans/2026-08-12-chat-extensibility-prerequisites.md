# Chat Extensibility Prerequisites Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two defects in already-shipped code that any second CHAT message type would inherit — the chat handler ignoring the frame's `OPTIONAL` flag (a MESSAGE_REGISTRY.md §6 violation that tears down an older peer's chat channel), and the FFI receive path registering + prompting for a transfer before it knows whether one is coming (a phantom file-approval prompt on every chat-only dial).

**Architecture:** Two independent, surgical fixes. Fix 1 adds a dispatch rule to `ChatHandler::handle` *before* decode, so an unknown message type flagged `OPTIONAL` is ignored and the channel survives. Fix 2 reorders `Manager::handle_incoming` so it waits (briefly, bounded) for an actual transfer stream channel *before* doing any transfer bookkeeping — the approval gate itself is untouched, only *when* it runs. Neither fix changes a wire format, a public API, or I6 semantics.

**Tech Stack:** Rust (workspace at `rust/`), existing `peerbeam-chat` + `peerbeam-ffi` crates, tokio.

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in **library** (crate) code. Tests may `unwrap`.
- **Do NOT change I6 approval semantics** — the `pending` map, `accept`/`accept_trust`/`reject`, `AcceptDecision`, `auto_accept_trusted`, and trust-only-on-explicit-trust all keep their current behavior. Only the *order* in which the gate runs relative to obtaining a stream channel changes.
- `STREAM_GRACE: Duration = Duration::from_secs(3)` — a named const, with the existing `PEER_PROGRESS_GRACE` (also 3s) cited as precedent in its doc comment.
- `docs/MESSAGE_REGISTRY.md` §6 is the authority for the `OPTIONAL` rule: *unknown MessageType + `OPTIONAL` set → receiver ignores and continues; `OPTIONAL` clear → receiver closes **that channel** with a typed error.*
- `peerbeam-chat` does **not** depend on `tracing` — do not add a dependency just to log a skipped frame. A comment is sufficient.
- Per-task gate, from `rust/`: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. If the disk fills: `rm -rf rust/target/debug/incremental` and retry (this machine runs tight on space).
- Commit per task; trailer exactly: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Local commits only; do not push.
- Do not modify constitutional docs (ROADMAP.md, docs/VISION.md, docs/FUTURE_ARCHITECTURE.md, docs/ARCHITECTURAL_INVARIANTS.md).
- Tasks 1 and 2 each require an explicit **fail-then-pass** verification of their key regression test (revert the fix, watch the test fail, restore, watch it pass) — reported in the task report.

---

## File Structure

- `rust/crates/peerbeam-chat/src/handler.rs` — the dispatch rule + unit tests (Task 1).
- `rust/crates/peerbeam-chat/tests/roundtrip.rs` — the channel-survival integration test (Task 1).
- `rust/crates/peerbeam-ffi/src/transfer.rs` — `STREAM_GRACE` const + the `handle_incoming` reorder (Task 2).
- `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` — the no-phantom-transfer test (Task 2).
- `docs/MESSAGE_REGISTRY.md` — record that Chat honors §6 (Task 3).

---

## Task 1: Honor the `OPTIONAL` flag on unknown chat message types

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/handler.rs` (imports; `ChatHandler::handle`; `#[cfg(test)] mod tests`)
- Test: `rust/crates/peerbeam-chat/tests/roundtrip.rs` (new channel-survival test)

**Interfaces:**
- Consumes: `MessageFlags::{OPTIONAL, contains}` and `SessionFrame` from `peerbeam_domain::session` (both already exist); `MSG_TEXT` (`pub const MSG_TEXT: u16 = 1`) from `crate::message`, already re-exported at the crate root.
- Produces: no new public API. `ChatHandler::handle`'s behavior changes only for message types other than `MSG_TEXT`.

- [ ] **Step 1: Write the failing unit tests**

In `rust/crates/peerbeam-chat/src/handler.rs`, inside the existing `#[cfg(test)] mod tests`, add these two tests. (The module already imports what the existing tests use; add `MessageFlags` to the `peerbeam_domain::session` import list in the test module if it is not already there.)

```rust
    /// MESSAGE_REGISTRY.md §6: an unknown MessageType flagged OPTIONAL must be
    /// ignored — the message is skipped and the channel survives. Without this,
    /// adding any second chat message type tears down an older peer's channel.
    #[tokio::test]
    async fn handle_ignores_unknown_optional_message_type() {
        let (cs, _dir) = store(5);
        let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cl = received.clone();
        let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        // An additive future message type (e.g. a file reference), marked
        // OPTIONAL by its sender, carrying a body this build cannot parse.
        let unknown = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(2),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{\"whatever\":true}"),
        );

        handler.handle(unknown).await.expect("optional unknown is ignored, not an error");

        assert!(received.lock().unwrap().is_empty(), "sink must not fire");
        assert!(
            cs.history(&peer).unwrap().is_empty(),
            "nothing may be persisted for an ignored frame"
        );
    }

    /// The other half of §6: an unknown MessageType that is NOT optional was
    /// required, so it still fails this channel (and only this channel).
    #[tokio::test]
    async fn handle_still_rejects_unknown_required_message_type() {
        let (cs, _dir) = store(6);
        let sink: ReceivedSink = Arc::new(|_rec| {});
        let (handler, peer_slot) = ChatHandler::new(cs, sink);
        let _ = peer_slot.set(DeviceId::from("pb-sender"));

        let required = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(2),
            MessageFlags::END_OF_MESSAGE, // no OPTIONAL bit
            Bytes::from_static(b"{\"whatever\":true}"),
        );

        let err = handler.handle(required).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
    }
```

- [ ] **Step 2: Run them to verify the OPTIONAL one fails**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat handle_ignores_unknown_optional_message_type`
Expected: FAIL — `handle` currently returns `Err(FrameDecode)` for any non-TEXT type, so `.expect("optional unknown is ignored, not an error")` panics.

- [ ] **Step 3: Add the dispatch rule**

In `rust/crates/peerbeam-chat/src/handler.rs`, extend the `peerbeam_domain::session` import to include `MessageFlags`:

```rust
use peerbeam_domain::session::{
    ChannelType, MessageFlags, MessageHandler, SessionError, SessionFrame,
};
```

and import `MSG_TEXT` alongside `ChatMessage`:

```rust
use crate::message::{ChatMessage, MSG_TEXT};
```

Then in `impl MessageHandler for ChatHandler`, insert the rule into `handle` immediately after the peer-bound check and **before** `ChatMessage::from_frame`:

```rust
        // MESSAGE_REGISTRY.md §6 — an unknown MessageType within a known
        // channel is governed by the frame's OPTIONAL flag: set means the
        // receiver ignores the message and the channel continues (forward
        // compatibility for additive message types); clear means the message
        // was required, so this channel fails — and only this channel.
        //
        // `ChatMessage::from_frame` rejects any non-TEXT type, which is correct
        // for a function whose job is "decode a text chat message" — so the
        // rule belongs here, at dispatch, not there.
        if frame.message_type.get() != MSG_TEXT {
            if frame.flags.contains(MessageFlags::OPTIONAL) {
                // Ignored on purpose: a newer peer sent an additive message this
                // build does not implement. (No log — this crate has no tracing
                // dependency and one is not worth adding for a skipped frame.)
                return Ok(());
            }
            return Err(SessionError::FrameDecode(format!(
                "unsupported chat message type {} (required)",
                frame.message_type.get()
            )));
        }
```

Leave the rest of `handle` (from `let msg = ChatMessage::from_frame(&frame)?;` onward) exactly as it is.

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat --lib`
Expected: PASS — including the pre-existing `handle_rejects_malformed_frame_without_panicking` (its frame uses `MessageType::new(999)` with `END_OF_MESSAGE` only, i.e. *required*, so it must still return `Err`). **Verify that test still passes unchanged; do not edit it.**

- [ ] **Step 5: Write the failing channel-survival integration test**

This is the test that proves the actual defect. Add to `rust/crates/peerbeam-chat/tests/roundtrip.rs`. It reuses the harness from `a_sends_b_receives_and_persists` in that same file verbatim (identities, `MemTransport::pair()`, `PeerSession::open` for both roles, `peer_slot_b.set(a_id)` before the run loops, `tokio::spawn` of each `run()`), then sends a raw unknown-OPTIONAL frame and a normal text message **on the same channel**.

Add `MessageFlags`, `MessageType`, and `SessionFrame` to the `peerbeam_domain::session` import at the top of the file, and `use bytes::Bytes;` (add `bytes` to `[dev-dependencies]` in `rust/crates/peerbeam-chat/Cargo.toml` only if it is not already reachable — `bytes` is already a normal dependency of this crate, so it is available to tests without a change).

```rust
/// Regression (MESSAGE_REGISTRY.md §6): an unknown MessageType flagged OPTIONAL
/// must be ignored WITHOUT killing the channel. Pre-fix, `ChatHandler::handle`
/// returned `Err` for any non-TEXT type and the channel actor treats any handler
/// error as fatal — so the unknown frame tore the chat channel down and the text
/// message that followed it on that same channel never arrived.
#[tokio::test]
async fn unknown_optional_message_does_not_kill_the_chat_channel() {
    let store_b = chat_store(2);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

    let (ta, tb) = MemTransport::pair();
    let (a_ev, _a_ev_rx) = unbounded_channel();
    let (b_ev, _b_ev_rx) = unbounded_channel();
    let (a_ch, _a_ch_rx) = unbounded_channel();
    let (b_ch, _b_ch_rx) = unbounded_channel();
    let (a_in, _a_in_rx) = unbounded_channel();
    let (b_in, _b_in_rx) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let a_id = id_a.device_id.clone();

    let a_cfg = SessionConfig::new(caps());
    let b_cfg = SessionConfig::new(caps()).with_handlers(HandlerRegistry::new().with(handler_b));

    let fa = PeerSession::open(
        ta, SessionRole::Initiator, a_cfg, a_ev, a_ch, a_in, None, id_a, enc_a, trust_a,
    );
    let fb = PeerSession::open(
        tb, SessionRole::Responder, b_cfg, b_ev, b_ch, b_in, None, id_b, enc_b, trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator opens");
    let mut b = rb.expect("responder opens");
    let a_handle = a.handle();
    let _ = peer_slot_b.set(a_id.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // Open ONE chat channel and wait for the peer to accept it.
    let channel = a_handle
        .open_channel(ChannelType::CHAT)
        .await
        .expect("open chat channel");
    let mut opened = false;
    for _ in 0..500 {
        let chans = a_handle.channels().await.expect("channels");
        if chans.iter().any(|c| c.id == channel && c.state.is_open()) {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(opened, "chat channel never opened");

    // 1. An additive future message type this build does not implement,
    //    correctly flagged OPTIONAL by its sender.
    a_handle
        .send_on_channel(
            channel,
            MessageType::new(2),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{\"file\":\"report.pdf\"}"),
        )
        .await
        .expect("send unknown optional frame");

    // 2. A perfectly ordinary text message on the SAME channel, after it.
    let msg = ChatMessage::new("still here").expect("mint");
    let frame = msg.to_frame(channel).expect("encode");
    a_handle
        .send_on_channel(channel, ChatMessage::message_type(), frame.flags, frame.payload)
        .await
        .expect("send text frame");

    // The text must arrive: the unknown frame was skipped, not fatal.
    let mut got: Option<ChatRecord> = None;
    for _ in 0..300 {
        if let Some(first) = received.lock().unwrap().first().cloned() {
            got = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let rec = got.expect(
        "text after an unknown OPTIONAL frame never arrived — the channel was torn down",
    );
    assert_eq!(rec.body, "still here");
    assert_eq!(
        store_b.history(&a_id).expect("history").len(),
        1,
        "exactly one record: the unknown frame persisted nothing"
    );
}
```

- [ ] **Step 6: Verify fail-then-pass on the integration test**

Run it against the fix: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat --test roundtrip unknown_optional_message_does_not_kill_the_chat_channel`
Expected: PASS.

Now prove it is load-bearing: temporarily revert the Step 3 rule (comment out the `if frame.message_type.get() != MSG_TEXT { ... }` block), re-run the same test, and confirm it **FAILS** on the `"the channel was torn down"` expect. Then restore the block and confirm it passes again. Record both outcomes in the report.

- [ ] **Step 7: Full gate + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add rust/crates/peerbeam-chat
git commit -m "fix(chat): honor the OPTIONAL flag on unknown chat message types

MESSAGE_REGISTRY.md section 6 requires an unknown MessageType flagged
OPTIONAL to be ignored so the channel continues; the handler failed the
channel for any non-TEXT type instead, and the channel actor treats a
handler error as fatal. Once a second chat message type exists that tears
down an older peer's chat channel - and because 1b's flush is
fire-and-forget over one channel, every message queued behind the unknown
one is marked Sent, dequeued, and lost.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Do not register or prompt for a transfer that never arrives

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (new `STREAM_GRACE` const near `PEER_PROGRESS_GRACE`; reorder inside `Manager::handle_incoming`)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` (new no-phantom-transfer test)

**Interfaces:**
- Consumes: nothing from Task 1 (independent).
- Produces: no new public API. `handle_incoming`'s observable change: a connection that opens no transfer stream within `STREAM_GRACE` produces no `transfer_queued` event, no `active` entry, no approval prompt, and no history row.

- [ ] **Step 1: Write the failing test**

In `rust/crates/peerbeam-ffi/tests/chat_ffi.rs`, add a test that mirrors the existing `chat_received_into_ffi_and_history_round_trip` harness (line ~358) — a manually-built real QUIC peer that dials **into** the FFI engine and sends one chat message, using the file's existing helpers: `init_ffi(port, dir)`, `peer_identity`, `peer_chat_store`, `session_meta`, `dial_channels_retrying`, `events_snapshot()`, `wait_event`, `call_json`, `take`.

The new assertions, after the chat message is confirmed delivered:

```rust
    // A chat-only dial must not fabricate a transfer. Pre-fix, handle_incoming
    // registered an "(incoming)" transfer and blocked on the approval gate
    // before it knew whether any stream channel was coming — so every inbound
    // chat message raised a phantom file-approval prompt on the receiver.
    let events = events_snapshot();
    let phantom: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("transfer_queued"))
        .collect();
    assert!(
        phantom.is_empty(),
        "a chat-only dial must emit no transfer_queued; got {phantom:?}"
    );

    // ...and no transfer may be left sitting in `active`.
    // `pb_transfers_active` takes no arguments, so it is safe to call directly;
    // its envelope is `{ok, data: {transfers: [...]}}` (Manager::active_list).
    let active = take(pb_transfers_active());
    let list = active
        .get("data")
        .and_then(|d| d.get("transfers"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        list.is_empty(),
        "a chat-only dial must leave no active transfer; got {list:?}"
    );
```

Add `pb_transfers_active` to the `use peerbeam_ffi::{...}` import list at the top of the test file alongside the other `pb_*` functions it already imports.

- [ ] **Step 2: Run it to verify it fails**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-ffi --test chat_ffi`
Expected: FAIL on the `transfer_queued` assertion — the phantom transfer is emitted today.

- [ ] **Step 3: Add the `STREAM_GRACE` const**

In `rust/crates/peerbeam-ffi/src/transfer.rs`, directly after the existing `PEER_PROGRESS_GRACE` const:

```rust
/// How long to wait for the peer's first transfer stream channel before
/// concluding this connection carries no transfer at all. Since chat 1b a peer
/// may dial purely to deliver chat (`chat_flush_peer`, the drain loop,
/// flush-on-connect), and such a dial must not register a transfer or raise an
/// approval prompt. A real sender opens its stream immediately after the
/// handshake, so this only has to absorb scheduling jitter — same order as
/// `PEER_PROGRESS_GRACE` above.
const STREAM_GRACE: Duration = Duration::from_secs(3);
```

- [ ] **Step 4: Reorder `handle_incoming`**

In `Manager::handle_incoming`, leave the `session_exec::accept(...)` block and the flush-on-connect block (including its `events::chat_status` emits) **exactly as they are**. Immediately after the flush-on-connect block, insert the stream-channel wait, and delete the old `let incoming_ch = match session.next_incoming().await { ... }` block further down (the one that currently sits after `transfer_started`):

```rust
        // Only a connection that actually opens a transfer stream is a
        // transfer. Since chat 1b, peers dial purely to deliver chat, so
        // registering a transfer and prompting the user before knowing whether
        // any stream is coming raised a phantom "incoming file" approval for
        // every chat message — and, with auto-accept on, wrote a failed-transfer
        // history row for it. Wait for the stream first, bounded.
        let incoming_ch = match tokio::time::timeout(STREAM_GRACE, session.next_incoming()).await {
            Ok(Some(c)) => c,
            // No stream channel: a chat-only dial. Close quietly — no `active`
            // entry, no transfer_queued, no approval prompt, no history row.
            Ok(None) | Err(_) => {
                session.close().await;
                return;
            }
        };
```

Everything between that and the `drive(...)` call — `let id = self.next_id();`, the `peer` display-name block, `let active = self.register(&id, "receiving", &peer, "(incoming)", None);`, the `transfer_queued` emit, the `auto`/`approved`/`wait_for_accept` approval block, the `if !accepted { ... }` decline path, the `transfer_started` emit, and `*active.status.lock().unwrap() = "transferring".into();` — stays **byte-identical and in the same order**. Only its position relative to obtaining `incoming_ch` changes.

Do not change: the `pending` map, `accept`/`accept_trust`/`reject`, `AcceptDecision`, `auto_accept_trusted`, or trust-only-on-explicit-trust. A stream channel that arrives and then dies mid-transfer must still fail exactly as it does today (that path is inside `drive`/`receive_on_channel` and is untouched).

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-ffi --test chat_ffi`
Expected: PASS — all chat_ffi tests, including the new one.

- [ ] **Step 6: Verify the real-transfer path is unbroken**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-ffi --test transfer_ffi && cargo test -p peerbeam-cli --test transfer_e2e`
Expected: PASS, unchanged — `transfer_ffi` (4 tests, incl. `receive_into_ffi_with_accept` and `send_from_ffi_events_and_stats`) and `transfer_e2e` (5 tests, real two-process QUIC file/folder sends). These are the regression net proving a genuine transfer still registers, still prompts, and still completes.

- [ ] **Step 7: Verify fail-then-pass on the key test**

Temporarily revert Step 4's reorder (move the `incoming_ch` wait back below `transfer_started`, restoring the original `match session.next_incoming().await` form), re-run `cargo test -p peerbeam-ffi --test chat_ffi`, and confirm the new test **FAILS** on the `transfer_queued` assertion. Restore the fix and confirm it passes. Record both outcomes in the report.

- [ ] **Step 8: Full gate + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add rust/crates/peerbeam-ffi
git commit -m "fix(ffi): do not register or prompt for a transfer that never arrives

handle_incoming registered an incoming transfer and blocked on the approval
gate before awaiting the peer's first stream channel. Since chat 1b peers
dial purely to deliver chat, so every inbound chat message raised a phantom
file-approval prompt on the receiver - and with auto-accept on, recorded a
failed transfer in history for what was only a chat message. Wait for the
stream channel first, bounded by STREAM_GRACE; a connection that opens none
closes quietly. The approval gate itself is unchanged.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Record the §6 compliance in the message registry

**Files:**
- Modify: `docs/MESSAGE_REGISTRY.md`

**Interfaces:** none (docs only).

- [ ] **Step 1: Update the Chat entry**

In `docs/MESSAGE_REGISTRY.md` §4 (per-channel MessageType namespaces), the Chat line currently reads:

> **Chat (0x0101):** `Message = 1` (implemented, 1a); `Receipt`, `Reaction`, `Edit` reserved (not implemented).

Extend it to record that the channel now implements the §6 rule — a sentence or two, in the file's existing voice, stating that the Chat handler honors §6: an unknown MessageType flagged `OPTIONAL` is ignored and the channel continues, while an unknown *required* type fails that channel only. Do not restate §6 in full; reference it.

Do not touch any other section, and do not touch the constitutional documents.

- [ ] **Step 2: Gate + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check` (no Rust changed; confirms nothing broke).

```bash
git add docs/MESSAGE_REGISTRY.md
git commit -m "docs: record that the Chat channel honors the section 6 OPTIONAL rule

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Fix 1 (honor `OPTIONAL`, dispatch-level, `from_frame` untouched, required branch preserved) → Task 1 Steps 3-4. ✓
- Fix 1's four required tests (optional-ignored; required-still-errors; known TEXT unaffected; channel-survival over a real two-`PeerSession` pair) → Task 1 Steps 1 and 5; "known TEXT unaffected" is covered by the pre-existing `handle_persists_and_notifies_once_bound` / `handle_dedups_same_message_id` tests, which Step 4 requires to keep passing. ✓
- Fix 2 (await stream first, bounded by `STREAM_GRACE = 3s` with the `PEER_PROGRESS_GRACE` precedent; chat-only dial closes quietly with no `active`/`transfer_queued`/prompt/history; approval gate byte-identical; mid-transfer failure path preserved) → Task 2 Steps 3-4. ✓
- Fix 2's tests (chat-only dial emits nothing over real QUIC; real transfers unchanged via `transfer_ffi` + `transfer_e2e`) → Task 2 Steps 1, 5, 6. ✓
- Registry doc note → Task 3. ✓
- Deferred items (feature-bit negotiation; per-file approval-with-a-name) → correctly absent from every task. ✓
- Both fail-then-pass verifications → Task 1 Step 6, Task 2 Step 7. ✓

**Type consistency:** `MSG_TEXT: u16` compared against `frame.message_type.get()` (also `u16`) ✓; `MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE)` matches the existing `with`/`contains` API in `peerbeam-domain/src/session/frame.rs` ✓; `SessionError::FrameDecode(String)` matches the variant the existing tests assert on ✓; `STREAM_GRACE: Duration` matches the sibling consts' type ✓; `tokio::time::timeout(..., session.next_incoming())` yields `Result<Option<IncomingStreamChannel>, Elapsed>`, so `Ok(Some(c)) | Ok(None) | Err(_)` is exhaustive ✓.

**Placeholder scan:** none — every code step carries real code or an exact command. All API shapes used above were verified against the source while writing this plan: `ChannelInfo { id, channel_type, state, stats }` with `state.is_open()` (`peerbeam-transfer/src/session/channel.rs:40`); `pb_transfers_active()` is argument-free (so safe to call) and returns `{ok, data: {transfers: [...]}}` via `Manager::active_list` (`peerbeam-ffi/src/lib.rs:269`, `transfer.rs`); `MSG_TEXT: u16 = 1` (`peerbeam-chat/src/message.rs:16`); `PEER_PROGRESS_GRACE = Duration::from_secs(3)` (`peerbeam-ffi/src/transfer.rs:1272`). The one step that asks the implementer to read before writing is Task 3 Step 1 (the Chat line's exact current wording), because that is a prose edit whose surrounding voice must be matched rather than a shape to be guessed.
