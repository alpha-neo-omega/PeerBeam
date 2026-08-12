# File-in-Chat — Increment 2a (Online) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attach a file inside a chat thread: a small `FileRef` message on the CHAT channel places the row in the conversation and binds it — by a single shared id — to a normal transfer that carries the bytes over the existing TRANSFER stream channel.

**Architecture:** Control-plane correlation, not a new transport. The sender mints one id, uses it as both the `FileRef` message id and the transfer id, and sends the `FileRef` only when the peer negotiated `CHAT_FEAT_FILEREF`. The receiver — which since increment 0 holds the stream channel before registering — peeks the transfer meta, and registers the transfer under the *sender's* id with the real name and size, so the approval prompt is informative and the chat row and the transfer are the same thing on both ends.

**Tech Stack:** Rust (workspace at `rust/`), existing `peerbeam-chat` / `peerbeam-transfer` / `peerbeam-ffi` crates, Dart/Flutter FFI + Material 3.

## Global Constraints

- **Scope is 2a (online) only.** Attaching to an unreachable peer FAILS with a clear message. No outbox/queue work for files (that is 2b): no Android cache staging, no source-changed detection, no separate file queue, no declined-terminal signalling, no drain integration.
- **File bytes stay on the existing TRANSFER stream channel.** No streaming on CHAT (invariant I2).
- `CHAT_FEAT_FILEREF: u32 = 1 << 0`. Chat `MessageType FILE_REF = 2`. `FileRef` frames are sent with `MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE)` (MESSAGE_REGISTRY.md §7: ship additive types as OPTIONAL).
- **Every schema/wire addition must be additive and backward-compatible.** A 1a/1b-era `ChatRecord` JSON (no `kind`, no `file`) must still decode as `Text` — test it. A peer advertising `features: 0` must never be sent a `FileRef`.
- **The wire `FileRef` must NEVER carry `local_path`.** Separate types (`FileRef` on the wire, `FileMeta` in the record) plus a test asserting the encoded frame's JSON has no `local_path` key.
- `FileRef.name` ≤ 255 bytes AND must round-trip `Path::file_name()` (rejects `../`, separators, empty).
- **The I6 approval gate's decision logic is unchanged** — the `pending` map, `accept`/`accept_trust`/`reject`, `AcceptDecision`, `auto_accept_trusted`, trust-only-on-explicit-trust. It only gains a real name/size and a correlated id.
- No `unwrap`/`expect`/`panic!`/`unsafe` in **library** (crate) code. The CLI **binary** may use `expect` only where existing style does. Tests may `unwrap`.
- Per-task gate from `rust/`: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Flutter tasks additionally, from `flutter/`: `flutter analyze && flutter test`.
- **Regression nets that must pass unchanged:** `peerbeam-ffi` `tests/transfer_ffi.rs`, `peerbeam-cli` `tests/transfer_e2e.rs`, `peerbeam-ffi` `tests/chat_ffi.rs`, `peerbeam-chat` `tests/roundtrip.rs`.
- If the disk fills: `rm -rf rust/target/debug/incremental` and retry (this machine runs tight on space).
- **Test-environment note:** `chat_ffi::chat_drain_delivers_queued_message_once_peer_comes_online` flakes only when another PeerBeam process holds UDP 49500; check `pgrep -af peerbeam` before blaming a diff. Never run `pkill -f peerbeam` (it self-matches and kills the shell) — use exact PIDs.
- Commit per task; trailer exactly: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Local commits only; do not push.
- Do not modify constitutional docs (ROADMAP.md, docs/VISION.md, docs/FUTURE_ARCHITECTURE.md, docs/ARCHITECTURAL_INVARIANTS.md). MESSAGE_REGISTRY.md is derived and editable.

---

## File Structure

- `rust/crates/peerbeam-domain/src/session/negotiation.rs` — `CHAT_FEAT_FILEREF` (first feature-bit constant in the codebase).
- `rust/crates/peerbeam-chat/src/message.rs` — `FileRef` wire type + `MSG_FILE_REF` + `pub fn mint_id()`.
- `rust/crates/peerbeam-chat/src/record.rs` — `Kind`, `FileMeta`, additive `ChatRecord` fields, `Status` variants.
- `rust/crates/peerbeam-chat/src/store.rs` — `set_status`, `reconcile_on_load`, outbox field preservation.
- `rust/crates/peerbeam-chat/src/handler.rs` — a `FILE_REF` dispatch arm.
- `rust/crates/peerbeam-chat/src/send.rs` — `prepare_file_send`, `send_file_ref`.
- `rust/crates/peerbeam-transfer/src/{stream.rs,folder.rs}` — `transfer_id` on `Received`/`FolderReceived`.
- `rust/crates/peerbeam-ffi/src/{session_exec.rs,transfer.rs,events.rs,lib.rs}` — negotiated caps on `Session`, caller-supplied id, meta peek, `peer_id` on events, `pb_chat_send_file`.
- `rust/bins/peerbeam-cli/src/{cli.rs,chat.rs}` — `chat send --file`, file rows.
- `flutter/lib/…` — attach button, file bubble, inline approval, offline-reachable thread.
- `docs/MESSAGE_REGISTRY.md`, `docs/CLI.md`.

---

## Task 1: `FileRef` wire type, `FileMeta`, and the additive record schema

**Files:**
- Modify: `rust/crates/peerbeam-domain/src/session/negotiation.rs` (add `CHAT_FEAT_FILEREF`)
- Modify: `rust/crates/peerbeam-chat/src/message.rs` (`MSG_FILE_REF`, `FileRef`, make `mint_id` public)
- Modify: `rust/crates/peerbeam-chat/src/record.rs` (`Kind`, `FileMeta`, additive `ChatRecord` fields, `Status` variants)
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (re-exports)

**Interfaces:**
- Produces:
  - `peerbeam_domain::session::CHAT_FEAT_FILEREF: u32 = 1 << 0`
  - `peerbeam_chat::MSG_FILE_REF: u16 = 2`
  - `peerbeam_chat::FileRef { id: String, timestamp: String, name: String, size: u64 }` with `FileRef::new(name, size) -> Result<FileRef, ChatError>`, `FileRef::message_type() -> MessageType`, `to_frame(ChannelId) -> Result<SessionFrame, ChatError>`, `from_frame(&SessionFrame) -> Result<FileRef, ChatError>`
  - `peerbeam_chat::mint_id() -> String` (was private)
  - `peerbeam_chat::{Kind, FileMeta}`; `ChatRecord.kind: Kind`, `ChatRecord.file: Option<FileMeta>`
  - `Status::{PendingApproval, Transferring, Declined, Failed, Interrupted}` alongside `Pending|Sent|Received`
  - `ChatError::BadName(String)`

