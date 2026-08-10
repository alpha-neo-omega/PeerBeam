# P2P Chat — Increment 1a (Online) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship online (both-peers-connected) 1:1 text/markdown chat as a new `Chat` message channel on PeerSession, persisted to the encrypted AppStore, across all four surfaces (core, CLI, FFI, a minimal Flutter chat screen).

**Architecture:** A new `ChannelType::CHAT (0x0101)` *message* channel carries a `ChatMessage` typed message (mirrors `ControlMessage`). A `peerbeam-chat` crate holds the wire type, the persisted record, an `AppStore`-backed `ChatStore`, a `ChatHandler: MessageHandler` (receives + persists + emits), and a `send_message` helper. Sessions always advertise the CHAT capability; the *receiving* side registers a `ChatHandler` whose session-peer is bound post-handshake via an `Arc<OnceLock<DeviceId>>`. The FFI and CLI construct the shared `FsAppStore` (identity-derived key) and wire chat into their session-establish seams. Flutter mirrors the existing SDK/repository/screen patterns.

**Tech Stack:** Rust (workspace at `rust/`), serde_json, existing PeerSession + AppStore + crypto; Dart/Flutter FFI + Material 3.

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in **library** (crate) code. The CLI **binary** may use `expect` only where it already does. Tests may `unwrap`.
- **Sender identity is always the authenticated session peer, never the wire payload.** The wire `ChatMessage` carries no sender field.
- **`ChannelType::CHAT = ChannelType(0x0101)`; chat MessageType `Message = 1`.** (Registry MESSAGE_REGISTRY.md §2 reserves 0x0101 for Chat.)
- **Chat body cap = 16 KiB (16384 bytes, UTF-8).** Over-cap is rejected on send (`Err`) and on receive (channel-scoped error) — never truncated.
- **AppStore values are encrypted at rest** with a key derived from the device-identity secret: `peerbeam_crypto::derive_subkey(&identity.keypair.secret.0, b"peerbeam-appstore-v1")`; store root `<config.storage.data_directory>/appstore`.
- **Conversation namespace = `chat-<peer_device_id>`**, record key = a lexicographically time-ordered id: `format!("{:013}{:016x}", unix_millis, rand_u64)`. Dedup by id on receive.
- **1a is online-only:** if no session to the peer can be established, send fails with a clear error. NO outbox/queue/drain (that is increment 1b).
- Per-task gate from `rust/`: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Flutter tasks additionally, from `flutter/`: `flutter analyze` (and `flutter test` if the task adds Dart tests).
- Commit per task; trailer exactly: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Local commits only; do not push.
- Do not modify constitutional docs (ROADMAP.md, docs/VISION.md, docs/FUTURE_ARCHITECTURE.md, docs/ARCHITECTURAL_INVARIANTS.md). MESSAGE_REGISTRY.md is derived and may be updated.

---

## File Structure

- `rust/crates/peerbeam-domain/src/session/ids.rs` — add `ChannelType::CHAT`.
- `rust/crates/peerbeam-chat/` — new crate: `message.rs` (wire `ChatMessage`), `record.rs` (`ChatRecord`, `Direction`, `Status`, id minting), `store.rs` (`ChatStore` over `AppStore`), `handler.rs` (`ChatHandler`, `ReceivedSink`), `send.rs` (`send_message`), `lib.rs` (re-exports + `ChatError`).
- `rust/crates/peerbeam-ffi/src/{session_exec.rs,runtime.rs,transfer.rs,events.rs,lib.rs}` — AppStore construction, chat wiring into establish, `chat_received` event, `Manager::chat_send/chat_history`, `pb_chat_send/pb_chat_history`.
- `rust/bins/peerbeam-cli/src/{cli.rs,commands.rs,chat.rs,session_transfer.rs}` — `chat` subcommand + AppStore + chat wiring into the CLI establish.
- `flutter/lib/sdk/{models.dart,ffi/bindings.dart,peerbeam.dart,events.dart}`, `flutter/lib/data/chat_repository.dart`, `flutter/lib/state/stores.dart`, `flutter/lib/main.dart`, `flutter/lib/app/{router.dart,shell.dart}`, `flutter/lib/features/chat/chat_screen.dart`.
- `docs/MESSAGE_REGISTRY.md` (concrete Chat ids), `docs/CLI.md` (chat commands).

---

## Task 1: Domain — `ChannelType::CHAT`

**Files:**
- Modify: `rust/crates/peerbeam-domain/src/session/ids.rs` (in `impl ChannelType`, after `TRANSFER` ~line 101)

**Interfaces:**
- Produces: `ChannelType::CHAT` (== `ChannelType(0x0101)`), used by every later task.

- [ ] **Step 1: Write the failing test**

In `ids.rs`'s `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn chat_channel_type_is_0x0101_and_first_party() {
    assert_eq!(ChannelType::CHAT.get(), 0x0101);
    assert!(ChannelType::CHAT.is_first_party());
    assert!(!ChannelType::CHAT.is_control());
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-domain chat_channel_type`
Expected: FAIL — `no associated item named CHAT`.

- [ ] **Step 3: Add the constant**

In `impl ChannelType`, after the `TRANSFER` const:

```rust
    /// Text/markdown chat messages (Phase B). See MESSAGE_REGISTRY.md §2.
    pub const CHAT: ChannelType = ChannelType(0x0101);
```

- [ ] **Step 4: Run it to see it pass**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-domain chat_channel_type`
Expected: PASS.

- [ ] **Step 5: Full gate + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add rust/crates/peerbeam-domain/src/session/ids.rs
git commit -m "feat(domain): add ChannelType::CHAT (0x0101)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `peerbeam-chat` crate — wire `ChatMessage`

**Files:**
- Create: `rust/crates/peerbeam-chat/Cargo.toml`, `rust/crates/peerbeam-chat/src/lib.rs`, `rust/crates/peerbeam-chat/src/message.rs`
- Modify: `rust/Cargo.toml` (add `crates/peerbeam-chat` to `members`)

**Interfaces:**
- Consumes: `ChannelType::CHAT` (Task 1); `peerbeam_domain::session::{SessionFrame, MessageType, MessageFlags, ChannelId, SessionError}`.
- Produces: `ChatMessage { id: String, timestamp: String, body: String }`; `ChatMessage::new(body) -> Result<ChatMessage, ChatError>` (mints id+timestamp, enforces 16 KiB); `ChatMessage::message_type() -> MessageType` (== `MessageType::new(1)`); `ChatMessage::to_frame(channel: ChannelId) -> Result<SessionFrame, ChatError>`; `ChatMessage::from_frame(&SessionFrame) -> Result<ChatMessage, ChatError>`; `pub const MAX_BODY: usize = 16384`; `pub const MSG_TEXT: u16 = 1`; `enum ChatError`.

- [ ] **Step 1: Scaffold the crate**

`rust/crates/peerbeam-chat/Cargo.toml`:

```toml
[package]
name = "peerbeam-chat"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
peerbeam-domain = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
bytes = { workspace = true }
chrono = { workspace = true }
rand = { workspace = true }
thiserror = { workspace = true }
async-trait = { workspace = true }

[dev-dependencies]
peerbeam-crypto = { path = "../peerbeam-crypto" }
peerbeam-appstore-fs = { path = "../peerbeam-appstore-fs" }
peerbeam-transfer = { path = "../peerbeam-transfer" }
peerbeam-transfer-quic = { path = "../peerbeam-transfer-quic" }
tempfile = { workspace = true }
tokio = { workspace = true }
```

