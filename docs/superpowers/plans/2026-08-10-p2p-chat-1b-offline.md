# P2P Chat — Increment 1b (Offline store-and-forward) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add offline store-and-forward to chat — a message to an unreachable peer is queued in an encrypted outbox and delivered by a background drain when the peer next comes online, with the message's status tracked Pending→Sent across all surfaces.

**Architecture:** A single `chat-outbox` AppStore namespace (keyed by message id) holds undelivered outbound messages. `ChatStore` gains outbox ops + `record_sent`; `peerbeam-chat` gains `flush_to_session` (open the CHAT channel once, send each queued message, mark Sent + dequeue). Send becomes enqueue-Pending-then-opportunistic-flush (also removes 1a's blocking send). A drain loop — hosted in the FFI runtime (capturing the engine+manager Arcs) and the CLI `serve_loop`/`watch` — periodically resolves peers-with-pending via discovery, dials, and flushes. Flush-on-connect piggybacks any established session. A `chat_status` event drives the Flutter Pending→Sent bubble tick. Keep-forever: no attempt/TTL cap. No wire-protocol change (reuse `ChannelType::CHAT` + `Message=1`).

**Tech Stack:** Rust (workspace `rust/`), the shipped `peerbeam-chat` crate + AppStore + PeerSession; Dart/Flutter FFI + Material 3.

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in **library** (crate) code. The CLI **binary** may use `expect` only where existing style does. Tests may `unwrap`.
- **Sender identity is always the authenticated session peer** (`session.peer_device`), never the wire payload.
- **No wire-protocol change:** reuse `ChannelType::CHAT` + `MessageType Message = 1`. No new frame/message types.
- **Keep-forever outbox:** no attempt cap, no TTL — a queued entry is retried every drain tick until delivered; never dropped.
- AppStore values stay encrypted at rest (the store + key are unchanged from 1a).
- `DRAIN_EVERY` is a named constant, `Duration::from_secs(15)`.
- Per-task gate from `rust/`: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Flutter tasks additionally, from `flutter/`: `flutter analyze` (and `flutter test`). If the disk fills: `rm -rf rust/target/debug/incremental` and retry.
- Commit per task; trailer exactly: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Local commits only; do not push.
- Do not modify constitutional docs (ROADMAP.md, docs/VISION.md, docs/FUTURE_ARCHITECTURE.md, docs/ARCHITECTURAL_INVARIANTS.md). No new registry message id is added (1b adds no wire message).

---

## File Structure

- `rust/crates/peerbeam-chat/src/record.rs` — `ChatRecord::sent_pending`/status helper (build a record with an explicit status).
- `rust/crates/peerbeam-chat/src/store.rs` — outbox namespace + ops (`enqueue`, `outbox_pending`, `outbox_for`, `outbox_peers`, `outbox_remove`, `record_sent`) + `OutboxEntry`.
- `rust/crates/peerbeam-chat/src/send.rs` — `flush_to_session` (reuses the extracted open+wait); keep `send_message` for the direct path.
- `rust/crates/peerbeam-ffi/src/transfer.rs` — `Manager::chat_send` (enqueue+return+spawn flush), `chat_flush_peer`, `chat_outbox_peers`, flush-on-connect in `handle_incoming`.
- `rust/crates/peerbeam-ffi/src/events.rs` — `chat_status` emitter.
- `rust/crates/peerbeam-ffi/src/runtime.rs` — the drain loop, spawned in `init()`.
- `rust/bins/peerbeam-cli/src/chat.rs` + `commands.rs` — `chat send` enqueue+flush; drain tick in `serve_loop`/`watch`; flush-on-connect.
- `flutter/lib/sdk/{events.dart,models.dart}`, `data/chat_repository.dart`, `features/chat/chat_screen.dart` — `chat_status` event, `ChatMessage.copyWith`, status handler, bubble indicator.
- `docs/CLI.md` — offline-chat note.

---

## Task 1: `peerbeam-chat` — outbox store ops + Pending records

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/record.rs` (status-explicit constructor)
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (`OutboxEntry` + outbox ops)
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (re-export `OutboxEntry`)

**Interfaces:**
- Consumes: `ChatMessage {id,timestamp,body}`, `ChatRecord`, `Direction`, `Status` (Pending|Sent|Received), `AppStore`, `DeviceId`, `ChatError`.
- Produces:
  - `ChatRecord::out(peer, &ChatMessage, Status)` — build an outgoing record with an explicit status.
  - `OutboxEntry { peer_id: String, message_id: String, body: String, timestamp: String }` (+ `encode`/`decode`).
  - `ChatStore::enqueue(&DeviceId, &ChatMessage)` — persist an `Out`/`Pending` conversation record AND put an outbox entry.
  - `ChatStore::outbox_pending() -> Result<Vec<OutboxEntry>, ChatError>` (FIFO by key across all peers).
  - `ChatStore::outbox_for(&DeviceId) -> Result<Vec<OutboxEntry>, ChatError>` (one peer, FIFO).
  - `ChatStore::outbox_peers() -> Result<Vec<DeviceId>, ChatError>` (distinct peer ids with pending).
  - `ChatStore::outbox_remove(message_id: &str) -> Result<(), ChatError>`.
  - `ChatStore::record_sent(&OutboxEntry) -> Result<(), ChatError>` (upsert the conversation record to `Sent`).
  - `const OUTBOX_NS: &str = "chat-outbox"`.

- [ ] **Step 1: Write the failing tests** (append to `store.rs` `#[cfg(test)] mod tests`)

```rust
#[test]
fn enqueue_persists_pending_record_and_outbox_entry() {
    let (cs, _dir) = store();
    let peer = DeviceId::from("pb-bob");
    let m = ChatMessage::new("queued").unwrap();
    cs.enqueue(&peer, &m).unwrap();

    // conversation record is Pending/Out
    let hist = cs.history(&peer).unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].status, Status::Pending);
    assert_eq!(hist[0].direction, Direction::Out);
    assert_eq!(hist[0].id, m.id);

    // outbox has the entry
    let out = cs.outbox_for(&peer).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].message_id, m.id);
    assert_eq!(out[0].body, "queued");
    assert_eq!(out[0].peer_id, "pb-bob");
}

#[test]
fn outbox_pending_and_peers_and_fifo_order() {
    let (cs, _dir) = store();
    let a = DeviceId::from("pb-a");
    let b = DeviceId::from("pb-b");
    let m1 = ChatMessage::new("a1").unwrap();
    let m2 = ChatMessage::new("b1").unwrap();
    let m3 = ChatMessage::new("a2").unwrap();
    cs.enqueue(&a, &m1).unwrap();
    cs.enqueue(&b, &m2).unwrap();
    cs.enqueue(&a, &m3).unwrap();

    let all = cs.outbox_pending().unwrap();
    assert_eq!(all.len(), 3);
    // FIFO by key (message ids are time-ordered): m1, m2, m3
    assert_eq!(all[0].message_id, m1.id);
    assert_eq!(all[2].message_id, m3.id);

    let mut peers: Vec<String> = cs.outbox_peers().unwrap().into_iter().map(|d| d.0).collect();
    peers.sort();
    assert_eq!(peers, vec!["pb-a".to_string(), "pb-b".to_string()]);

    let a_only = cs.outbox_for(&a).unwrap();
    assert_eq!(a_only.len(), 2);
    assert_eq!(a_only[0].message_id, m1.id);
    assert_eq!(a_only[1].message_id, m3.id);
}

#[test]
fn record_sent_flips_pending_to_sent_in_place_and_remove_dequeues() {
    let (cs, _dir) = store();
    let peer = DeviceId::from("pb-bob");
    let m = ChatMessage::new("x").unwrap();
    cs.enqueue(&peer, &m).unwrap();
    let entry = cs.outbox_for(&peer).unwrap().remove(0);

    cs.record_sent(&entry).unwrap();
    cs.outbox_remove(&entry.message_id).unwrap();

    let hist = cs.history(&peer).unwrap();
    assert_eq!(hist.len(), 1); // same record, upserted (not a second row)
    assert_eq!(hist[0].id, m.id);
    assert_eq!(hist[0].status, Status::Sent);
    assert!(cs.outbox_for(&peer).unwrap().is_empty());
}
```

- [ ] **Step 2: Run to see them fail** — `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat outbox` → FAIL (missing items).

- [ ] **Step 3: Add the status-explicit record constructor** (`record.rs`, in `impl ChatRecord`)

```rust
    /// An outgoing record with an explicit status (used for the offline outbox:
    /// `Pending` on enqueue, `Sent` once flushed).
    #[must_use]
    pub fn out(peer: &DeviceId, msg: &ChatMessage, status: Status) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status,
        }
    }
```

- [ ] **Step 4: Add `OutboxEntry` + outbox ops** (`store.rs`)

```rust
use serde::{Deserialize, Serialize};

/// The AppStore namespace holding all undelivered outbound messages (across all
/// peers), keyed by message id (time-ordered), so `list` returns FIFO order.
pub const OUTBOX_NS: &str = "chat-outbox";

/// One queued outbound message awaiting delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub peer_id: String,
    pub message_id: String,
    pub body: String,
    pub timestamp: String,
}

impl OutboxEntry {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    fn decode(bytes: &[u8]) -> Result<OutboxEntry, ChatError> {
        serde_json::from_slice(bytes).map_err(|e| ChatError::Serialization(e.to_string()))
    }
}
```

Add these methods to `impl ChatStore`:

```rust
    /// Persist an outgoing message as Pending and enqueue it to the outbox.
    pub fn enqueue(&self, peer: &DeviceId, msg: &ChatMessage) -> Result<(), ChatError> {
        self.append(&ChatRecord::out(peer, msg, Status::Pending))?;
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: msg.id.clone(),
            body: msg.body.clone(),
            timestamp: msg.timestamp.clone(),
        };
        self.store
            .put(OUTBOX_NS, &msg.id, &entry.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// All queued entries, FIFO (ascending by message id).
    pub fn outbox_pending(&self) -> Result<Vec<OutboxEntry>, ChatError> {
        let raw = self
            .store
            .list(OUTBOX_NS)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        let mut out = Vec::with_capacity(raw.len());
        for (_key, value) in raw {
            out.push(OutboxEntry::decode(&value)?);
        }
        Ok(out)
    }

    /// Queued entries for one peer, FIFO.
    pub fn outbox_for(&self, peer: &DeviceId) -> Result<Vec<OutboxEntry>, ChatError> {
        Ok(self
            .outbox_pending()?
            .into_iter()
            .filter(|e| e.peer_id == peer.0)
            .collect())
    }

    /// Distinct peer ids that have queued messages.
    pub fn outbox_peers(&self) -> Result<Vec<DeviceId>, ChatError> {
        let mut seen = std::collections::BTreeSet::new();
        for e in self.outbox_pending()? {
            seen.insert(e.peer_id);
        }
        Ok(seen.into_iter().map(DeviceId::from).collect())
    }

    /// Remove a delivered entry from the outbox.
    pub fn outbox_remove(&self, message_id: &str) -> Result<(), ChatError> {
        self.store
            .delete(OUTBOX_NS, message_id)
            .map(|_| ())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// Upsert the conversation record for a delivered entry to `Sent`.
    pub fn record_sent(&self, entry: &OutboxEntry) -> Result<(), ChatError> {
        let rec = ChatRecord {
            id: entry.message_id.clone(),
            peer_id: entry.peer_id.clone(),
            direction: Direction::Out,
            timestamp: entry.timestamp.clone(),
            body: entry.body.clone(),
            status: Status::Sent,
        };
        self.append(&rec)
    }
```

(Confirm `AppStore::delete(namespace, key) -> Result<bool>` exists — it does, per the port. `outbox_remove` ignores the bool.)

- [ ] **Step 5: Re-export `OutboxEntry`** in `lib.rs`: add `OutboxEntry` (and `OUTBOX_NS` if useful) to the `pub use store::{...}` line.

- [ ] **Step 6: Run tests + full gate + commit**

`cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat` (PASS), then the full gate.

```bash
git add rust/crates/peerbeam-chat/src
git commit -m "feat(chat): outbox store ops + Pending records (1b substrate)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `peerbeam-chat` — `flush_to_session`

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/send.rs` (extract wire-send; add `flush_to_session`)
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (re-export `flush_to_session`)
- Test: `rust/crates/peerbeam-chat/tests/roundtrip.rs` (add a flush round-trip)

**Interfaces:**
- Consumes: `ChatStore::outbox_for/record_sent/outbox_remove` (Task 1); `SessionHandle::{open_channel,send_on_channel,channels}`; the private `wait_for_channel_open` in `send.rs`; `ChatMessage`, `OutboxEntry`.
- Produces: `pub async fn flush_to_session(handle: &SessionHandle, store: &ChatStore, peer: &DeviceId) -> Result<Vec<String>, SendError>` — opens the CHAT channel once, sends each of `peer`'s queued messages in FIFO order, and for each success upserts the record to `Sent` + removes the outbox entry; returns the message ids actually flushed. Stops that peer's flush on the first send error (remaining entries stay queued); a channel-open failure is `Err`.

- [ ] **Step 1: Write the failing flush round-trip test** (`tests/roundtrip.rs`)

Reuse the existing two-`PeerSession` harness in this file (the 1a `a_sends_b_receives_and_persists` test). Add:

```rust
#[tokio::test]
async fn flush_delivers_queued_messages_and_marks_sent() {
    // Build the same two-session harness as the 1a round-trip: A initiator with
    // a ChatStore `store_a`, B responder with a ChatHandler over `store_b`, peer
    // slot bound, both run loops spawned. (Copy that setup.)
    // ... harness setup identical to a_sends_b_receives_and_persists ...

    // Enqueue two messages for B while "offline" (before sending anything).
    let m1 = peerbeam_chat::ChatMessage::new("first").unwrap();
    let m2 = peerbeam_chat::ChatMessage::new("second").unwrap();
    store_a.enqueue(&b_id, &m1).unwrap();
    store_a.enqueue(&b_id, &m2).unwrap();
    assert_eq!(store_a.outbox_for(&b_id).unwrap().len(), 2);

    // Flush over the live session.
    let flushed = peerbeam_chat::flush_to_session(&a_handle, &store_a, &b_id).await.unwrap();
    assert_eq!(flushed.len(), 2);

    // Sender: outbox drained, records flipped to Sent.
    assert!(store_a.outbox_for(&b_id).unwrap().is_empty());
    let hist_a = store_a.history(&b_id).unwrap();
    assert!(hist_a.iter().all(|r| r.status == peerbeam_chat::Status::Sent));

    // Receiver: both delivered + persisted, in order (poll briefly).
    // ... poll store_b.history(&a_id) until len==2 (bounded ~2s) ...
    let hist_b = /* store_b.history(&a_id) */;
    assert_eq!(hist_b.len(), 2);
    assert_eq!(hist_b[0].body, "first");
    assert_eq!(hist_b[1].body, "second");
}

#[tokio::test]
async fn reflush_is_idempotent_on_the_receiver() {
    // Enqueue one, flush, then RE-enqueue the same message id and flush again;
    // the receiver must still have exactly one record (dedup by id).
    // (Build the same harness; after the first flush, re-put the same OutboxEntry
    //  via store_a.enqueue with a ChatMessage carrying the SAME id — construct a
    //  ChatMessage { id: m.id.clone(), timestamp: m.timestamp.clone(), body: m.body.clone() }
    //  — flush again, assert store_b.history(&a_id).len()==1.)
}
```

Complete the harness by copying the 1a round-trip setup verbatim (identities, MemTransport, PeerSession::open, handler registration + peer-slot bind, run-loop spawns). Poll for receiver state with a bounded loop, not a fixed sleep.

- [ ] **Step 2: Run to see it fail** — `cargo test -p peerbeam-chat --test roundtrip flush` → FAIL (`flush_to_session` missing).

- [ ] **Step 3: Extract the wire-send + implement `flush_to_session`** (`send.rs`)

Add a private helper that sends one `ChatMessage` over an already-open channel (factored from `send_message`'s middle):

```rust
async fn send_on_open_channel(
    handle: &SessionHandle,
    channel: peerbeam_domain::session::ChannelId,
    msg: &ChatMessage,
) -> Result<(), SendError> {
    let frame = msg.to_frame(channel)?;
    handle
        .send_on_channel(channel, ChatMessage::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()))
}
```

Refactor `send_message` to reuse it (open → wait → `send_on_open_channel(handle, channel, &msg)` → persist) — behavior unchanged. Then add:

```rust
/// Flush all of `peer`'s queued outbox messages over an established session.
/// Opens the CHAT channel once, sends each queued message in FIFO order, and on
/// each success upserts the conversation record to `Sent` and removes the outbox
/// entry. Returns the message ids flushed. A per-message send error stops this
/// peer's flush (the rest stay queued); a channel-open failure returns `Err`.
pub async fn flush_to_session(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
) -> Result<Vec<String>, SendError> {
    let entries = store.outbox_for(peer)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;

    let mut flushed = Vec::new();
    for entry in entries {
        let msg = ChatMessage {
            id: entry.message_id.clone(),
            timestamp: entry.timestamp.clone(),
            body: entry.body.clone(),
        };
        if send_on_open_channel(handle, channel, &msg).await.is_err() {
            break; // peer went away mid-flush; remaining entries stay queued
        }
        store.record_sent(&entry)?;
        store.outbox_remove(&entry.message_id)?;
        flushed.push(entry.message_id);
    }
    Ok(flushed)
}
```

(`ChatMessage` fields are `pub` within the crate, so the struct literal compiles in `send.rs`. `ChannelType`, `DeviceId`, `ChatStore` imports: add as needed.)

- [ ] **Step 4: Re-export** `flush_to_session` in `lib.rs` (`pub use send::{send_message, flush_to_session, SendError};`).

- [ ] **Step 5: Run tests + full gate + commit**

```bash
git add rust/crates/peerbeam-chat
git commit -m "feat(chat): flush_to_session — deliver queued outbox over a session

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: FFI — enqueue-and-return send + per-peer flush + flush-on-connect + chat_status

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`chat_send`, `chat_flush_peer`, `chat_outbox_peers`, `handle_incoming`)
- Modify: `rust/crates/peerbeam-ffi/src/events.rs` (`chat_status`)

**Interfaces:**
- Consumes: `ChatStore::{enqueue,outbox_peers}` + `flush_to_session` (Tasks 1-2); `session_exec::dial/accept`; `self.chat`, `self.session(...)`, `device_from`.
- Produces: `Manager::chat_send(self: &Arc<Self>, req) -> Op` (enqueue + return + spawn flush); `Manager::chat_flush_peer(&self, device: Device) -> Vec<String>` (dial → `flush_to_session` → close → emit `chat_status` per flushed id; returns flushed ids); `Manager::chat_outbox_peers(&self) -> Vec<DeviceId>`; `events::chat_status(peer_id, message_id, status)`.

- [ ] **Step 1: Add `events::chat_status`** (`events.rs`, mirroring `events::chat`)

```rust
/// Emit a `chat_status` event when a queued message's delivery status changes.
pub fn chat_status(peer_id: &str, message_id: &str, status: &str) {
    emit(&json!({
        "type": "chat_status",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message_id": message_id,
        "peer_id": peer_id,
        "status": status,
    }));
}
```

- [ ] **Step 2: Rework `Manager::chat_send` → enqueue + return + spawn flush** (`transfer.rs`)

Change the receiver to `self: &Arc<Self>` (like `send`). New body:

```rust
pub fn chat_send(self: &Arc<Self>, req: &Value) -> Op {
    let device = device_from(req.get("peer"))?;
    let text = req
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or((Code::InvalidArgument, "text required".into()))?;
    let msg = peerbeam_chat::ChatMessage::new(text).map_err(|e| (Code::InvalidArgument, e.to_string()))?;
    // Persist Pending + enqueue immediately; return without waiting on the network.
    self.chat
        .enqueue(&device.id, &msg)
        .map_err(|e| (Code::Internal, e.to_string()))?;
    let id = msg.id.clone();
    // Opportunistic delivery in the background (best-effort; drain covers the rest).
    let me = self.clone();
    crate::runtime::spawn(async move {
        let _ = me.chat_flush_peer(device).await;
    });
    Ok(json!({ "id": id }))
}
```

- [ ] **Step 3: Add `chat_flush_peer` + `chat_outbox_peers`** (`transfer.rs`)

```rust
/// Distinct peers that currently have queued messages.
pub fn chat_outbox_peers(&self) -> Vec<DeviceId> {
    self.chat.outbox_peers().unwrap_or_default()
}

/// Dial `device` and flush its queued messages; emits `chat_status` for each
/// delivered message. Best-effort: an unreachable peer leaves its queue intact.
pub async fn chat_flush_peer(&self, device: Device) -> Vec<String> {
    let meta = self.session(&format!("chat-{}", device.id.0), device.id.clone(), 0);
    let session = match crate::session_exec::dial(
        &self.quic, &self.rm, &device, &meta,
        self.identity(), self.enc.clone(), self.trust.clone(), None,
    ).await {
        Ok(s) => s,
        Err(_) => return Vec::new(), // unreachable; stays queued
    };
    let peer = session.peer_device.clone();
    let flushed = peerbeam_chat::flush_to_session(&session.handle, &self.chat, &peer)
        .await
        .unwrap_or_default();
    session.close().await;
    for mid in &flushed {
        crate::events::chat_status(&peer.0, mid, "sent");
    }
    flushed
}
```

- [ ] **Step 4: Flush-on-connect in `handle_incoming`** (`transfer.rs`)

Right after `session_exec::accept(...)` succeeds and `session.peer_device` is known (before the transfer-accept gate), opportunistically flush any of that peer's queued messages over the just-established session:

```rust
    // Flush-on-connect: deliver anything queued for this peer over the session
    // we just accepted (cheaper + faster than waiting for the next drain tick).
    {
        let flushed = peerbeam_chat::flush_to_session(&session.handle, &self.chat, &session.peer_device)
            .await
            .unwrap_or_default();
        for mid in &flushed {
            crate::events::chat_status(&session.peer_device.0, mid, "sent");
        }
    }
```

(`session.handle` is a `SessionHandle`; confirm it's accessible pre-`next_incoming`. This flush is outbound over the responder session — the peer's own receive path dispatches our CHAT frames to its handler.)

- [ ] **Step 5: Verify `pb_chat_send` still compiles** — `pb_chat_send` calls `runtime::manager()?.chat_send(&read_json(json)?)`; with `chat_send(self: &Arc<Self>, ...)`, `runtime::manager()` returns `Arc<Manager>` and the call auto-refs (same as `pb_transfer_send` → `send`). No change needed in `lib.rs`.

- [ ] **Step 6: Full gate + commit**

Run the gate. (The 1a `chat_ffi` tests still exercise `chat_send`; they may need a tweak since `chat_send` no longer blocks until delivery — if a test asserted immediate delivery via `chat_history`, add a bounded poll, since delivery is now via the spawned flush. Adjust the test to poll, do not weaken the assertion.)

```bash
git add rust/crates/peerbeam-ffi/src/transfer.rs rust/crates/peerbeam-ffi/src/events.rs
git commit -m "feat(ffi): chat_send enqueues + opportunistic/per-peer flush + flush-on-connect + chat_status

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: FFI — background drain loop

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/runtime.rs` (drain loop + spawn in `init`)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` (offline→online delivery e2e)

**Interfaces:**
- Consumes: `Manager::{chat_outbox_peers, chat_flush_peer}` (Task 3); `Engine::devices() -> Vec<ManagedDevice>`; `ManagedDevice.device` (`Device{id,addresses,port,...}`).
- Produces: `const DRAIN_EVERY: Duration`; `async fn chat_drain_loop(engine: Arc<Engine>, manager: Arc<Manager>)`, spawned in `init()`.

- [ ] **Step 1: Add the drain loop** (`runtime.rs`)

```rust
const DRAIN_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// Periodically deliver queued chat messages to peers that are now reachable.
/// Lives here so it can hold the engine (for `devices()`) and the manager (for
/// dial+flush) directly. Keep-forever: an unreachable peer's queue is retried
/// every tick, never dropped.
async fn chat_drain_loop(engine: Arc<Engine>, manager: Arc<Manager>) {
    let mut ticker = tokio::time::interval(DRAIN_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let peers = manager.chat_outbox_peers();
        if peers.is_empty() {
            continue;
        }
        let online = engine.devices();
        for peer in peers {
            // Resolve the peer's current address from discovery; skip if offline.
            let Some(md) = online.iter().find(|m| m.device.id == peer && m.online) else {
                continue;
            };
            if md.device.addresses.is_empty() || md.device.port == 0 {
                continue;
            }
            let _ = manager.chat_flush_peer(md.device.clone()).await;
        }
    }
}
```

- [ ] **Step 2: Spawn it in `init()`** — after the `manager` and `engine` locals are built (before/after they're stored in statics), capture clones and spawn:

```rust
    rt().spawn(chat_drain_loop(engine.clone(), manager.clone()));
```

(`engine`/`manager` are `Arc<Engine>`/`Arc<Manager>` locals in `init`; clone before the `*lock(&ENGINE) = Some(engine);` moves.)

- [ ] **Step 3: Write the offline→online e2e test** (`chat_ffi.rs`)

Mirror the 1a `chat_ffi` harness (FFI engine + a real QUIC peer). Test shape:

```rust
// 1. FFI engine initialised. Enqueue a message to a peer that is NOT reachable
//    yet (chat_send with the peer's real addresses+port but nothing listening),
//    assert pb_chat_history shows it Pending and it's in the outbox.
// 2. Bring up a real peer listener at that address (mirror the 1a receiver peer),
//    ensure discovery sees it (or invoke the flush path directly if discovery is
//    not wired in the test harness — see note).
// 3. Wait (bounded) for delivery: assert the peer received the message AND a
//    chat_status {message_id, status:"sent"} event fired AND pb_chat_history now
//    shows it Sent AND the outbox is empty.
```

Note: if driving the periodic drain via discovery is impractical in the test harness (discovery timing), test the drain's unit of work instead by calling `Manager::chat_flush_peer(peer_device)` directly against a live peer listener (the drain loop is a thin scheduler over it), plus a separate assertion that `chat_send` enqueues Pending without blocking. Do NOT skip the delivery assertion — prove flush marks Sent + emits chat_status + empties the outbox.

- [ ] **Step 4: Run the test + full gate + commit**

```bash
git add rust/crates/peerbeam-ffi
git commit -m "feat(ffi): background chat drain loop (offline delivery)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: CLI — enqueue send + drain tick in serve/watch + flush-on-connect

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/chat.rs` (`send` enqueue+flush; drain in `watch`)
- Modify: `rust/bins/peerbeam-cli/src/commands.rs` (drain in `serve_loop`; flush-on-connect)

**Interfaces:**
- Consumes: `ChatStore::{enqueue,outbox_peers}` + `flush_to_session` (Tasks 1-2); `session_transfer::dial/accept`; `commands::{snapshot, chat_store, build_engine}`; `engine.devices()`.
- Produces: `chat send` enqueues then opportunistically flushes; `serve_loop`/`watch` run a periodic drain tick + flush-on-connect.

- [ ] **Step 1: `chat send` → enqueue then opportunistic flush** (`chat.rs::send`)

Replace the dial-then-send body: build a `ChatMessage`, `store.enqueue(&target.id, &msg)`, print/return the id immediately, then attempt one opportunistic dial+`flush_to_session` (non-fatal on failure — an offline peer stays queued):

```rust
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let msg = peerbeam_chat::ChatMessage::new(&text).map_err(|e| CliError::Usage(e.to_string()))?;
    store.enqueue(&target.id, &msg).map_err(CliError::from)?;
    let id = msg.id.clone();

    // Opportunistic immediate delivery; if the peer is unreachable it stays queued
    // for a running host (daemon / chat watch) to drain later.
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let delivered = match session_transfer::dial(
        &quic, &routes, &target, "chat", &sc.ident, &sc.enc, &sc.trust, None,
    ).await {
        Ok(session) => {
            let peer = DeviceId::from(session.peer_id.clone());
            let flushed = peerbeam_chat::flush_to_session(&session.handle, &store, &peer)
                .await
                .unwrap_or_default();
            session.close().await;
            !flushed.is_empty()
        }
        Err(_) => false,
    };

    if ctx.json {
        ctx.json_line(&json!({ "event": "chat_sent", "id": id, "peer": target.id.0, "delivered": delivered }));
    } else if delivered {
        ctx.line(&ctx.green(&format!("sent to {}", target.name)));
    } else {
        ctx.line(&ctx.dim(&format!("queued for {} (offline — a running daemon/watch will deliver)", target.name)));
    }
    Ok(())
```

(`target` resolution stays as-is; note `target.id` for `--addr` may be a placeholder `"addr"` — acceptable, the record keys under it and flush uses the authenticated `session.peer_id`. Keep the existing name-resolution path for `--to`.)

- [ ] **Step 2: Drain tick + flush-on-connect in `serve_loop`** (`commands.rs`)

`serve_loop` already builds `engine` (discovery) + `sc`. Build a `chat_store` once, and restructure the accept `loop` into a `tokio::select!` with a `DRAIN_EVERY` interval arm. On the accept arm, after establishing the session, flush that peer's outbox (flush-on-connect). On the interval arm, run the same drain as the FFI (resolve peers via `engine.devices()`, dial reachable, flush). Define a local `chat_drain_tick(engine, store, quic, routes, sc)` helper in `commands.rs` (or `chat.rs`) shared by `serve_loop` and `watch`:

```rust
// pub(crate) in chat.rs — reused by serve_loop and watch.
pub(crate) async fn drain_tick(
    engine: &peerbeam_engine::Engine,
    store: &peerbeam_chat::ChatStore,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    sc: &SecureCtx,
) {
    let peers = store.outbox_peers().unwrap_or_default();
    let online = engine.devices();
    for peer in peers {
        let Some(md) = online.iter().find(|m| m.device.id == peer && m.online) else { continue };
        if md.device.addresses.is_empty() || md.device.port == 0 { continue; }
        if let Ok(session) = session_transfer::dial(
            quic, routes, &md.device, "chat", &sc.ident, &sc.enc, &sc.trust, None,
        ).await {
            let p = DeviceId::from(session.peer_id.clone());
            let _ = peerbeam_chat::flush_to_session(&session.handle, store, &p).await;
            session.close().await;
        }
    }
}
```

Restructure `serve_loop`'s `loop { let qc = incoming.next().await ... }` to:

```rust
    let chat = chat_store(config, &sc.enc, &sc.ident);
    let mut drain = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = drain.tick() => {
                crate::chat::drain_tick(engine_ref, &chat, &quic, &routes_for_drain, &sc).await;
            }
            item = incoming.next() => {
                let qc = match item { Some(Ok(c)) => c, Some(Err(_)) => continue, None => break };
                // ... existing accept + pairing gate + receive ...
                // flush-on-connect: after `session` is established and peer known,
                // let _ = peerbeam_chat::flush_to_session(&session.handle, &chat, &DeviceId::from(session.peer_id.clone())).await;
            }
        }
        if once { break; }
    }
```

Notes for the implementer: `serve_loop` builds `engine` via `build_engine` (an `Option`) — the drain arm needs a live `Engine` ref for `devices()`; use the same `engine` that `serve_loop` starts discovery on. `serve_loop` doesn't currently hold a `RouteManager`/`QuicTransport` for dialing (it only serves) — construct a `RouteManager::new(quic.clone())` for the drain (a dial endpoint), reusing the `quic` it already made, or a second `QuicTransport` if the serving one can't also dial (verify; the CLI `send` path makes its own `QuicTransport` for dialing — mirror that). Keep `if once { break; }` semantics: with `select!`, only break on the accept arm's terminal outcomes as today (a drain tick must not end a `--once` receive; guard the `once` break to the accept arm).

- [ ] **Step 3: Same drain tick in `chat watch`** (`chat.rs::watch`)

Restructure `watch`'s `loop { incoming.next().await }` into the same `tokio::select!` (accept arm + `drain.tick()` arm calling `drain_tick`), and add flush-on-connect after `accept` in the accept arm. `watch` already builds `engine` + `store` + `quic`.

- [ ] **Step 4: Test** — add a CLI unit test where practical (e.g. `chat send --addr <unreachable>` enqueues: assert the message is Pending in `chat history` and present in the outbox without the command erroring). The full offline→online delivery is covered by the chat-crate + FFI e2e tests; a live CLI two-process drain test is optional. Run: `cargo test -p peerbeam-cli chat`.

- [ ] **Step 5: Full gate + commit**

```bash
git add rust/bins/peerbeam-cli/src
git commit -m "feat(cli): chat send enqueues; drain loop + flush-on-connect in serve/watch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Flutter — chat_status event + status indicator

**Files:**
- Modify: `flutter/lib/sdk/events.dart` (`ChatStatus` + `chat_status` case)
- Modify: `flutter/lib/sdk/models.dart` (`ChatMessage.copyWith`)
- Modify: `flutter/lib/data/chat_repository.dart` (handle `ChatStatus`)
- Modify: `flutter/lib/features/chat/chat_screen.dart` (`_ChatBubble` status indicator)
- Test: `flutter/test/data/repository_test.dart` + `flutter/test/sdk/chat_test.dart`

**Interfaces:**
- Consumes: `ChatMessage {id, peerId, direction, body, at, status}`.
- Produces: `ChatStatus extends BridgeEvent { messageId, peerId, status }`; `ChatMessage.copyWith`; a repository handler that flips a message's status in place.

- [ ] **Step 1: `ChatStatus` event** (`events.dart`) — add the case after `chat_received` and the class after `ChatReceived`:

```dart
      case 'chat_status':
        return ChatStatus(
          messageId: j['message_id'] as String? ?? '',
          peerId: j['peer_id'] as String? ?? '',
          status: j['status'] as String? ?? '',
        );
```
```dart
/// A delivery-status change for a previously-sent chat message.
class ChatStatus extends BridgeEvent {
  final String messageId;
  final String peerId;
  final String status;
  const ChatStatus({required this.messageId, required this.peerId, required this.status});
}
```

- [ ] **Step 2: `ChatMessage.copyWith`** (`models.dart`, in `ChatMessage`)

```dart
  ChatMessage copyWith({String? status}) => ChatMessage(
        id: id,
        peerId: peerId,
        direction: direction,
        body: body,
        at: at,
        status: status ?? this.status,
      );
```

- [ ] **Step 3: Handle `ChatStatus` in the repository** (`chat_repository.dart`)

Extend the events listener and add a handler:

```dart
    _sub = _api?.events.listen((e) {
      if (e is ChatReceived) _onReceived(e.message);
      if (e is ChatStatus) _onStatus(e);
    });
```
```dart
  void _onStatus(ChatStatus e) {
    final list = _byPeer[e.peerId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.id == e.messageId);
    if (i < 0) return;
    list[i] = list[i].copyWith(status: e.status);
    notifyListeners();
  }
```

- [ ] **Step 4: Bubble status indicator** (`chat_screen.dart::_ChatBubble`)

In the timestamp `Column`, replace the bare timestamp `Text` with a `Row` that, for outgoing messages (`mine`), appends a trailing glyph: `status == 'pending'` → `Icons.schedule` (clock), else (`'sent'`) → `Icons.check_rounded`, sized `~14`, colored `fg.withValues(alpha: 0.7)`:

```dart
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(_time(message.at),
                          style: text.labelSmall?.copyWith(color: fg.withValues(alpha: 0.7))),
                      if (mine) ...[
                        const Gap(AppSpace.xxs),
                        Icon(
                          message.status == 'pending' ? Icons.schedule : Icons.check_rounded,
                          size: 14,
                          color: fg.withValues(alpha: 0.7),
                        ),
                      ],
                    ],
                  ),
```

- [ ] **Step 5: Tests**

- `chat_test.dart`: `ChatStatus` parses from a `{type:'chat_status', message_id, peer_id, status}` JSON; `ChatMessage.copyWith(status:'sent')` changes only status.
- `repository_test.dart`: seed a conversation (via `chat_received` or `refresh`), fire a `ChatStatus` for a message id, assert `messagesFor(peerId)` shows that message's status flipped to `sent`.

- [ ] **Step 6: analyze + test + commit**

`cd /home/althaf-ahammed/Projects/omega/PeerBeam/flutter && flutter analyze && flutter test`

```bash
git add flutter/lib flutter/test
git commit -m "feat(flutter): chat_status event + Pending/Sent bubble indicator

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Docs

**Files:**
- Modify: `docs/CLI.md` (offline-chat note)

- [ ] **Step 1:** Update the `chat` section in `docs/CLI.md`: `chat send` now **queues** a message if the peer is offline and returns immediately; delivery happens when a running host (the app, or `peerbeam daemon` / `peerbeam chat watch`) next reaches the peer. Note: queued messages persist encrypted locally and are retried indefinitely (no expiry); `Sent` means handed to a live session (not a read receipt). Add `--port` to the documented `chat watch` synopsis while here (the deferred 1a doc nit).

- [ ] **Step 2:** Gate (`cd rust && cargo fmt --check`) + commit:

```bash
git add docs/CLI.md
git commit -m "docs: offline chat (queue/drain) + chat watch --port (increment 1b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Outbox namespace + entries + FIFO → Task 1. ✓
- Enqueue-Pending-then-opportunistic-flush send (fixes 1a blocking) → Task 3 (FFI), Task 5 (CLI). ✓
- `flush_to_session` (reuses wire-send, marks Sent, dequeues, stops on error) → Task 2. ✓
- Drain loop resolving peers via discovery + dialing reachable ones → Task 4 (FFI runtime), Task 5 (CLI serve/watch). ✓
- Flush-on-connect → Task 3 (FFI handle_incoming), Task 5 (CLI accept). ✓
- Keep-forever (no cap) → no attempt/TTL logic anywhere; drain retries every tick. ✓
- Receiver dedup makes re-flush idempotent → reuses shipped 1a handler; Task 2 test asserts it. ✓
- `chat_status` Pending→Sent event + Flutter bubble indicator → Task 3 (emit), Task 6 (event + copyWith + repo + bubble). ✓
- No wire change (reuse CHAT + Message=1) → `flush_to_session` sends the same `Message` frame; no new ids. ✓
- Docs → Task 7. ✓

**Type consistency:** `OutboxEntry {peer_id,message_id,body,timestamp}`; `enqueue(&DeviceId,&ChatMessage)`; `outbox_for/outbox_pending -> Vec<OutboxEntry>`; `outbox_peers -> Vec<DeviceId>`; `record_sent(&OutboxEntry)`; `flush_to_session(&SessionHandle,&ChatStore,&DeviceId) -> Result<Vec<String>,SendError>`; `chat_flush_peer(&self, Device) -> Vec<String>`; `chat_outbox_peers(&self) -> Vec<DeviceId>`; `events::chat_status(peer_id,message_id,status)`; Dart `ChatStatus{messageId,peerId,status}` + `'chat_status'` JSON `{message_id,peer_id,status}`. Consistent across Rust emit ↔ Dart parse. ✓

**Integration risks flagged for implementers (each with a concrete resolution):**
- The drain must reach both the engine (`devices()`, private accessor) and the manager (dial+flush) — resolved by hosting the loop in `runtime.rs` and capturing the `engine`/`manager` Arcs in `init()` (not via the private `engine()` fn).
- CLI `serve_loop` lacks a dial endpoint — construct a `QuicTransport`+`RouteManager` for the drain (mirror `chat send`), and guard the `--once` break to the accept arm so a drain tick can't end a one-shot receive.
- 1a `chat_ffi` tests assumed synchronous delivery from `chat_send`; with enqueue-and-return, delivery is via the spawned flush — the implementer must convert those assertions to bounded polls (Task 3 step 6), not weaken them.

**Placeholder scan:** none — every step has concrete code or an exact command; the round-trip/e2e tests reuse the named 1a harnesses verbatim.