- [ ] **Step 1: Write the failing tests** (in `message.rs`'s `mod tests` and `record.rs`'s `mod tests`)

```rust
// message.rs tests
#[test]
fn file_ref_roundtrips_through_a_frame() {
    let r = FileRef::new("report.pdf", 4096).unwrap();
    assert_eq!(r.name, "report.pdf");
    assert_eq!(r.size, 4096);
    assert_eq!(r.id.len(), 13 + 16);
    let frame = r.to_frame(ChannelId::new(3)).unwrap();
    assert_eq!(frame.message_type.get(), MSG_FILE_REF);
    assert!(frame.flags.is_optional(), "additive types ship OPTIONAL (registry §7)");
    assert!(frame.flags.contains(MessageFlags::END_OF_MESSAGE));
    assert_eq!(FileRef::from_frame(&frame).unwrap(), r);
}

/// The wire type must never leak the sender's local path. `FileMeta` (record
/// side) holds it; `FileRef` (wire side) must not even have the field.
#[test]
fn file_ref_frame_never_contains_a_local_path() {
    let r = FileRef::new("report.pdf", 1).unwrap();
    let frame = r.to_frame(ChannelId::new(1)).unwrap();
    let json = String::from_utf8(frame.payload.to_vec()).unwrap();
    assert!(!json.contains("local_path"), "wire frame leaked a local path: {json}");
}

#[test]
fn file_ref_rejects_unsafe_or_oversized_names() {
    for bad in ["../escape", "/etc/passwd", "a/b", "", "."] {
        assert!(matches!(FileRef::new(bad, 1), Err(ChatError::BadName(_))), "accepted {bad:?}");
    }
    let long = "x".repeat(256);
    assert!(matches!(FileRef::new(&long, 1), Err(ChatError::BadName(_))));
    assert!(FileRef::new(&"x".repeat(255), 1).is_ok());
}

/// A hostile peer's frame must be rejected on decode too, not just on send.
#[test]
fn from_frame_rejects_a_hostile_name() {
    let good = FileRef::new("ok.txt", 1).unwrap();
    let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
    let hostile = r#"{"id":"x","timestamp":"t","name":"../escape","size":1}"#;
    frame.payload = bytes::Bytes::from_static(hostile.as_bytes());
    assert!(matches!(FileRef::from_frame(&frame), Err(ChatError::BadName(_))));
}

#[test]
fn from_frame_rejects_the_wrong_message_type() {
    let text = ChatMessage::new("hi").unwrap().to_frame(ChannelId::new(1)).unwrap();
    assert!(matches!(FileRef::from_frame(&text), Err(ChatError::WrongType(_))));
}
```

```rust
// record.rs tests
/// A record persisted by 1a/1b (no `kind`, no `file`) must still decode.
#[test]
fn legacy_record_json_decodes_as_text() {
    let legacy = br#"{"id":"1","peer_id":"pb-a","direction":"out",
        "timestamp":"t","body":"hello","status":"sent"}"#;
    let rec = ChatRecord::decode(legacy).unwrap();
    assert_eq!(rec.kind, Kind::Text);
    assert!(rec.file.is_none());
    assert_eq!(rec.body, "hello");
}

#[test]
fn file_record_carries_its_meta_and_roundtrips() {
    let peer = DeviceId::from("pb-bob");
    let r = FileRef::new("a.bin", 7).unwrap();
    let meta = FileMeta { name: r.name.clone(), size: r.size, local_path: Some("/tmp/a.bin".into()) };
    let rec = ChatRecord::file_out(&peer, &r, meta.clone(), Status::Transferring);
    assert_eq!(rec.kind, Kind::File);
    assert_eq!(rec.direction, Direction::Out);
    assert_eq!(rec.id, r.id);
    let back = ChatRecord::decode(&rec.encode()).unwrap();
    assert_eq!(back, rec);
    assert_eq!(back.file.unwrap().local_path.as_deref(), Some("/tmp/a.bin"));
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat file_ref`
Expected: FAIL to compile — `FileRef`, `Kind`, `FileMeta` undefined.

- [ ] **Step 3: Add the feature bit** (`peerbeam-domain/src/session/negotiation.rs`, near `Capability`)

```rust
/// Feature bit on the CHAT capability: this peer understands the `FileRef`
/// message (chat MessageType 2) and can correlate it with a transfer.
///
/// `Capability.features` is already on the wire and `CapabilitySet::intersect`
/// already ANDs the bits, so advertising this is not a wire change: a peer from
/// before this feature advertises `features: 0`, the intersection clears the
/// bit, and a sender simply never offers it a `FileRef`.
pub const CHAT_FEAT_FILEREF: u32 = 1 << 0;
```

Re-export it from the session module's `pub use` list so `peerbeam_domain::session::CHAT_FEAT_FILEREF` resolves.

- [ ] **Step 4: Add `MSG_FILE_REF`, `FileRef`, and make `mint_id` public** (`peerbeam-chat/src/message.rs`)

Add the const beside `MSG_TEXT`, add `BadName` to `ChatError`, change `fn mint_id()` to `pub fn mint_id()` (keep its doc comment and the monotonic tie-breaker exactly as they are), and add:

```rust
/// Maximum length of a `FileRef` name, in bytes.
pub const MAX_NAME: usize = 255;
/// MessageType id for a file reference within the Chat channel namespace.
pub const MSG_FILE_REF: u16 = 2;

/// Validate a peer- or user-supplied file name: a bare filename, nothing else.
/// Rejects `../`, absolute paths, separators, empty, `.`/`..`, and over-long.
fn validate_name(name: &str) -> Result<(), ChatError> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(ChatError::BadName(format!("bad length: {}", name.len())));
    }
    let round_trips = std::path::Path::new(name)
        .file_name()
        .map(|f| f == std::ffi::OsStr::new(name))
        .unwrap_or(false);
    if !round_trips {
        return Err(ChatError::BadName(format!("not a bare filename: {name}")));
    }
    Ok(())
}

/// A reference to a file being shared in a conversation, as it travels on the
/// wire. Carries NO local path — the sender's filesystem layout is private (the
/// record-side `FileMeta` holds that). The bytes themselves travel over the
/// TRANSFER stream channel; this message only places the row in the thread and
/// correlates it, because `id` is also used as the transfer id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// Time-ordered id — the chat record key AND the transfer id.
    pub id: String,
    /// RFC3339 timestamp minted by the sender.
    pub timestamp: String,
    /// Bare file name (validated; never a path).
    pub name: String,
    /// Size in bytes, for display before the transfer starts.
    pub size: u64,
}

impl FileRef {
    /// Create a reference, minting a time-ordered id + timestamp.
    pub fn new(name: &str, size: u64) -> Result<FileRef, ChatError> {
        validate_name(name)?;
        Ok(FileRef {
            id: mint_id(),
            timestamp: Utc::now().to_rfc3339(),
            name: name.to_string(),
            size,
        })
    }

    /// The chat MessageType (`FileRef` = 2).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_FILE_REF)
    }

    /// Encode as a Chat-channel frame. Sent OPTIONAL so a peer that does not
    /// implement it skips the message instead of failing the channel
    /// (MESSAGE_REGISTRY.md §6/§7).
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
        validate_name(&self.name)?;
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Chat-channel frame. The name is re-validated: it is
    /// attacker-controlled input.
    pub fn from_frame(frame: &SessionFrame) -> Result<FileRef, ChatError> {
        if frame.message_type.get() != MSG_FILE_REF {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let r: FileRef = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        validate_name(&r.name)?;
        Ok(r)
    }
}
```