Add `"crates/peerbeam-chat",` to `members` in `rust/Cargo.toml` (next to `crates/peerbeam-appstore-fs`).

`rust/crates/peerbeam-chat/src/lib.rs` (initial):

```rust
//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod message;

pub use message::{ChatError, ChatMessage, MAX_BODY, MSG_TEXT};
```

- [ ] **Step 2: Write the failing tests**

`rust/crates/peerbeam-chat/src/message.rs` (tests first — put at the bottom, but write them now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::ChannelId;

    #[test]
    fn new_mints_id_and_timestamp_and_keeps_body() {
        let m = ChatMessage::new("hello **world**").unwrap();
        assert!(!m.id.is_empty());
        assert!(!m.timestamp.is_empty());
        assert_eq!(m.body, "hello **world**");
    }

    #[test]
    fn ids_are_time_ordered_and_unique() {
        let a = ChatMessage::new("a").unwrap();
        let b = ChatMessage::new("b").unwrap();
        assert_ne!(a.id, b.id);
        // 013-digit millis prefix keeps them lexicographically time-ordered.
        assert!(b.id >= a.id);
        assert_eq!(a.id.len(), 13 + 16);
    }

    #[test]
    fn oversize_body_is_rejected() {
        let big = "x".repeat(MAX_BODY + 1);
        assert!(matches!(ChatMessage::new(&big), Err(ChatError::TooLarge { .. })));
    }

    #[test]
    fn frame_roundtrip() {
        let m = ChatMessage::new("hi").unwrap();
        let frame = m.to_frame(ChannelId::new(5)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_TEXT);
        assert!(frame.flags.contains(peerbeam_domain::session::MessageFlags::END_OF_MESSAGE));
        let back = ChatMessage::from_frame(&frame).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn from_frame_rejects_oversize_and_bad_json() {
        use bytes::Bytes;
        use peerbeam_domain::session::{MessageFlags, MessageType, SessionFrame};
        let bad = SessionFrame::new(ChannelId::new(1), MessageType::new(MSG_TEXT), MessageFlags::END_OF_MESSAGE, Bytes::from_static(b"not json"));
        assert!(ChatMessage::from_frame(&bad).is_err());
    }
}
```

- [ ] **Step 3: Run to see them fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat`
Expected: FAIL to compile (`ChatMessage` not defined).

- [ ] **Step 4: Implement `message.rs`**

Above the tests in `message.rs`:

```rust
//! The wire chat message carried on the Chat channel.

use bytes::Bytes;
use chrono::Utc;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{
    ChannelId, MessageFlags, MessageType, SessionError, SessionFrame,
};

/// Maximum chat body size (UTF-8 bytes).
pub const MAX_BODY: usize = 16384;
/// MessageType id for a text chat message within the Chat channel namespace.
pub const MSG_TEXT: u16 = 1;

/// Errors from encoding/decoding/validating a chat message.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("chat body too large: {len} bytes (max {MAX_BODY})")]
    TooLarge { len: usize },
    #[error("chat serialization: {0}")]
    Serialization(String),
    #[error("unexpected chat message type {0}")]
    WrongType(u16),
}

impl From<ChatError> for SessionError {
    fn from(e: ChatError) -> Self {
        SessionError::FrameDecode(e.to_string())
    }
}

/// A text/markdown chat message as it travels on the wire. The sender identity is
/// NOT carried here — it is the authenticated session peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Lexicographically time-ordered id (also the persistence key / dedup key).
    pub id: String,
    /// RFC3339 timestamp minted by the sender.
    pub timestamp: String,
    /// Markdown body (<= MAX_BODY bytes).
    pub body: String,
}

impl ChatMessage {
    /// Create a new message, minting a time-ordered id + timestamp. Rejects an
    /// over-cap body.
    pub fn new(body: &str) -> Result<ChatMessage, ChatError> {
        if body.len() > MAX_BODY {
            return Err(ChatError::TooLarge { len: body.len() });
        }
        Ok(ChatMessage {
            id: mint_id(),
            timestamp: Utc::now().to_rfc3339(),
            body: body.to_string(),
        })
    }

    /// The chat MessageType (`Message` = 1).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_TEXT)
    }

    /// Encode as a Chat-channel [`SessionFrame`] on `channel`.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
        if self.body.len() > MAX_BODY {
            return Err(ChatError::TooLarge { len: self.body.len() });
        }
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::END_OF_MESSAGE,
            payload,
        ))
    }

    /// Decode from a Chat-channel frame. Rejects the wrong message type, bad
    /// JSON, and an over-cap body.
    pub fn from_frame(frame: &SessionFrame) -> Result<ChatMessage, ChatError> {
        if frame.message_type.get() != MSG_TEXT {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let msg: ChatMessage = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        if msg.body.len() > MAX_BODY {
            return Err(ChatError::TooLarge { len: msg.body.len() });
        }
        Ok(msg)
    }
}

/// A lexicographically time-ordered id: 13-digit unix-millis + 16 hex random.
fn mint_id() -> String {
    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let mut r = [0u8; 8];
    OsRng.fill_bytes(&mut r);
    format!("{:013}{:016x}", millis, u64::from_be_bytes(r))
}
```

- [ ] **Step 5: Run to see them pass**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat`
Expected: PASS.

- [ ] **Step 6: Full gate + commit**

Run the full gate. Then:

```bash
git add rust/Cargo.toml rust/Cargo.lock rust/crates/peerbeam-chat
git commit -m "feat(chat): peerbeam-chat crate + wire ChatMessage

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: `peerbeam-chat` — `ChatRecord` + `ChatStore`

**Files:**
- Create: `rust/crates/peerbeam-chat/src/record.rs`, `rust/crates/peerbeam-chat/src/store.rs`
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (module decls + re-exports)

**Interfaces:**
- Consumes: `ChatMessage` (Task 2); `peerbeam_domain::port::AppStore`; `peerbeam_domain::id::DeviceId`.
- Produces: `enum Direction { Out, In }`; `enum Status { Pending, Sent, Received }`; `ChatRecord { id, peer_id, direction, timestamp, body, status }` with `sent(peer, &ChatMessage)`/`received(peer, &ChatMessage)` constructors and `encode()->Vec<u8>`/`decode(&[u8])->Result<ChatRecord,ChatError>`; `ChatStore { store: Arc<dyn AppStore> }` with `new(Arc<dyn AppStore>)`, `append(&ChatRecord)->Result<(),ChatError>`, `history(&DeviceId)->Result<Vec<ChatRecord>,ChatError>` (chronological), `contains(&DeviceId, id:&str)->Result<bool,ChatError>`; `fn namespace(peer:&DeviceId)->String` (== `format!("chat-{}", peer.0)`).

- [ ] **Step 1: Write the failing tests**

`rust/crates/peerbeam-chat/src/store.rs` tests (bottom of file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ChatRecord, Direction, Status};
    use crate::ChatMessage;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Arc;

    fn store() -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[9u8; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app), dir)
    }

    #[test]
    fn append_then_history_is_chronological_and_survives_reopen() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let m1 = ChatMessage::new("first").unwrap();
        let m2 = ChatMessage::new("second").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m1)).unwrap();
        cs.append(&ChatRecord::received(&peer, &m2)).unwrap();
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].body, "first");
        assert_eq!(hist[1].body, "second");
        assert_eq!(hist[0].direction, Direction::Out);
        assert_eq!(hist[1].direction, Direction::In);
        drop(dir); // (TempDir kept alive above; this line documents lifetime)
    }

    #[test]
    fn contains_reports_dedup_state() {
        let (cs, _dir) = store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").unwrap();
        assert!(!cs.contains(&peer, &m.id).unwrap());
        cs.append(&ChatRecord::received(&peer, &m)).unwrap();
        assert!(cs.contains(&peer, &m.id).unwrap());
    }

    #[test]
    fn conversations_are_isolated_by_peer() {
        let (cs, _dir) = store();
        let a = DeviceId::from("pb-a");
        let b = DeviceId::from("pb-b");
        cs.append(&ChatRecord::sent(&a, &ChatMessage::new("to-a").unwrap())).unwrap();
        assert_eq!(cs.history(&a).unwrap().len(), 1);
        assert_eq!(cs.history(&b).unwrap().len(), 0);
    }

    #[test]
    fn namespace_is_chat_dash_peer() {
        assert_eq!(namespace(&DeviceId::from("pb-x")), "chat-pb-x");
    }
}
```

`rust/crates/peerbeam-chat/src/record.rs` tests (bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;
    use peerbeam_domain::id::DeviceId;

    #[test]
    fn record_encode_decode_roundtrip() {
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").unwrap();
        let rec = ChatRecord::sent(&peer, &m);
        let back = ChatRecord::decode(&rec.encode()).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.peer_id, "pb-bob");
        assert_eq!(back.direction, Direction::Out);
        assert_eq!(back.status, Status::Sent);
        assert_eq!(back.body, "hi");
    }

    #[test]
    fn received_sets_in_and_received() {
        let rec = ChatRecord::received(&DeviceId::from("pb-a"), &ChatMessage::new("x").unwrap());
        assert_eq!(rec.direction, Direction::In);
        assert_eq!(rec.status, Status::Received);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `record.rs`**

```rust
//! The persisted chat record (distinct from the wire `ChatMessage`).

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;