Add to `ChatError`:

```rust
    #[error("bad file name: {0}")]
    BadName(String),
```

- [ ] **Step 5: Grow the record schema additively** (`peerbeam-chat/src/record.rs`)

```rust
/// What a record holds: a text body, or a reference to a shared file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A text/markdown message (every record written before file-in-chat).
    #[default]
    Text,
    /// A shared file; see `ChatRecord::file`.
    File,
}

/// Record-side file metadata. NEVER serialized to a frame — `local_path` is the
/// owner's private filesystem layout (the wire type is `FileRef`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub name: String,
    pub size: u64,
    /// Where the file lives on THIS device: the source path on the sender, the
    /// saved path on the receiver. `None` until a receive completes.
    #[serde(default)]
    pub local_path: Option<String>,
}
```

Extend `Status` (append variants; do not reorder or rename existing ones):

```rust
pub enum Status {
    Pending,
    Sent,
    Received,
    /// A file offered to us, awaiting the user's accept/decline.
    PendingApproval,
    /// A file whose bytes are moving.
    Transferring,
    /// The peer declined the file.
    Declined,
    /// The transfer failed.
    Failed,
    /// Left mid-flight by a crash/restart; no event will ever complete it.
    Interrupted,
}
```

Extend `ChatRecord` with two `#[serde(default)]` fields (so every already-persisted record still decodes) and add the two file constructors:

```rust
pub struct ChatRecord {
    pub id: String,
    pub peer_id: String,
    pub direction: Direction,
    pub timestamp: String,
    pub body: String,
    pub status: Status,
    /// Text (default, so legacy records decode) or File.
    #[serde(default)]
    pub kind: Kind,
    /// Present only when `kind == Kind::File`.
    #[serde(default)]
    pub file: Option<FileMeta>,
}
```

Every existing constructor (`sent`, `received`, `out`) must now also set `kind: Kind::Text, file: None`. Add:

```rust
    /// A record for a file we are sending to `peer`.
    #[must_use]
    pub fn file_out(peer: &DeviceId, r: &FileRef, meta: FileMeta, status: Status) -> ChatRecord {
        ChatRecord {
            id: r.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: r.timestamp.clone(),
            body: String::new(),
            status,
            kind: Kind::File,
            file: Some(meta),
        }
    }

    /// A record for a file `peer` is offering us (awaiting approval).
    #[must_use]
    pub fn file_in(peer: &DeviceId, r: &FileRef) -> ChatRecord {
        ChatRecord {
            id: r.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::In,
            timestamp: r.timestamp.clone(),
            body: String::new(),
            status: Status::PendingApproval,
            kind: Kind::File,
            file: Some(FileMeta { name: r.name.clone(), size: r.size, local_path: None }),
        }
    }
```

- [ ] **Step 6: Re-export** in `peerbeam-chat/src/lib.rs`: add `FileRef, MSG_FILE_REF, MAX_NAME, mint_id` to the `message` re-export line and `Kind, FileMeta` to the `record` one.

- [ ] **Step 7: Run tests, full gate, commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat` then the full gate.

```bash
git add rust/crates/peerbeam-domain rust/crates/peerbeam-chat
git commit -m "feat(chat): FileRef wire type + additive file record schema

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Handler dispatch, status transitions, boot reconciliation, outbox field preservation

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/handler.rs` (a `FILE_REF` arm)
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (`set_status`, `reconcile_on_load`, `OutboxEntry` fidelity)
- Test: `rust/crates/peerbeam-chat/tests/roundtrip.rs` (a real two-session FileRef delivery)

**Interfaces:**
- Consumes: `FileRef`, `MSG_FILE_REF`, `Kind`, `FileMeta`, `Status` (Task 1).
- Produces:
  - `ChatStore::set_status(&DeviceId, id: &str, Status) -> Result<(), ChatError>` — loads the record, replaces its status, re-puts it (upsert at the same key). No-op `Ok(())` if absent.
  - `ChatStore::reconcile_on_load() -> Result<usize, ChatError>` — for every conversation namespace it can see via the outbox's peers *and* an explicit peer list argument… **see Step 4 for the exact signature** (`reconcile_peer(&DeviceId) -> Result<usize, ChatError>`, since `AppStore` cannot enumerate namespaces).
  - `ChatHandler` persists an `In`/`File`/`PendingApproval` record for a `FileRef` and fires the sink.

- [ ] **Step 1: Write the failing tests** (`handler.rs` tests + `store.rs` tests)

```rust
// handler.rs tests
#[tokio::test]
async fn handle_persists_a_file_ref_as_pending_approval() {
    let (cs, _dir) = store(7);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
    let peer = DeviceId::from("pb-sender");
    let _ = peer_slot.set(peer.clone());

    let r = FileRef::new("report.pdf", 4096).unwrap();
    handler.handle(r.to_frame(ChannelId::new(1)).unwrap()).await.unwrap();

    let hist = cs.history(&peer).unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].kind, Kind::File);
    assert_eq!(hist[0].status, Status::PendingApproval);
    assert_eq!(hist[0].id, r.id, "the record key IS the transfer id");
    let meta = hist[0].file.clone().unwrap();
    assert_eq!(meta.name, "report.pdf");
    assert_eq!(meta.size, 4096);
    assert!(meta.local_path.is_none());
    assert_eq!(received.lock().unwrap().len(), 1, "sink fired once");
}