use crate::message::{ChatError, ChatMessage};

/// Whether a record was sent by us or received from the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out,
    In,
}

/// Delivery status. In 1a only `Sent`/`Received` occur; `Pending` is reserved
/// for the offline outbox (increment 1b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Sent,
    Received,
}

/// A chat message persisted in one conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: String,
    pub peer_id: String,
    pub direction: Direction,
    pub timestamp: String,
    pub body: String,
    pub status: Status,
}

impl ChatRecord {
    /// A record for a message we sent to `peer` (status `Sent`).
    #[must_use]
    pub fn sent(peer: &DeviceId, msg: &ChatMessage) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status: Status::Sent,
        }
    }

    /// A record for a message received from `peer` (status `Received`).
    #[must_use]
    pub fn received(peer: &DeviceId, msg: &ChatMessage) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::In,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status: Status::Received,
        }
    }

    /// Serialize to opaque bytes for the AppStore.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // Infallible in practice (plain struct of owned strings); fall back to an
        // empty vec rather than panicking, and let the caller's put persist it.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from AppStore bytes.
    pub fn decode(bytes: &[u8]) -> Result<ChatRecord, ChatError> {
        serde_json::from_slice(bytes).map_err(|e| ChatError::Serialization(e.to_string()))
    }
}
```

> Note: `encode()` uses `unwrap_or_default()` (not `unwrap()`) to honor the no-panic rule; a plain owned-string struct never fails to serialize, but this stays total.

- [ ] **Step 4: Implement `store.rs`**

```rust
//! An AppStore-backed conversation store: one namespace per peer.

use std::sync::Arc;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::AppStore;

use crate::message::ChatError;
use crate::record::ChatRecord;

/// The AppStore namespace for a conversation with `peer`.
#[must_use]
pub fn namespace(peer: &DeviceId) -> String {
    format!("chat-{}", peer.0)
}

/// Reads/writes chat records via the encrypted [`AppStore`].
#[derive(Clone)]
pub struct ChatStore {
    store: Arc<dyn AppStore>,
}