#[tokio::test]
async fn handle_dedups_a_repeated_file_ref() {
    let (cs, _dir) = store(8);
    let sink: ReceivedSink = Arc::new(|_| {});
    let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
    let peer = DeviceId::from("pb-sender");
    let _ = peer_slot.set(peer.clone());
    let r = FileRef::new("a.bin", 1).unwrap();
    handler.handle(r.to_frame(ChannelId::new(1)).unwrap()).await.unwrap();
    handler.handle(r.to_frame(ChannelId::new(1)).unwrap()).await.unwrap();
    assert_eq!(cs.history(&peer).unwrap().len(), 1);
}

/// A hostile FileRef must not create a record, and must not kill the channel
/// harder than the registry allows (it is OPTIONAL, so it is skipped).
#[tokio::test]
async fn handle_rejects_a_file_ref_with_a_hostile_name() {
    let (cs, _dir) = store(9);
    let sink: ReceivedSink = Arc::new(|_| {});
    let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
    let peer = DeviceId::from("pb-sender");
    let _ = peer_slot.set(peer.clone());
    let good = FileRef::new("ok.txt", 1).unwrap();
    let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
    frame.payload = bytes::Bytes::from_static(
        br#"{"id":"x","timestamp":"t","name":"../escape","size":1}"#,
    );
    assert!(handler.handle(frame).await.is_err());
    assert!(cs.history(&peer).unwrap().is_empty());
}
```

```rust
// store.rs tests
#[test]
fn set_status_updates_in_place() {
    let (cs, _dir) = store();
    let peer = DeviceId::from("pb-bob");
    let r = FileRef::new("a.bin", 3).unwrap();
    let meta = FileMeta { name: r.name.clone(), size: r.size, local_path: None };
    cs.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring)).unwrap();
    cs.set_status(&peer, &r.id, Status::Sent).unwrap();
    let hist = cs.history(&peer).unwrap();
    assert_eq!(hist.len(), 1, "upsert, not a second row");
    assert_eq!(hist[0].status, Status::Sent);
    assert_eq!(hist[0].kind, Kind::File, "kind survives a status change");
    // Absent id is a no-op, not an error.
    assert!(cs.set_status(&peer, "nope", Status::Failed).is_ok());
}

#[test]
fn reconcile_marks_mid_flight_records_interrupted() {
    let (cs, _dir) = store();
    let peer = DeviceId::from("pb-bob");
    let a = FileRef::new("a.bin", 1).unwrap();
    let b = FileRef::new("b.bin", 1).unwrap();
    let m = |r: &FileRef| FileMeta { name: r.name.clone(), size: r.size, local_path: None };
    cs.append(&ChatRecord::file_out(&peer, &a, m(&a), Status::Transferring)).unwrap();
    cs.append(&ChatRecord::file_in(&peer, &b)).unwrap(); // PendingApproval
    let text = ChatMessage::new("hi").unwrap();
    cs.append(&ChatRecord::sent(&peer, &text)).unwrap();

    assert_eq!(cs.reconcile_peer(&peer).unwrap(), 2);
    let hist = cs.history(&peer).unwrap();
    let by_id = |id: &str| hist.iter().find(|r| r.id == id).unwrap().status;
    assert_eq!(by_id(&a.id), Status::Interrupted);
    assert_eq!(by_id(&b.id), Status::Interrupted);
    assert_eq!(by_id(&text.id), Status::Sent, "settled records untouched");
}

/// 1b's record_sent rebuilt a record from OutboxEntry's own fields, which would
/// silently drop the additive kind/file. The round trip must preserve them.
#[test]
fn outbox_round_trip_preserves_additive_record_fields() {
    let (cs, _dir) = store();
    let peer = DeviceId::from("pb-bob");
    let msg = ChatMessage::new("queued").unwrap();
    cs.enqueue(&peer, &msg).unwrap();
    let entry = cs.outbox_for(&peer).unwrap().remove(0);
    cs.record_sent(&entry).unwrap();
    let hist = cs.history(&peer).unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].status, Status::Sent);
    assert_eq!(hist[0].kind, Kind::Text);
    assert!(hist[0].file.is_none());
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat`
Expected: FAIL — `set_status`/`reconcile_peer` undefined; the FileRef frame is currently swallowed by the increment-0 unknown-OPTIONAL arm so no record appears.

- [ ] **Step 3: Add the `FILE_REF` dispatch arm** (`handler.rs`)

**Critical placement:** increment 0's rule treats *any* `message_type != MSG_TEXT` as unknown. `FILE_REF` must be handled as a **known** type *before* that fallback, or it is silently skipped. Restructure the type check into a match:

```rust
        match frame.message_type.get() {
            MSG_TEXT => {
                let msg = ChatMessage::from_frame(&frame)?;
                if self.store.contains(peer, &msg.id).map_err(SessionError::from)? {
                    return Ok(());
                }
                let rec = ChatRecord::received(peer, &msg);
                self.store.append(&rec).map_err(SessionError::from)?;
                (self.sink)(rec);
                Ok(())
            }
            MSG_FILE_REF => {
                // The peer is offering a file. The bytes arrive separately over a
                // TRANSFER stream channel; this row is what the user approves,
                // and its id is the transfer id that will correlate the two.
                let r = FileRef::from_frame(&frame)?;
                if self.store.contains(peer, &r.id).map_err(SessionError::from)? {
                    return Ok(());
                }
                let rec = ChatRecord::file_in(peer, &r);
                self.store.append(&rec).map_err(SessionError::from)?;
                (self.sink)(rec);
                Ok(())
            }
            // MESSAGE_REGISTRY.md §6 — unknown type: OPTIONAL means skip and keep
            // the channel; required means fail this channel only. (Increment 0.)
            other => {
                if frame.flags.is_optional() {
                    return Ok(());
                }
                Err(SessionError::FrameDecode(format!(
                    "unsupported chat message type {other} (required)"
                )))
            }
        }
```

Keep the peer-bound check above this exactly as it is.

- [ ] **Step 4: Add `set_status`, `reconcile_peer`, and fix the outbox round trip** (`store.rs`)

`AppStore` cannot enumerate namespaces, so reconciliation is per-peer; the FFI/CLI call it for each peer they know. Add:

```rust
    /// Replace a record's status in place (upsert at the same key). A missing
    /// record is a no-op, not an error — a status event can outlive its row.
    pub fn set_status(&self, peer: &DeviceId, id: &str, status: Status) -> Result<(), ChatError> {
        let ns = namespace(peer);
        let Some(bytes) = self
            .store
            .get(&ns, id)
            .map_err(|e| ChatError::Serialization(e.to_string()))?
        else {
            return Ok(());
        };
        let mut rec = ChatRecord::decode(&bytes)?;
        rec.status = status;
        self.append(&rec)
    }

    /// Settle records left mid-flight by a crash or restart. Transfer ids are
    /// process-scoped and no event replays, so a record still `Transferring` or
    /// `PendingApproval` at startup would spin forever. Returns how many were
    /// changed.
    pub fn reconcile_peer(&self, peer: &DeviceId) -> Result<usize, ChatError> {
        let mut changed = 0;
        for rec in self.history(peer)? {
            if matches!(rec.status, Status::Transferring | Status::PendingApproval) {
                self.set_status(peer, &rec.id, Status::Interrupted)?;
                changed += 1;
            }
        }
        Ok(changed)
    }