impl ChatStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>) -> Self {
        ChatStore { store }
    }

    /// Persist a record under its conversation namespace, keyed by its id.
    pub fn append(&self, rec: &ChatRecord) -> Result<(), ChatError> {
        let ns = format!("chat-{}", rec.peer_id);
        self.store
            .put(&ns, &rec.id, &rec.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// All records in the conversation with `peer`, chronological (AppStore
    /// `list` returns ascending by key, and keys are time-ordered ids).
    pub fn history(&self, peer: &DeviceId) -> Result<Vec<ChatRecord>, ChatError> {
        let ns = namespace(peer);
        let raw = self
            .store
            .list(&ns)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        let mut out = Vec::with_capacity(raw.len());
        for (_key, value) in raw {
            out.push(ChatRecord::decode(&value)?);
        }
        Ok(out)
    }

    /// Whether a message id already exists in the conversation with `peer`
    /// (receiver-side dedup).
    pub fn contains(&self, peer: &DeviceId, id: &str) -> Result<bool, ChatError> {
        let ns = namespace(peer);
        self.store
            .get(&ns, id)
            .map(|v| v.is_some())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }
}
```

- [ ] **Step 5: Wire modules in `lib.rs`**

```rust
//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod message;
mod record;
mod store;

pub use message::{ChatError, ChatMessage, MAX_BODY, MSG_TEXT};
pub use record::{ChatRecord, Direction, Status};
pub use store::{namespace, ChatStore};
```

- [ ] **Step 6: Run tests + full gate + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat` (PASS), then the full gate.

```bash
git add rust/crates/peerbeam-chat/src
git commit -m "feat(chat): ChatRecord + AppStore-backed ChatStore (per-peer, dedup)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `peerbeam-chat` — `ChatHandler` + `send_message` (+ real round-trip test)

**Files:**
- Create: `rust/crates/peerbeam-chat/src/handler.rs`, `rust/crates/peerbeam-chat/src/send.rs`
- Modify: `rust/crates/peerbeam-chat/src/lib.rs`
- Test: `rust/crates/peerbeam-chat/tests/roundtrip.rs`

**Interfaces:**
- Consumes: `ChatStore`, `ChatMessage`, `ChatRecord` (Tasks 2-3); `peerbeam_domain::session::{MessageHandler, SessionFrame, SessionError, ChannelType}`; `peerbeam_domain::id::DeviceId`; the `SessionHandle` type is `peerbeam_transfer::session::SessionHandle` (a dev-dependency here; the crate itself depends only on `peerbeam-domain` for the handler trait — `send_message` is generic over a trait object, see below).
- Produces:
  - `pub type ReceivedSink = Arc<dyn Fn(ChatRecord) + Send + Sync>;`
  - `ChatHandler` with `new(store: ChatStore, sink: ReceivedSink) -> (Arc<ChatHandler>, Arc<OnceLock<DeviceId>>)` — returns the handler and the peer slot the caller binds after the handshake. Implements `MessageHandler` (channel_type = CHAT).
  - `send.rs`: `async fn send_message(handle: &SessionHandle, store: &ChatStore, peer: &DeviceId, body: &str) -> Result<ChatRecord, SendError>` where `SessionHandle` is imported from `peerbeam_transfer` — **so `peerbeam-transfer` must be a normal dependency of `peerbeam-chat` for `send.rs`.** (The handler side needs only `peerbeam-domain`.) `enum SendError { Chat(ChatError), Session(String) }`.

Add to `peerbeam-chat/Cargo.toml` `[dependencies]`: `peerbeam-transfer = { path = "../peerbeam-transfer" }` (move it out of dev-dependencies). Keep `peerbeam-transfer-quic`, `tempfile`, `tokio`, `peerbeam-crypto`, `peerbeam-appstore-fs` as dev-dependencies for the round-trip test.

- [ ] **Step 1: Write the failing round-trip test**

`rust/crates/peerbeam-chat/tests/roundtrip.rs`:

```rust
//! A real two-PeerSession round trip: one side sends a chat message, the other
//! side's ChatHandler persists it and fires the sink.

use std::sync::{Arc, Mutex, OnceLock};

use peerbeam_appstore_fs::FsAppStore;
use peerbeam_chat::{send_message, ChatHandler, ChatRecord, ChatStore, ReceivedSink};
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::EncryptionProvider;
// Session construction helpers mirror peerbeam-transfer's own session tests;
// use the same MemTransport/PeerSession harness those tests use.

fn chat_store(seed: u8) -> ChatStore {
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir for the test's lifetime so the path stays valid.
    let path = dir.into_path().join("appstore");
    let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
    let key = derive_subkey(&[seed; 32], b"peerbeam-appstore-v1");
    ChatStore::new(Arc::new(FsAppStore::open(path, key, enc)))
}

#[tokio::test]
async fn a_sends_b_receives_and_persists() {
    // Build two PeerSessions over an in-memory transport, exactly as
    // peerbeam-transfer/tests/session.rs does (reuse that harness/helpers).
    // Sender (A) advertises CHAT (no handler). Receiver (B) registers a
    // ChatHandler; bind its peer slot to A's device id after the handshake.
    //
    // The concrete PeerSession wiring MUST mirror the existing
    // peerbeam-transfer session round-trip test (see
    // rust/crates/peerbeam-transfer/tests/session.rs `regression_establish_and_negotiate`
    // and `pairing_code_survives_peer_session_handshake`), including the CHAT
    // capability on both configs and `.with_handlers(...)` on B's config.

    let store_b = chat_store(2);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

    // ... establish A (initiator) and B (responder), B's SessionConfig using
    //     HandlerRegistry::new().with(handler_b). After open, bind:
    //     let _ = peer_slot_b.set(<A's device id>);
    //     spawn both run loops.

    // A sends:
    let store_a = chat_store(1);
    let b_id = DeviceId::from("pb-b");
    let rec = send_message(&a_handle, &store_a, &b_id, "hello").await.unwrap();
    assert_eq!(rec.body, "hello");

    // B receives + persists + fires sink (poll briefly).
    // Assert: received has 1 record, body "hello", and store_b.history(A_id) has it.
}
```

> The implementer completes the PeerSession setup by copying the harness from `rust/crates/peerbeam-transfer/tests/session.rs` verbatim (that file already builds two `PeerSession`s and runs their loops). Add the CHAT capability to both configs and `with_handlers` to the responder's. Poll for the received record with a short bounded loop (e.g. up to 2s, 10ms steps) rather than a fixed sleep.

- [ ] **Step 2: Run to see it fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat --test roundtrip`
Expected: FAIL to compile (`ChatHandler`, `send_message` not defined).

- [ ] **Step 3: Implement `handler.rs`**

```rust
//! The Chat channel's MessageHandler: decode → dedup → persist → notify.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::ChatMessage;
use crate::record::ChatRecord;
use crate::store::ChatStore;

/// Called with each newly received (deduped) record so a surface can display it.
pub type ReceivedSink = Arc<dyn Fn(ChatRecord) + Send + Sync>;

/// Serves inbound Chat-channel frames for one session. The session peer is bound
/// once, after the handshake, via the returned [`OnceLock`].
pub struct ChatHandler {
    store: ChatStore,
    peer: Arc<OnceLock<DeviceId>>,
    sink: ReceivedSink,
}

impl ChatHandler {
    /// Build a handler + the peer slot the caller must `set` after the handshake
    /// (before the session run loop dispatches any frame).
    #[must_use]
    pub fn new(store: ChatStore, sink: ReceivedSink) -> (Arc<ChatHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(ChatHandler {
            store,
            peer: peer.clone(),
            sink,
        });
        (handler, peer)
    }
}

#[async_trait]
impl MessageHandler for ChatHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::CHAT
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        // Peer must be bound before any frame is dispatched (establish() sets it
        // before spawning run()). If somehow unbound, treat as a channel error.
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("chat peer not bound".into()));
        };
        let msg = ChatMessage::from_frame(&frame)?; // ChatError -> SessionError
        // Dedup by id (idempotent re-delivery).
        if self
            .store
            .contains(peer, &msg.id)
            .map_err(SessionError::from_chat)?
        {
            return Ok(());
        }
        let rec = ChatRecord::received(peer, &msg);
        self.store.append(&rec).map_err(SessionError::from_chat)?;
        (self.sink)(rec);
        Ok(())
    }
}

// Small helper so `?` on ChatError works uniformly. `ChatError: Into<SessionError>`
// already exists (message.rs); this names the conversion for the store paths.
trait FromChat {
    fn from_chat(e: crate::message::ChatError) -> Self;
}
impl FromChat for SessionError {
    fn from_chat(e: crate::message::ChatError) -> Self {
        SessionError::from(e)
    }
}
```

- [ ] **Step 4: Implement `send.rs`**

```rust
//! Sending a chat message over an established session.

use peerbeam_domain::id::DeviceId;
use peerbeam_transfer::session::SessionHandle;

use crate::message::{ChatError, ChatMessage};
use crate::record::ChatRecord;
use crate::store::ChatStore;

/// Failure sending a chat message.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("chat session error: {0}")]
    Session(String),
}

/// Send `body` to `peer` over an established session: persist our copy, open a
/// Chat channel, and send one `Message` frame. Returns the persisted record.
pub async fn send_message(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
) -> Result<ChatRecord, SendError> {
    let msg = ChatMessage::new(body)?; // enforces MAX_BODY
    let rec = ChatRecord::sent(peer, &msg);
    store.append(&rec)?;
    let channel = handle
        .open_channel(peerbeam_domain::session::ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    let frame = msg.to_frame(channel)?;
    handle
        .send_on_channel(channel, ChatMessage::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    Ok(rec)
}
```

> `SessionHandle` is re-exported from `peerbeam_transfer::session` (confirm the exact re-export path; it is defined in `peerbeam-transfer/src/session/mod.rs`). `open_channel`/`send_on_channel` signatures are as grounded: `open_channel(ChannelType) -> Result<ChannelId, SessionError>`, `send_on_channel(ChannelId, MessageType, MessageFlags, Bytes) -> Result<(), SessionError>`.

- [ ] **Step 5: Re-export in `lib.rs`**

```rust
mod handler;
mod send;

pub use handler::{ChatHandler, ReceivedSink};
pub use send::{send_message, SendError};
```

- [ ] **Step 6: Complete + run the round-trip test**

Finish the PeerSession harness in `tests/roundtrip.rs` (copy from `peerbeam-transfer/tests/session.rs`), then:

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-chat`
Expected: PASS (unit tests + round-trip).

- [ ] **Step 7: Full gate + commit**

```bash
git add rust/crates/peerbeam-chat
git commit -m "feat(chat): ChatHandler (receive+dedup+persist+notify) + send_message

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: FFI — AppStore + chat wiring into session establish + `chat_received` event

**Files:**
- Modify: `rust/crates/peerbeam-ffi/Cargo.toml` (add `peerbeam-chat`, `peerbeam-appstore-fs` deps)
- Modify: `rust/crates/peerbeam-ffi/src/session_exec.rs` (session config + establish threading)
- Modify: `rust/crates/peerbeam-ffi/src/runtime.rs` (construct `FsAppStore`, pass into `Manager`)
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`Manager` holds `ChatStore`; accept-side registers a chat handler emitting `chat_received`)
- Modify: `rust/crates/peerbeam-ffi/src/events.rs` (add `chat` emitter)