```

Fix `record_sent` so it cannot drop additive fields — read the existing record and change only its status, falling back to the reconstruction only when no record exists:

```rust
    /// Upsert the conversation record for a delivered entry to `Sent`.
    ///
    /// Reads the existing record and flips only its status, so additive fields
    /// (`kind`, `file`) survive; rebuilding from `OutboxEntry`'s four fields
    /// would silently drop them.
    pub fn record_sent(&self, entry: &OutboxEntry) -> Result<(), ChatError> {
        let peer = DeviceId::from(entry.peer_id.clone());
        let ns = namespace(&peer);
        if let Some(bytes) = self
            .store
            .get(&ns, &entry.message_id)
            .map_err(|e| ChatError::Serialization(e.to_string()))?
        {
            let mut rec = ChatRecord::decode(&bytes)?;
            rec.status = Status::Sent;
            return self.append(&rec);
        }
        self.append(&ChatRecord {
            id: entry.message_id.clone(),
            peer_id: entry.peer_id.clone(),
            direction: Direction::Out,
            timestamp: entry.timestamp.clone(),
            body: entry.body.clone(),
            status: Status::Sent,
            kind: Kind::Text,
            file: None,
        })
    }
```

- [ ] **Step 5: Add the real two-session FileRef test** (`tests/roundtrip.rs`)

Copy the harness from `a_sends_b_receives_and_persists` verbatim (two `PeerSession`s over `MemTransport`, handler on B, `peer_slot_b.set(a_id)` before the run loops). Then, instead of `send_message`, open a CHAT channel, wait for it to open (poll `a_handle.channels()` for `c.id == channel && c.state.is_open()`), and send a `FileRef` frame:

```rust
    let r = FileRef::new("report.pdf", 4096).unwrap();
    let frame = r.to_frame(channel).unwrap();
    a_handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
        .await
        .expect("send file ref");
    // B persists it as a PendingApproval File row, keyed by the id that will
    // also be the transfer id.
    // ... bounded poll (200 × 10ms) on store_b.history(&a_id) ...
    assert_eq!(hist[0].kind, Kind::File);
    assert_eq!(hist[0].status, Status::PendingApproval);
    assert_eq!(hist[0].id, r.id);
```

- [ ] **Step 6: Run tests, full gate, commit**

```bash
git add rust/crates/peerbeam-chat
git commit -m "feat(chat): FileRef dispatch, status transitions, boot reconciliation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Carry the sender's transfer id back out of the receive path

**Files:**
- Modify: `rust/crates/peerbeam-transfer/src/stream.rs` (`Received.transfer_id`)
- Modify: `rust/crates/peerbeam-transfer/src/folder.rs` (`FolderReceived.transfer_id`)

**Interfaces:**
- Produces: `Received { outcome, name, bytes, transfer_id: String }` and `FolderReceived { outcome, root, files, bytes, transfer_id: String }`.

Today `TransferMeta.transfer_id` crosses the wire and is used for progress, then dropped — `Received` has no such field, so a receiver cannot learn which transfer it just took. That is the missing half of correlation.

- [ ] **Step 1: Write the failing test** (`peerbeam-transfer/tests/transfer.rs` or the nearest existing receive test)

Find the existing test that drives `send_file` → `receive_file` over a `MemLink`/duplex pair and add an assertion that the receiver's `Received.transfer_id` equals the `SendRequest.transfer_id` the sender used:

```rust
    assert_eq!(received.transfer_id, req.transfer_id,
        "the receiver must learn the sender's transfer id");
```

- [ ] **Step 2: Run to see it fail** — `cargo test -p peerbeam-transfer` → FAIL: no field `transfer_id` on `Received`.

- [ ] **Step 3: Add the field on both types**

In `stream.rs`, add `pub transfer_id: String` to `Received` (document it: *"the sender's transfer id, from the wire meta — lets a caller correlate this receive with an out-of-band reference such as a chat FileRef"*), and populate it at the single `Ok(Received { .. })` construction site from the already-decoded `meta.transfer_id` (the value is in scope — it is used for progress just above).

In `folder.rs`, do the same for `FolderReceived`, populating from the `transfer_id` binding that `recv_manifest` already returns.

- [ ] **Step 4: Fix all construction/consumption sites the compiler flags.** Run `cargo build --workspace --all-targets`; every `Received {` / `FolderReceived {` literal and any exhaustive destructuring needs the new field. `peerbeam-ffi`'s `ChannelReceived` match arms consume these — keep their behavior unchanged for now (Task 4 uses the value).

- [ ] **Step 5: Run tests, full gate, commit**

```bash
git add rust/crates/peerbeam-transfer
git commit -m "feat(transfer): surface the sender's transfer_id on Received/FolderReceived

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Correlation in the FFI — caller-supplied id, meta peek, `peer_id` on events

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/session_exec.rs` (expose negotiated capabilities on `Session`)
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`send` accepts an id; `handle_incoming` peeks meta)
- Modify: `rust/crates/peerbeam-ffi/src/events.rs` (`peer_id` on transfer events)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` (the end-to-end correlation test)

**Interfaces:**
- Consumes: `Received.transfer_id` (Task 3); `CHAT_FEAT_FILEREF` (Task 1).
- Produces:
  - `session_exec::Session.capabilities: CapabilitySet` (the **negotiated** set) + `Session::supports_file_ref() -> bool`
  - `Manager::send(&self, req: &Value)` honors an optional `transfer_id` in the request JSON
  - `handle_incoming` registers under the sender's id with the real name/size
  - `events::transfer(...)` payloads gain `peer_id`

- [ ] **Step 1: Write the failing correlation test** (`chat_ffi.rs`)

Mirror the existing real-QUIC harness. A manual peer dials into the FFI engine, sends a `FileRef` on CHAT, then sends a file over TRANSFER using **the FileRef's id** as `SendRequest.transfer_id`. Assert:

```rust
    // The approval prompt must carry the real name and size — not "(incoming)".
    let queued = wait_event(10, |e| e["type"] == "transfer_queued")
        .expect("transfer_queued");
    assert_eq!(queued["transfer_id"], file_ref_id, "receiver used the SENDER'S id");
    let p = &queued["payload"];
    assert_eq!(p["file"], "report.pdf");
    assert_eq!(p["size"], 4096);
    assert_eq!(p["peer_id"], sender_device_id, "events must carry the peer device id");
    // And the chat row the FileRef created is the SAME id.
    let hist = call_json(pb_chat_history, &json!({"peer_id": sender_device_id}));
    let msgs = hist["data"]["messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| m["id"] == file_ref_id && m["kind"] == "file"));
```

- [ ] **Step 2: Run to see it fail** — the receiver mints its own id, the payload has no `file`/`size`/`peer_id`.

- [ ] **Step 3: Expose negotiated capabilities on `Session`** (`session_exec.rs`)

In `establish()`, read `ps.capabilities().clone()` in the same block as the other `ps.*()` reads (`ps.id()`, `ps.peer()`, `ps.peer_name()`, `ps.newly_trusted()`, `ps.pairing_code()`), **before** `ps` is moved into the `run` closure. Add the field and a helper:

```rust
    /// The capabilities both sides agreed on (already intersected).
    pub capabilities: CapabilitySet,
```
```rust
impl Session {
    /// Whether the peer negotiated the chat FileRef feature. A peer from before
    /// this feature advertises `features: 0`, so this is false and we must not
    /// offer it a file in chat.
    #[must_use]
    pub fn supports_file_ref(&self) -> bool {
        self.capabilities
            .features(ChannelType::CHAT)
            .is_some_and(|f| f & CHAT_FEAT_FILEREF != 0)
    }
}
```

Also advertise the bit: in `session_cfg`, replace `Capability::new(CHAT)` with `Capability::with_features(CHAT, CHAT_FEAT_FILEREF)`.

- [ ] **Step 4: Accept a caller-supplied transfer id** (`transfer.rs`)

In `Manager::send`, read an optional `transfer_id` from the request and use it instead of `self.next_id()`:

```rust
        // A caller (chat file-share) may supply the id so the transfer and its
        // chat row share one identity. Otherwise mint one as before.
        let id = req
            .get("transfer_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.next_id());
```

Everything downstream already threads `id` into `register`, `SendRequest.transfer_id` and the events — no other change.

- [ ] **Step 5: Peek the transfer meta before registering** (`transfer.rs`, `handle_incoming`)

Increment 0 already obtains `incoming_ch` before any registration. Now decode the first frame to learn the sender's id/name/size **without consuming it**, using the same `PeekLink` mechanism `receive_on_channel` uses. Add a small helper in `peerbeam-transfer` (`session/transfer.rs`) so the FFI does not reimplement framing:

```rust
/// What the first frame of an incoming transfer channel says, without consuming
/// it: the sender's transfer id, the display name, and the size (0 for a folder).
/// The returned channel replays the peeked frame, so the caller can hand it
/// straight to [`receive_on_channel`].
pub async fn peek_incoming_meta(
    incoming: IncomingStreamChannel,
) -> Result<(IncomingStreamChannel, TransferPreview)>;

pub struct TransferPreview {
    pub transfer_id: String,
    pub name: String,
    pub size: u64,
    pub is_folder: bool,
}
```

Implement it by reading one frame, decoding `TransferMeta` (file) or the `FolderMessage::Manifest` (folder), and returning an `IncomingStreamChannel` whose link replays that frame — mirroring `PeekLink`. If the peek fails, return the channel unchanged with a `TransferPreview` carrying an empty id so the caller falls back to `self.next_id()`.

Then in `handle_incoming`, after obtaining `incoming_ch`:

```rust
        let (incoming_ch, preview) = peek_incoming_meta(incoming_ch).await.unwrap_or_else(...);
        let id = if preview.transfer_id.is_empty() { self.next_id() } else { preview.transfer_id.clone() };
        let display = if preview.name.is_empty() { "(incoming)".to_string() } else { preview.name.clone() };
        let active = self.register(&id, "receiving", &peer, &display, Some(preview.size));
        events::transfer(&id, "transfer_queued", json!({
            "peer": peer,
            "peer_id": session.peer_device.0,
            "incoming": true,
            "file": display,
            "size": preview.size,
            "newly_trusted": session.newly_trusted,
            "pairing_code": session.pairing_code.clone(),
        }));
```

The approval gate below this stays byte-identical. `register`'s last parameter is the optional total size — check its current signature and pass the peeked size.

- [ ] **Step 6: Add `peer_id` to transfer events** (`events.rs` / call sites)

Every `events::transfer(...)` payload emitted from a path that knows the peer's `DeviceId` gains `"peer_id"`. Today they carry only the human-readable name, so a surface cannot route an event to a conversation. At minimum: `transfer_queued`, `transfer_started`, `transfer_completed`, `transfer_failed`, `transfer_cancelled`.

- [ ] **Step 7: Run the correlation test + regression nets, full gate, commit**

Run: `cargo test -p peerbeam-ffi --test chat_ffi && cargo test -p peerbeam-ffi --test transfer_ffi && cargo test -p peerbeam-cli --test transfer_e2e`

```bash
git add rust/crates/peerbeam-ffi rust/crates/peerbeam-transfer
git commit -m "feat(ffi): correlate a chat FileRef with its transfer by a shared id

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: The FFI send path — `pb_chat_send_file`

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/send.rs` (`prepare_file_send`, `send_file_ref`)
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`Manager::chat_send_file`, status transitions from transfer events)
- Modify: `rust/crates/peerbeam-ffi/src/lib.rs` (`pb_chat_send_file`)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces:
  - `peerbeam_chat::prepare_file_send(store, peer, path) -> Result<(FileRef, ChatRecord), SendError>` — validates the path (exists, **not a directory**), reads its size, mints the `FileRef`, persists an `Out`/`File`/`Transferring` record with `local_path`, returns both.
  - `peerbeam_chat::send_file_ref(handle, store, peer, &FileRef) -> Result<(), SendError>` — opens the CHAT channel, waits for open, sends the frame.
  - `Manager::chat_send_file(self: &Arc<Self>, req: &Value) -> Op`
  - `pb_chat_send_file(json) -> {id}`

- [ ] **Step 1: Write the failing end-to-end test** (`chat_ffi.rs`)

FFI engine sends a real file to a manual QUIC peer that advertises `CHAT_FEAT_FILEREF`, accepts, and receives it. Assert: `{id}` returned; the peer got a `FileRef` with that id **and** a transfer tagged with the same id; `pb_chat_history` shows one `kind=="file"` row that ends `sent`. Add a second test: a peer advertising **no** feature bit causes `pb_chat_send_file` to return an error mentioning the peer cannot receive chat attachments, and **no** transfer is started.

- [ ] **Step 2: Run to see it fail.**

- [ ] **Step 3: Add `prepare_file_send` + `send_file_ref`** (`peerbeam-chat/src/send.rs`)

```rust
/// Validate a path and stage a file-share: mint the FileRef and persist the
/// outgoing record. Does no I/O beyond metadata — the bytes move over TRANSFER.
pub fn prepare_file_send(
    store: &ChatStore,
    peer: &DeviceId,
    path: &str,
) -> Result<(FileRef, ChatRecord), SendError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| SendError::Session(format!("cannot read {path}: {e}")))?;
    if meta.is_dir() {
        return Err(SendError::Session(
            "folders aren't supported in chat yet — use Send folder".into(),
        ));
    }
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| SendError::Session(format!("no file name in {path}")))?;
    let r = FileRef::new(&name, meta.len())?;
    let rec = ChatRecord::file_out(
        peer,
        &r,
        FileMeta { name: r.name.clone(), size: r.size, local_path: Some(path.to_string()) },
        Status::Transferring,
    );
    store.append(&rec)?;
    Ok((r, rec))
}