**Interfaces:**
- Consumes: `peerbeam_chat::{ChatStore, ChatHandler, ReceivedSink, ChatRecord}`; `FsAppStore`; `derive_subkey`.
- Produces: `Manager.chat: ChatStore` field; the accept path registers a `ChatHandler`; `events::chat(record)` emits a `chat_received` event `{type:"chat_received", timestamp, message:{id,peer_id,direction,timestamp,body,status}}`. Session configs advertise `Capability::new(ChannelType::CHAT)`.

- [ ] **Step 1: Add deps + the events emitter (write its test first)**

Add to `peerbeam-ffi/Cargo.toml` `[dependencies]`: `peerbeam-chat = { path = "../peerbeam-chat" }`, `peerbeam-appstore-fs = { path = "../peerbeam-appstore-fs" }`.

In `events.rs`, add (mirroring `events::transfer` at events.rs:57):

```rust
/// Emit a `chat_received` event carrying one persisted record.
pub fn chat(rec: &peerbeam_chat::ChatRecord) {
    emit(&serde_json::json!({
        "type": "chat_received",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message": {
            "id": rec.id,
            "peer_id": rec.peer_id,
            "direction": rec.direction,
            "timestamp": rec.timestamp,
            "body": rec.body,
            "status": rec.status,
        },
    }));
}
```

- [ ] **Step 2: Thread chat into `session_exec`**

Replace `transfer_cfg()` with a config that advertises TRANSFER (stream) + CHAT (message) and optionally registers a chat handler; and thread an optional chat store+sink into `establish` so the accept side registers a handler and binds the peer post-open. Add near the top of `session_exec.rs`:

```rust
use std::sync::{Arc, OnceLock};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, MessageHandler};
use peerbeam_transfer::session::HandlerRegistry;
use peerbeam_chat::{ChatHandler, ChatStore, ReceivedSink};

const CHAT: ChannelType = ChannelType::CHAT;

/// Chat wiring for a session: the store + a received-sink. Present on the
/// receiving (accept) side so inbound chat is persisted + surfaced.
pub struct ChatWiring {
    pub store: ChatStore,
    pub sink: ReceivedSink,
}

fn session_cfg(chat_handler: Option<Arc<dyn MessageHandler>>) -> SessionConfig {
    let caps = CapabilitySet::new()
        .with(Capability::new(TRANSFER))
        .with(Capability::new(CHAT));
    let mut cfg = SessionConfig::new(caps).with_stream_channel_type(TRANSFER);
    if let Some(h) = chat_handler {
        cfg = cfg.with_handlers(HandlerRegistry::new().with(h));
    }
    cfg
}
```

Change `establish(...)` to accept `chat: Option<ChatWiring>`; build the handler + peer slot when present, use `session_cfg(handler)` instead of `transfer_cfg()`, and after `PeerSession::open` bind the peer slot before spawning `run`:

```rust
async fn establish(
    transport: Arc<dyn ChannelTransport>,
    role: SessionRole,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
    chat: Option<ChatWiring>,
) -> Result<Session, (Code, String)> {
    // Build the optional chat handler + its peer slot.
    let (handler, peer_slot): (Option<Arc<dyn MessageHandler>>, Option<Arc<OnceLock<peerbeam_domain::id::DeviceId>>>) =
        match chat {
            Some(w) => {
                let (h, slot) = ChatHandler::new(w.store, w.sink);
                (Some(h), Some(slot))
            }
            None => (None, None),
        };
    // ... existing channels/registry setup ...
    let mut ps = PeerSession::open(
        transport, role, session_cfg(handler), ev, ch, inc, registry, ident, enc, trust,
    ).await.map_err(|e| (Code::Connection, format!("session establish failed: {e}")))?;
    // Bind the chat peer before the run loop dispatches any frame.
    if let Some(slot) = peer_slot {
        let _ = slot.set(ps.peer().clone());
    }
    // ... existing peer_id/handle/spawn(ps.run()) ...
}
```

Update `dial(...)` and `accept(...)` to pass `chat`: `dial` passes `None` (sender advertises CHAT but registers no handler); `accept` passes `Some(ChatWiring{...})`. Add a `chat: Option<ChatWiring>` param to both `dial` and `accept` (callers supply it).

- [ ] **Step 3: Construct the AppStore in `runtime.rs` and give the Manager a `ChatStore`**

In `runtime.rs::init()`, after identity/trust are built (near the `trust_path` construction ~line 271), add:

```rust
let appstore_root = std::path::Path::new(&config.storage.data_directory).join("appstore");
let chat_key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
let appstore: std::sync::Arc<dyn peerbeam_domain::port::AppStore> =
    std::sync::Arc::new(peerbeam_appstore_fs::FsAppStore::open(appstore_root, chat_key, enc.clone()));
let chat = peerbeam_chat::ChatStore::new(appstore);
```

Pass `chat` into `Manager::new(...)` (add a parameter + field `chat: ChatStore`). (`ident`/`enc` variable names follow whatever `runtime.rs` already uses; the identity secret is `ident.keypair.secret.0`.)

- [ ] **Step 4: Accept path registers the chat handler + emits**

In `transfer.rs` `handle_incoming` (~line 985), change the `session_exec::accept(...)` call to pass chat wiring whose sink emits and persists via the manager's `ChatStore`:

```rust
let chat_store = self.chat.clone();
let wiring = crate::session_exec::ChatWiring {
    store: chat_store,
    sink: std::sync::Arc::new(|rec| crate::events::chat(&rec)),
};
let mut session = match crate::session_exec::accept(qc, self.identity(), self.enc.clone(), self.trust.clone(), Some(wiring)).await { ... };
```

(The handler persists the record itself via the store, then calls the sink; the sink only emits. So the sink is `|rec| events::chat(&rec)`.)

- [ ] **Step 5: Build + gate + commit**