/// Send a prepared FileRef over the session's CHAT channel.
pub async fn send_file_ref(
    handle: &SessionHandle,
    r: &FileRef,
) -> Result<(), SendError> {
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
    let frame = r.to_frame(channel)?;
    handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()))
}
```

- [ ] **Step 4: Add `Manager::chat_send_file`** (`transfer.rs`)

Order matters — validate and persist before touching the network, refuse a peer that cannot receive it, and always close the session:

```rust
pub fn chat_send_file(self: &Arc<Self>, req: &Value) -> Op {
    let device = device_from(req.get("peer"))?;
    let path = req.get("path").and_then(|v| v.as_str())
        .ok_or((Code::InvalidArgument, "path required".into()))?.to_string();
    // Validate + persist the outgoing row first, so a failure never half-sends.
    let (file_ref, _rec) = peerbeam_chat::prepare_file_send(&self.chat, &device.id, &path)
        .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
    let id = file_ref.id.clone();
    // Dial, check the negotiated feature, send the FileRef, then the bytes with
    // the SAME id. Spawned so the FFI call returns immediately.
    let me = self.clone();
    crate::runtime::spawn(async move { me.run_chat_file_send(device, path, file_ref).await });
    Ok(json!({ "id": id }))
}
```

`run_chat_file_send` dials (with `chat_wiring()`), and if `!session.supports_file_ref()` marks the record `Failed`, emits a `chat_status` carrying a message that the peer cannot receive chat attachments, closes the session and returns — **it does not fall back to a plain transfer**. Otherwise it sends the FileRef, then runs the normal file send with `transfer_id = id`, always closing the session, and drives `set_status` from the outcome (`Sent` on success, `Failed` otherwise).

- [ ] **Step 5: Drive both ends' records from transfer events.** Where `finish`/`finish_failed` and the decline path already emit `transfer_completed`/`_failed`/`_cancelled`, call `self.chat.set_status(peer, id, ...)` with `Sent`/`Received`, `Failed`, `Declined` respectively — guarded so it is a no-op for a transfer that has no chat row (`set_status` already no-ops on a missing record). Emit `chat_status` so surfaces update live.

- [ ] **Step 6: Export `pb_chat_send_file`** (`lib.rs`, mirroring `pb_chat_send`)

```rust
/// Send a file inside a chat thread: `{peer:{...},path}` → `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_send_file(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_send_file(&read_json(json)?))()))
}
```

- [ ] **Step 7: Call `reconcile_peer` at startup.** In `runtime::init`, after the `ChatStore` is built, reconcile every peer that has history the runtime knows about (at minimum every peer in `chat.outbox_peers()`); the Flutter side additionally reconciles when it opens a thread (Task 7).

- [ ] **Step 8: Run tests + regression nets, full gate, commit**

```bash
git add rust/crates/peerbeam-chat rust/crates/peerbeam-ffi
git commit -m "feat(ffi): pb_chat_send_file — attach a file to a conversation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: CLI — `chat send --file` and file rows

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/cli.rs` (`--file` on `ChatAction::Send`)
- Modify: `rust/bins/peerbeam-cli/src/chat.rs` (`send`, `history`, `watch`, `received_sink`)
- Test: `rust/bins/peerbeam-cli/src/chat.rs` tests + `tests/cli_parse.rs`

**Interfaces:** Consumes `prepare_file_send`, `send_file_ref`, `Session::supports_file_ref` (Tasks 1-5).

- [ ] **Step 1: Write failing tests** — `cli_parse.rs`: `chat send --to bob --file /tmp/a.bin` parses with `text` optional. `chat.rs` unit test: rendering a `Kind::File` record produces a line naming the file and its size (and JSON carrying `kind`/`file`), not a blank body.

- [ ] **Step 2: Add the flag** — make `ChatAction::Send`'s `text` an `Option<String>` and add `#[arg(long, value_name = "PATH")] file: Option<String>`; require exactly one of them (clap `required_unless_present`/`conflicts_with`, matching the file's existing style for `--to`/`--addr`).

- [ ] **Step 3: Implement the file branch in `send`** — resolve the peer exactly as the text path does; build `SecureCtx` + `chat_store`; `prepare_file_send`; dial with chat wiring; if `!supports_file_ref()` print a clear refusal and mark the record `Failed`; else `send_file_ref`, then `send_file_on_session` with `SendRequest.transfer_id = file_ref.id`; always `session.close().await`; report `{event:"chat_file_sent", id, peer, delivered}`.

- [ ] **Step 4: Render file rows in `history`, `watch`, and `received_sink`** — a `Kind::File` record has an empty `body`; print `[timestamp] <dir> file: <name> (<size>) — <status>` in human mode and include `kind` + `file` in JSON.

- [ ] **Step 5: Say what a headless receiver needs.** When `chat watch` receives a `FileRef`, print a line noting that accepting the file requires `peerbeam receive`/`daemon` running (`chat watch` alone discards incoming stream channels), so an operator is not left waiting for a transfer that will time out.

- [ ] **Step 6: Run tests, full gate, commit**

```bash
git add rust/bins/peerbeam-cli
git commit -m "feat(cli): chat send --file and file rows in history/watch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Flutter — attach button, file bubble, inline approval

**Files:**
- Modify: `flutter/lib/sdk/models.dart` (`ChatMessage` gains `kind`, `fileName`, `fileSize`, `localPath`)
- Modify: `flutter/lib/sdk/ffi/bindings.dart`, `flutter/lib/sdk/peerbeam.dart` (`chatSendFile`)
- Modify: `flutter/lib/sdk/events.dart` (file fields on the chat events)
- Modify: `flutter/lib/data/chat_repository.dart` (`sendFile`, transfer-event → status)
- Modify: `flutter/lib/features/chat/chat_screen.dart` (attach button, file bubble, inline approval)
- Modify: `flutter/lib/features/home/home_screen.dart` (chat reachable for offline peers)
- Test: `flutter/test/sdk/chat_test.dart`, `flutter/test/data/repository_test.dart`

- [ ] **Step 1: Write failing Dart tests** — `ChatMessage.fromJson` parses `kind:"file"` + `file:{name,size,local_path}`; a `ChatRepository.sendFile` call reaches the fake api; a `chat_status` for a file id flips that message's status; `messagesFor` keeps text and file rows distinct.

- [ ] **Step 2: Extend the model** — add `final String kind; final String? fileName; final int? fileSize; final String? localPath;` with `bool get isFile => kind == 'file'`, parsed from the same JSON the Rust `chat_history`/`chat_received` emit. Extend `copyWith` to preserve them.

- [ ] **Step 3: Bind + expose `chatSendFile`** — `bindings.dart` gains `_chatSendFile` looked up as `pb_chat_send_file` with a `String chatSendFile(String json)` wrapper; `PeerBeamApi` gains `Future<String> chatSendFile(PeerTarget peer, String path)`; the real impl marshals `{peer: peer.toJson(), path: path}` and returns `data['id']`. Add the method to the fake api used by tests.

- [ ] **Step 4: `ChatRepository.sendFile`** — mirrors `send`: append an optimistic file message (`status: 'transferring'`), call `chatSendFile`, then `refresh`. Also subscribe to `TransferEvent`s: when one carries a `peer_id` and its `transferId` matches a message id in that peer's list, update that message's status (`completed→sent`/`received`, `failed→failed`, `cancelled→declined`) and `notifyListeners()`.

- [ ] **Step 5: Attach button** — in the composer `Row`, add an `IconButton(Icons.attach_file_rounded)` before the `TextField` that calls `pickFilesToStage()` (the existing byte-free, Android-OOM-hardened picker) and, for **each** returned file, calls `state.chat.sendFile(peerId, peer, path)`. Never send only `picked.first`.

- [ ] **Step 6: File bubble** — when `message.isFile`, render a file row instead of body text: an icon, `fileName`, formatted `fileSize` (reuse `formatBytes` from `state/models.dart`), and a status line. For an outgoing `transferring` message show a `LinearProgressIndicator` driven by the matching `Transfer` in `TransferRepository` when present. For an incoming `pendingApproval` message, render Accept / Trust / Decline buttons wired to `state.transfer.accept(id)` / `acceptTrust(id)` / `reject(id)` — the ids match by construction. For a completed incoming message, tapping opens the file via `openLocalPath`, using the **History screen's Android SAF name-based fallback** when `localPath` dangles (on Android the engine's copy is deleted after SAF publish).

- [ ] **Step 7: Make a thread reachable for an offline peer** — stop gating the chat action on `device.online` in `home_screen.dart`/`DeviceTile`, and add a chat action to saved devices. (2a still fails the *send*; this is what makes 2b's queue reachable and is cheap now.)

- [ ] **Step 8: `flutter analyze && flutter test`, full Rust gate, commit**

```bash
git add flutter/lib flutter/test
git commit -m "feat(flutter): attach files in chat — file bubble, inline approval, open

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Docs

**Files:** `docs/MESSAGE_REGISTRY.md`, `docs/CLI.md`

- [ ] **Step 1: Registry** — §2/§4: record `FileRef = 2` as implemented for Chat (0x0101), and record `CHAT_FEAT_FILEREF = 1 << 0` as the first assigned CHAT capability feature bit, noting that a peer must advertise it before a `FileRef` is sent. Keep `Receipt`/`Reaction`/`Edit` reserved.
- [ ] **Step 2: CLI.md** — document `chat send [--to <peer>|--addr IP:PORT] (--file <path> | <text>)`; note that folders are not supported in chat; note that a headless receiver needs `peerbeam receive`/`daemon` running to accept a chat file (`chat watch` alone will not).
- [ ] **Step 3: Gate + commit**

```bash
cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check
git add docs/MESSAGE_REGISTRY.md docs/CLI.md
git commit -m "docs: file-in-chat message id, feature bit, and CLI usage

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:** feature negotiation → T1 (bit) + T4 (advertise/check) + T5 (refuse); FileRef wire type + name safety + no-local_path → T1; additive record schema + legacy decode → T1; handler dispatch + dedup → T2; status transitions, boot reconciliation, outbox fidelity → T2 (+T5 wiring); `transfer_id` out of the receive path → T3; correlation + approval-with-a-name + `peer_id` on events → T4; send path + folder refusal + always-close → T5; CLI → T6; Flutter (attach, bubble, inline approval, open, offline-reachable thread) → T7; docs → T8. Orphan rules: *transfer with no FileRef* is covered (T4 falls back to a minted id and creates no chat row); ***FileRef with no transfer* expiry is NOT implemented in 2a** — `reconcile_peer` settles such rows to `Interrupted` at next startup, which bounds the spec's "permanent ghost row" concern without a timer; a live timer belongs with 2b's drain. Flagged here rather than silently dropped.

**Type consistency:** `FileRef{id,timestamp,name,size}` (wire) vs `FileMeta{name,size,local_path}` (record) used consistently; `ChatRecord.kind: Kind` + `file: Option<FileMeta>`; `Status` variants match across Rust, the FFI JSON (`serde(rename_all="lowercase")`) and the Dart strings; `set_status(&DeviceId,&str,Status)`, `reconcile_peer(&DeviceId)->Result<usize,_>`, `prepare_file_send(store,peer,path)->(FileRef,ChatRecord)`, `send_file_ref(handle,&FileRef)`, `Session::supports_file_ref()`, `pb_chat_send_file({peer,path})->{id}` are used with identical signatures wherever they appear.

**Placeholder scan:** none. Two steps deliberately require reading before writing — `register`'s current size parameter (T4 S5) and the exact clap style for mutually-exclusive args (T6 S2) — each stating precisely what to check and what the result must satisfy.