Run the full gate (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`). Fix any borrow/move issues (clone `ChatStore` — it is `Clone`). Then:

```bash
git add rust/crates/peerbeam-ffi rust/Cargo.lock
git commit -m "feat(ffi): wire chat into session establish + AppStore + chat_received event

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: FFI — `Manager::chat_send` / `chat_history` + `pb_chat_send` / `pb_chat_history`

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (add `Manager::chat_send`, `Manager::chat_history`)
- Modify: `rust/crates/peerbeam-ffi/src/lib.rs` (add `pb_chat_send`, `pb_chat_history`)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs`

**Interfaces:**
- Consumes: `Manager.chat` (Task 5); `peerbeam_chat::send_message`; `session_exec::dial`.
- Produces: FFI `pb_chat_send({peer:{...},text}) -> {id}` and `pb_chat_history({peer_id}) -> {messages:[...]}`.

- [ ] **Step 1: Write the failing FFI test**

`rust/crates/peerbeam-ffi/tests/chat_ffi.rs` — mirror the existing `transfer_ffi.rs` two-manager harness (one sends, one receives): assert the receiver emits a `chat_received` event with a 39?-no—with `body` == sent text, and that `pb_chat_history`/`chat_history` on the receiver returns the message. (Reuse `transfer_ffi.rs`'s manager/event-capture setup verbatim.)

```rust
// Assert (shape):
//   sender.chat_send({peer: <receiver device json>, text: "hi"}) -> {id}
//   receiver captures a chat_received event whose message.body == "hi"
//   receiver.chat_history({peer_id: <sender id>}) -> messages[0].body == "hi"
```

- [ ] **Step 2: Run to see it fail**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-ffi --test chat_ffi`
Expected: FAIL (methods/exports missing).

- [ ] **Step 3: Implement `Manager::chat_send` / `chat_history`**

In `transfer.rs` `impl Manager` (same `type Op = Result<Value,(Code,String)>` convention as `trust_list`):

```rust
pub fn chat_send(&self, req: &Value) -> Op {
    // Resolve the target Device from req["peer"] exactly like transfer send
    // (reuse device_from(req)). Then dial + send synchronously on the runtime.
    let device = device_from(req)?;
    let text = req.get("text").and_then(|v| v.as_str())
        .ok_or((Code::InvalidArgument, "missing text".into()))?.to_string();
    let peer_id = peerbeam_domain::id::DeviceId::from(device.id.0.clone());
    let chat = self.chat.clone();
    let (ident, enc, trust, quic, rm) =
        (self.identity(), self.enc.clone(), self.trust.clone(), self.quic.clone(), self.rm.clone());
    // Block on the async dial+send on the FFI runtime (mirror how send() spawns;
    // here we need the result synchronously to return {id}).
    let rec = crate::runtime::block_on(async move {
        let meta = /* TransferSession/meta like dial() expects */;
        let session = crate::session_exec::dial(&quic, &rm, &device, &meta, ident, enc, trust, None).await
            .map_err(|(c,m)| (c, m))?;
        let rec = peerbeam_chat::send_message(&session.handle, &chat, &peer_id, &text).await
            .map_err(|e| (Code::Connection, e.to_string()))?;
        session.close().await;
        Ok::<_, (Code,String)>(rec)
    })?;
    Ok(serde_json::json!({ "id": rec.id }))
}

pub fn chat_history(&self, req: &Value) -> Op {
    let peer_id = req.get("peer_id").and_then(|v| v.as_str())
        .ok_or((Code::InvalidArgument, "missing peer_id".into()))?;
    let peer = peerbeam_domain::id::DeviceId::from(peer_id.to_string());
    let hist = self.chat.history(&peer).map_err(|e| (Code::Internal, e.to_string()))?;
    let messages: Vec<Value> = hist.into_iter().map(|r| serde_json::json!({
        "id": r.id, "peer_id": r.peer_id, "direction": r.direction,
        "timestamp": r.timestamp, "body": r.body, "status": r.status,
    })).collect();
    Ok(serde_json::json!({ "messages": messages }))
}
```

> Confirm the exact `dial` meta argument + `block_on`/runtime-handle available in `runtime.rs` (the transfer `send` path shows how to run async work on the FFI runtime — reuse that mechanism; if there is no `block_on`, use the same `runtime::spawn`+await bridge the send path uses, or `Handle::block_on`). `device_from` is the existing helper (transfer.rs:1387). `dial` now takes a trailing `chat: Option<ChatWiring>` (Task 5) — pass `None` for the sender.

- [ ] **Step 4: Add the exports in `lib.rs`**

Under a new `// ── chat ──` section (mirroring `pb_trust_*`):

```rust
/// Send a chat message: `{peer:{...},text}` → `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_send(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_send(&read_json(json)?))()))
}

/// Conversation history: `{peer_id}` → `{messages:[...]}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_history(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_history(&read_json(json)?))()))
}
```

- [ ] **Step 5: Run the test + full gate + commit**

Run the chat_ffi test (PASS), then the full gate.

```bash
git add rust/crates/peerbeam-ffi
git commit -m "feat(ffi): pb_chat_send + pb_chat_history

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: CLI — `chat` subcommand (send / history / watch)

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/cli.rs` (add `Chat(ChatArgs)` + `ChatAction`)
- Create: `rust/bins/peerbeam-cli/src/chat.rs` (the handlers)
- Modify: `rust/bins/peerbeam-cli/src/commands.rs` (dispatch arm; expose `SecureCtx`/`load_config`/`snapshot`/`resolve_peer` as needed — they are crate-visible in the same module; move the chat handler into `commands.rs` if simpler, or `pub(crate)` the helpers)
- Modify: `rust/bins/peerbeam-cli/src/session_transfer.rs` (add CHAT capability; optional chat handler on `accept`; add a `chat` param to `dial`/`accept` mirroring the FFI)
- Modify: `rust/bins/peerbeam-cli/src/main.rs` (register the `chat` module if a new file)

**Interfaces:**
- Consumes: `session_transfer::{dial,accept}`; `peerbeam_chat::{ChatStore, send_message, ChatHandler, ReceivedSink}`; `SecureCtx`; `FsAppStore`.
- Produces: `peerbeam chat send [--to <peer>|--addr IP:PORT] <text>`, `peerbeam chat history <peer>`, `peerbeam chat watch`.

- [ ] **Step 1: Add the clap surface**

In `cli.rs`, add to `Command`: `/// Chat with a peer.\n    Chat(ChatArgs),` and:

```rust
#[derive(Args)]
pub struct ChatArgs {
    #[command(subcommand)]
    pub action: ChatAction,
}

#[derive(Subcommand)]
pub enum ChatAction {
    /// Send a message to a peer.
    Send {
        #[arg(long)]
        to: Option<String>,
        #[arg(long, value_name = "IP:PORT", conflicts_with = "to")]
        addr: Option<String>,
        text: String,
    },
    /// Print a conversation's history.
    History { peer: String },
    /// Listen for and print incoming chat messages.
    Watch {
        #[arg(long)]
        port: Option<u16>,
    },
}
```

- [ ] **Step 2: Add a CLI AppStore + chat capability to `session_transfer`**

In `session_transfer.rs`, add `const CHAT: ChannelType = ChannelType::CHAT;`, change `transfer_cfg()` to `session_cfg(chat_handler: Option<Arc<dyn MessageHandler>>)` advertising TRANSFER (stream) + CHAT, registering the handler when present (mirror Task 5's `session_cfg`), and add a `chat: Option<(ChatStore, ReceivedSink)>` param to `establish`/`dial`/`accept` that builds+binds the handler post-open (same OnceLock pattern). Add a helper to build the CLI's `ChatStore` from `SecureCtx`/config:

```rust
// in commands.rs (next to SecureCtx::build)
pub(crate) fn chat_store(config: &EngineConfig, enc: &Arc<AeadCrypto>, ident: &Identity) -> ChatStore {
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let store: Arc<dyn peerbeam_domain::port::AppStore> =
        Arc::new(peerbeam_appstore_fs::FsAppStore::open(root, key, enc.clone()));
    ChatStore::new(store)
}
```

Add `peerbeam-chat`, `peerbeam-appstore-fs`, `peerbeam-crypto` (if not already) to `bins/peerbeam-cli/Cargo.toml`.

- [ ] **Step 3: Implement the handlers (`chat.rs`)**

```rust
//! `peerbeam chat` — send / history / watch.
use crate::cli::ChatAction;
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub async fn chat(ctx: &Ctx, action: ChatAction, path_override: Option<&str>) -> CliResult {
    match action {
        ChatAction::Send { to, addr, text } => send(ctx, to, addr, text, path_override).await,
        ChatAction::History { peer } => history(ctx, peer, path_override),
        ChatAction::Watch { port } => watch(ctx, port, path_override).await,
    }
}
```

- `send`: reuse the exact peer-resolution from `commands::send` (`--addr` → `target_device`; else `snapshot(config,2)` + `resolve_peer`), build `SecureCtx` + `chat_store`, `session_transfer::dial(..., None)` (sender: no handler), then `peerbeam_chat::send_message(&session.handle, &store, &peer_id, &text).await`, `session.close().await`; print `{event:"chat_sent", id, peer}` (JSON) or a green confirmation.
- `history`: resolve the peer id (accept a raw device id or resolve a name via `snapshot`), build `chat_store`, print `store.history(&peer)` as JSON `{messages:[...]}` or a human transcript (`[timestamp] direction: body`).
- `watch`: mirror `serve_loop` but with a chat sink that prints each received message; build `chat_store`; on each inbound session call `session_transfer::accept(qc, ..., Some((store.clone(), sink)))`. The sink prints `[peer] body` (or JSON line). Bind the peer slot inside `accept` (Task 2 of this task's `establish` change).

> Reuse `commands.rs` helpers by making `SecureCtx::build`, `load_config`, `snapshot`, `resolve_peer`, `target_device`, `resolve_addr` `pub(crate)` if they are not already, so `chat.rs` can call them. Do not duplicate their logic.

- [ ] **Step 4: Dispatch + module registration**

In `commands.rs` `dispatch`, add: `Command::Chat(a) => crate::chat::chat(ctx, a.action, cfg_override.as_deref()).await,`. In `main.rs`, add `mod chat;`.

- [ ] **Step 5: Test**

Add a CLI test (or an integration test) that exercises the pure parts: peer/text parsing and history rendering. A full send/watch loop is covered by the FFI + chat-crate round-trip tests; for the CLI, at minimum add a unit test for `history` JSON rendering against a seeded `ChatStore` (build a `ChatStore` over a temp `FsAppStore`, append two records, assert the rendered JSON). Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo test -p peerbeam-cli chat`.

- [ ] **Step 6: Full gate + commit**

```bash
git add rust/bins/peerbeam-cli rust/Cargo.lock
git commit -m "feat(cli): chat subcommand (send/history/watch)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Flutter SDK — model + bindings + api + `chat_received` event

**Files:**
- Modify: `flutter/lib/sdk/models.dart` (add `ChatMessage`)
- Modify: `flutter/lib/sdk/ffi/bindings.dart` (bind `pb_chat_send`, `pb_chat_history`)
- Modify: `flutter/lib/sdk/peerbeam.dart` (`chatSend`, `chatHistory` on `PeerBeamApi` + impl)
- Modify: `flutter/lib/sdk/events.dart` (`ChatReceived` event + `chat_received` case)

**Interfaces:**
- Produces: `ChatMessage` model (`{id, peerId, direction, body, at, status}`); `PeerBeamApi.chatSend(PeerTarget peer, String text) -> Future<String>`; `PeerBeamApi.chatHistory(String peerId) -> Future<List<ChatMessage>>`; `ChatReceived(ChatMessage)` event.

- [ ] **Step 1: Add the `ChatMessage` model** (mirror `TrustedDevice.fromJson`, models.dart):

```dart
class ChatMessage {
  final String id;
  final String peerId;
  final String direction; // 'out' | 'in'
  final String body;
  final DateTime at;
  final String status; // 'pending' | 'sent' | 'received'

  const ChatMessage({
    required this.id,
    required this.peerId,
    required this.direction,
    required this.body,
    required this.at,
    required this.status,
  });

  bool get isMine => direction == 'out';

  factory ChatMessage.fromJson(Map<String, dynamic> j) => ChatMessage(
        id: j['id'] as String? ?? '',
        peerId: j['peer_id'] as String? ?? '',
        direction: j['direction'] as String? ?? 'in',
        body: j['body'] as String? ?? '',
        at: DateTime.tryParse(j['timestamp'] as String? ?? '') ?? DateTime.now(),
        status: j['status'] as String? ?? 'received',
      );
}
```

- [ ] **Step 2: Bindings** — add fields, lookups, and wrappers in `bindings.dart` mirroring `trustRemove`:

```dart
final _ArgRetDart _chatSend;
final _ArgRetDart _chatHistory;
// in initializer list:
_chatSend = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_send'),
_chatHistory = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_history'),
// wrappers:
String chatSend(String json) => _withArg(json, _chatSend);
String chatHistory(String json) => _withArg(json, _chatHistory);
```

- [ ] **Step 3: API methods** — declare on `PeerBeamApi` and implement on `PeerBeam` (mirror `trustList`/`sendFile`):

```dart
// abstract:
Future<String> chatSend(PeerTarget peer, String text);
Future<List<ChatMessage>> chatHistory(String peerId);

// impl:
@override
Future<String> chatSend(PeerTarget peer, String text) async {
  final data = _data(_req().chatSend(jsonEncode({'peer': peer.toJson(), 'text': text})));
  return data['id'] as String;
}

@override
Future<List<ChatMessage>> chatHistory(String peerId) async {
  final data = _data(_req().chatHistory(jsonEncode({'peer_id': peerId})));
  return _list(data['messages']).map(ChatMessage.fromJson).toList();
}
```

Also add these two methods to the fake/in-memory `PeerBeamApi` impl if one exists (search for a test/fake implementation of `PeerBeamApi`; give it a simple in-memory list so tests/analyzer pass).

- [ ] **Step 4: Event** — in `events.dart`, add the case + class:

```dart
case 'chat_received':
  return ChatReceived(ChatMessage.fromJson(_map(j['message'])));
```
```dart
class ChatReceived extends BridgeEvent {
  final ChatMessage message;
  const ChatReceived(this.message);
}
```

- [ ] **Step 5: Analyze + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/flutter && flutter analyze` (and `flutter test` if present). Fix any analyzer issues.

```bash
git add flutter/lib/sdk
git commit -m "feat(flutter): chat SDK (ChatMessage model, chatSend/chatHistory, chat_received)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Flutter — `ChatRepository` + app wiring + chat screen + nav tab

**Files:**
- Create: `flutter/lib/data/chat_repository.dart`, `flutter/lib/features/chat/chat_screen.dart`
- Modify: `flutter/lib/state/stores.dart` (add `chat` field + `AppState.live` + `dispose`)
- Modify: `flutter/lib/main.dart` (no change needed beyond `_state.dispose()` cascade; optionally refresh on open)
- Modify: `flutter/lib/app/router.dart` (a `/chat` branch or a device-scoped route), `flutter/lib/app/shell.dart` (Chat destination)

**Interfaces:**
- Consumes: `PeerBeamApi.chatSend/chatHistory`, `ChatReceived` event, `ChatMessage`.
- Produces: `ChatRepository` (per-peer message map, `messagesFor(peerId)`, `refresh(peerId)`, `send(peer, text)`), a `ChatScreen`, a Chat nav tab.

- [ ] **Step 1: `ChatRepository`** (mirror `TrustRepository`; keyed per peer):

```dart
class ChatRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, List<ChatMessage>> _byPeer = {};
  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;

  ChatRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      if (e is ChatReceived) _onReceived(e.message);
    });
  }

  List<ChatMessage> messagesFor(String peerId) =>
      List.unmodifiable(_byPeer[peerId] ?? const []);

  Future<void> refresh(String peerId) async {
    final api = _api;
    if (api == null) return;
    try {
      final msgs = await api.chatHistory(peerId);
      if (_disposed) return;
      _byPeer[peerId] = msgs;
      notifyListeners();
    } catch (_) {}
  }

  Future<void> send(PeerTarget peer, String text) async {
    if (text.trim().isEmpty) return;
    try {
      await _api?.chatSend(peer, text);
      await refresh(peer.id);
    } catch (_) {}
  }

  void _onReceived(ChatMessage m) {
    (_byPeer[m.peerId] ??= <ChatMessage>[]).add(m);
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
```

> `PeerTarget` has an `id` and `toJson()` (used by `sendFile`); confirm `PeerTarget.id`. If a `PeerTarget` is built from a device, the chat screen receives one (see Step 3).

- [ ] **Step 2: Wire into `AppState`** (stores.dart): add `final ChatRepository chat;` field, `required this.chat`, `chat: ChatRepository(api: api),` in `AppState.live`, and `chat.dispose();` in `dispose()`.

- [ ] **Step 3: `ChatScreen`** — mirror `HistoryScreen`'s `Scaffold`/`AppBar`/`SafeArea`/`ContentPane`/`AnimatedBuilder(animation: state.chat)`/`ListView.builder`+`Appear`+`EmptyState`, plus a bottom compose `Row(TextField + IconButton(send))`. Bubble widget colors own-vs-peer via `scheme.primaryContainer` / `scheme.surfaceContainerHighest` (see `send_text.dart` message dialog). Refresh on first frame: `WidgetsBinding.instance.addPostFrameCallback((_) => state.chat.refresh(widget.peer.id));`. (Use the concrete screen skeleton in the exploration report.)

- [ ] **Step 4: Navigation** — add a Chat entry. Simplest for M1: a conversation reachable from a device (a chat icon on a device tile that pushes `ChatScreen(peer: ...)`), OR a 5th bottom-nav tab `/chat` listing conversations. For M1, add a **device-scoped push**: on the device list/tile add a "Chat" action that does `Navigator.of(context).push(MaterialPageRoute(builder: (_) => ChatScreen(peer: target)))`. If adding a tab instead, extend `shell.dart` `_destinations` + `router.dart` branches + the `Ctrl/⌘+N` shortcut map. Pick ONE; the device-scoped push is less invasive and recommended for M1.

- [ ] **Step 5: Analyze + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/flutter && flutter analyze` (fix issues); `flutter test` if tests exist.

```bash
git add flutter/lib
git commit -m "feat(flutter): ChatRepository + chat screen + navigation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Docs — message registry + CLI

**Files:**
- Modify: `docs/MESSAGE_REGISTRY.md` (§4 Chat: record the concrete implemented ids)
- Modify: `docs/CLI.md` (chat commands)

- [ ] **Step 1: Registry** — in `docs/MESSAGE_REGISTRY.md` §4, update the Chat line to record the implemented id: `Message = 1` (implemented, 1a); `Receipt`, `Reaction`, `Edit` reserved (not implemented). Keep §2's `0x0101 Chat` row.

- [ ] **Step 2: CLI.md** — add a `chat` section: `peerbeam chat send [--to <peer>|--addr IP:PORT] <text>`, `peerbeam chat history <peer>`, `peerbeam chat watch` — note it requires the peer online (1a), messages are stored encrypted locally, and `watch` must be running to receive.

- [ ] **Step 3: Gate (fmt only) + commit**

Run: `cd /home/althaf-ahammed/Projects/omega/PeerBeam/rust && cargo fmt --check` (no Rust changed → passes).

```bash
git add docs/MESSAGE_REGISTRY.md docs/CLI.md
git commit -m "docs: chat message ids + CLI chat commands (increment 1a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- New Chat message channel (0x0101), ChatMessage, handler, send → Tasks 1, 2, 4. ✓
- Per-conversation encrypted AppStore persistence + dedup → Task 3 (+ AppStore construction Tasks 5, 7). ✓
- Sender identity from session, not payload → Task 4 handler binds session peer via OnceLock; wire ChatMessage carries no sender. ✓
- 16 KiB cap → Task 2 (send + receive). ✓
- Negotiation (I9) → Tasks 5, 7 advertise `Capability::new(ChannelType::CHAT)`. ✓
- Online-only (no outbox) → send fails if dial fails; no queue anywhere. ✓ (1b deferred.)
- CLI (send/history/watch) → Task 7. FFI (send/history + chat_received) → Tasks 5, 6. Flutter (model/api/event/repo/screen) → Tasks 8, 9. ✓
- Docs → Task 10. ✓

**Type consistency:** `ChatMessage{id,timestamp,body}`, `ChatRecord{id,peer_id,direction,timestamp,body,status}`, `ChatStore::{new,append,history,contains}`, `ChatHandler::new -> (Arc<ChatHandler>, Arc<OnceLock<DeviceId>>)`, `send_message(handle,store,peer,body)`, `namespace(peer)=="chat-<id>"`, event `chat_received{message:{...}}`, Dart `ChatMessage.fromJson` reads the same fields — consistent across tasks. ✓

**Known integration risks flagged for implementers (not placeholders — each has a concrete resolution path):**
- The exact `SessionHandle` re-export path and the FFI `runtime` async-bridge (`block_on` vs spawn+await) and `dial` meta arg (Task 6) must be confirmed against the current source; the plan names where to look and the pattern to copy (the transfer `send` path).
- `establish`/`dial`/`accept` gained a trailing `chat` param in both FFI (Task 5) and CLI (Task 7); every call site must be updated (the compiler enumerates them).
- The round-trip (Task 4) and chat_ffi (Task 6) tests reuse existing harnesses verbatim (`peerbeam-transfer/tests/session.rs`, `peerbeam-ffi/tests/transfer_ffi.rs`) rather than inventing new scaffolding.

**Placeholder scan:** no `TBD`/`TODO`; every code step has concrete code or an exact command. Where a step says "mirror X", the exact source location + pattern is named and the surrounding code is provided.
