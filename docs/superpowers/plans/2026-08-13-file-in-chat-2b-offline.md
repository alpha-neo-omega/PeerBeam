# File-in-chat 2b — Offline Queueing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attaching a file to an offline peer queues and delivers later, instead of failing.

**Architecture:** The picked file is stream-copied into outbox-owned storage (so the queue owns bytes nobody can move or edit), an `OutboxEntry` gains optional file fields, and the existing drain delivers files alongside text with a one-file-in-flight guard. A new `FileDecline` chat message makes a refusal terminal, and a new `AppStore::namespaces` enumeration makes a thread reachable for a peer discovery cannot see.

**Tech Stack:** Rust (peerbeam-domain / -chat / -ffi / -platform / -config, peerbeam-cli), Flutter/Dart.

**Spec:** `docs/superpowers/specs/2026-08-13-file-in-chat-2b-design.md`

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in library (crate) code; tests may unwrap; the `pb_*` `unsafe extern "C"` exports and the existing `.lock().unwrap()` idiom in `transfer.rs` are the established, accepted patterns; the CLI binary may use `expect` ONLY where its own file already does.
- Every schema/wire addition MUST be additive and backward-compatible with already-persisted 1a/1b/2a records and already-released peers. New fields `#[serde(default)]`; new wire types ship `OPTIONAL`; a peer advertising `features: 0` is NEVER sent a `FileRef` and never silently downgraded (I9).
- I10: no whole file buffered — staging streams in and out; `AppStore` must never hold a blob (its `list()` returns every value in a namespace).
- I6: the approval gate's DECISION logic is unchanged; queueing changes WHEN bytes are offered, never WHETHER consent is asked.
- I2: reuse `StorageProvider`, the existing outbox, the existing approval path and transfer engine — no parallel systems. The settle guard has exactly ONE implementation (`ChatRecord::is_settleable_file_row`); do not fork it.
- Gate per task from `rust/`: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Flutter tasks additionally from `flutter/`: `flutter analyze && flutter test`.
- Regression nets that MUST pass unchanged: `peerbeam-ffi tests/transfer_ffi.rs`, `peerbeam-cli tests/transfer_e2e.rs`, `peerbeam-ffi tests/chat_ffi.rs`, `peerbeam-chat tests/roundtrip.rs`. Load-bearing for Task 6 specifically, because it re-routes 2a's proven online path through the queue.
- Adding a new `pb_*` export breaks all of `flutter/test/sdk/ffi_test.dart` until `cargo build -p peerbeam-ffi` is re-run — `Bindings.load` resolves symbols EAGERLY against the prebuilt cdylib. Any task adding a `pb_*` MUST rebuild before trusting the Flutter suite.
- Test-env: `peerbeam-ffi tests/chat_ffi.rs::chat_drain_delivers_queued_message_once_peer_comes_online` flakes ONLY when another PeerBeam process holds UDP 49500; check `pgrep -af peerbeam` first. NEVER run `pkill -f peerbeam` (the pattern self-matches and kills the shell); use exact PIDs or `pkill -x`. The user may have the GUI running — do not kill it. If the disk fills: `rm -rf rust/target/debug/incremental` and retry.
- Commit per task with trailer exactly `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`; local commits only, push held.
- Do NOT modify constitutional docs (`ROADMAP.md`, `docs/VISION.md`, `docs/FUTURE_ARCHITECTURE.md`, `docs/ARCHITECTURAL_INVARIANTS.md`). `docs/MESSAGE_REGISTRY.md` and `docs/FEATURE_CATALOG.md` are derived and editable.

## Values fixed by the spec (use verbatim)

| thing | value |
| --- | --- |
| chat `MessageType` for a decline | `MSG_FILE_DECLINE: u16 = 3` |
| decline feature bit | `CHAT_FEAT_FILEDECLINE: u32 = 1 << 1` |
| decline frame flags | `MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE)` |
| size cap default | `16 GiB` = `17_179_869_184` |
| free-space floor default | `512 MiB` = `536_870_912` |
| backstop | terminal at `offers_refused >= 3` |
| staged blob directory | `<data_directory>/outbox-blobs/` |
| queued row status | existing `Status::Pending` |
| new status | `Status::Staging` |

## File Structure

**Create:**
- `rust/crates/peerbeam-chat/src/staging.rs` — the staging store: stream-copy in, path for an id, delete, orphan sweep. One responsibility, so the copy loop and its bounds live in one testable place.

**Modify (Rust):**
- `rust/crates/peerbeam-domain/src/port/appstore.rs` — `namespaces(prefix)`.
- `rust/crates/peerbeam-appstore-fs/src/lib.rs` — the single `AppStore` impl.
- `rust/crates/peerbeam-platform/src/lib.rs` — `available_bytes`.
- `rust/crates/peerbeam-platform/Cargo.toml` — add `fs4`.
- `rust/Cargo.toml` — `fs4` in `[workspace.dependencies]`.
- `rust/crates/peerbeam-config/src/lib.rs` — two `DeviceConfig` fields.
- `rust/crates/peerbeam-chat/Cargo.toml` — add `futures`, `peerbeam-platform`.
- `rust/crates/peerbeam-chat/src/{lib,message,record,store,send,handler}.rs`.
- `rust/crates/peerbeam-ffi/src/{transfer,runtime,session_exec,lib}.rs`.
- `rust/bins/peerbeam-cli/src/{chat,cli,commands,session_transfer}.rs`.

**Modify (Flutter):** `lib/sdk/{models,peerbeam}.dart`, `lib/sdk/ffi/bindings.dart`, `lib/data/chat_repository.dart`, `lib/features/chat/chat_screen.dart`, `lib/features/home/home_screen.dart`, `test/sdk/fake_peerbeam.dart`.

**Modify (docs):** `docs/MESSAGE_REGISTRY.md`, `docs/CLI.md`.

---

### Task 1: Containment — one bad row must not poison a whole namespace

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (`history` at :79-90, `outbox_pending` at :138-148)
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing.
- Produces: `history` and `outbox_pending` keep their exact signatures — `history(&self, peer: &DeviceId) -> Result<Vec<ChatRecord>, ChatError>` and `outbox_pending(&self) -> Result<Vec<OutboxEntry>, ChatError>` — but skip an undecodable row instead of failing the call.

**Why this is first.** 2b adds `Status::Staging`, a variant a 2a binary cannot decode. Today `history` does `out.push(ChatRecord::decode(&value)?)` inside a loop, so the **first** unreadable row discards every already-decoded row and returns `Err` for the entire conversation. `outbox_pending` has the identical shape and is worse: `outbox_for` and `outbox_peers` both call it, so one bad entry silently disables **all** offline delivery for **every** peer. That is the same class as the `chat-outbox` namespace collision that already shipped once.

- [ ] **Step 1: Write the failing tests**

Add to `store.rs`'s test module:

```rust
    #[test]
    fn history_skips_an_undecodable_row_and_keeps_the_rest() {
        let (cs, store, _tmp) = new_store();
        let peer = DeviceId::from("pb-alice".to_string());
        let good = ChatRecord {
            id: "0000000000001".into(),
            peer_id: "pb-alice".into(),
            direction: Direction::Out,
            timestamp: "2026-08-13T10:00:00+00:00".into(),
            body: "before".into(),
            status: Status::Sent,
            kind: Kind::Text,
            file: None,
        };
        let later = ChatRecord {
            id: "0000000000003".into(),
            body: "after".into(),
            ..good.clone()
        };
        cs.append(&good).unwrap();
        // A row this build cannot read — exactly what a newer peer's schema
        // looks like to an older binary.
        store
            .put(&namespace(&peer), "0000000000002", b"{\"status\":\"from-the-future\"}")
            .unwrap();
        cs.append(&later).unwrap();

        let got = cs.history(&peer).expect("one bad row must not fail the conversation");
        let bodies: Vec<&str> = got.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(bodies, vec!["before", "after"]);
    }

    #[test]
    fn outbox_pending_skips_an_undecodable_entry_so_delivery_survives() {
        let (cs, store, _tmp) = new_store();
        let peer = DeviceId::from("pb-bob".to_string());
        cs.enqueue(&peer, &ChatMessage::new("real").unwrap()).unwrap();
        store.put(OUTBOX_NS, "0000000000009", b"not an outbox entry").unwrap();

        let pending = cs
            .outbox_pending()
            .expect("one bad entry must not disable the whole outbox");
        assert_eq!(pending.len(), 1, "the good entry still delivers");
        assert_eq!(pending[0].body, "real");
        // The cascade is the real damage: both of these read through
        // `outbox_pending`, so a poisoned outbox silently stops every peer.
        assert_eq!(cs.outbox_for(&peer).unwrap().len(), 1);
        assert_eq!(cs.outbox_peers().unwrap().len(), 1);
    }
```

`new_store()` is the existing helper in that module; if it returns only the `ChatStore`, extend it to also return the raw `Arc<dyn AppStore>` and the `TempDir` (the tests above need to `put` a hand-written bad value). Keep every existing call site compiling.

- [ ] **Step 2: Run to see them fail**

Run: `cd rust && cargo test -p peerbeam-chat history_skips_an_undecodable_row outbox_pending_skips`
Expected: FAIL — both return `Err(Serialization(..))` rather than the good rows.

- [ ] **Step 3: Make both loops tolerant**

In `history` (store.rs:79-90) replace the push loop:

```rust
        let mut out = Vec::with_capacity(raw.len());
        for (key, value) in raw {
            match ChatRecord::decode(&value) {
                Ok(rec) => out.push(rec),
                // A row this build cannot read — most likely written by a newer
                // version whose schema grew. Skipping it loses one row; failing
                // the call loses the entire conversation, including every row
                // this build understands perfectly well. Forward compatibility
                // is the whole point: 2b adds a `Status` variant that a 2a
                // binary hits exactly here.
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "skipping unreadable chat record");
                }
            }
        }
        Ok(out)
```

Apply the same shape in `outbox_pending` (store.rs:138-148), warning with `"skipping unreadable outbox entry"`.

`peerbeam-chat` has no `tracing` dependency today (see its `Cargo.toml`). Add `tracing = { workspace = true }` to `[dependencies]` — the workspace already pins it and every sibling crate uses it. If a reviewer objects to a new dependency for a log line, drop the `tracing::warn!` and skip silently with the comment intact; do not add a `println!`.

- [ ] **Step 4: Run to verify they pass**

Run: `cd rust && cargo test -p peerbeam-chat`
Expected: PASS, including the pre-existing store tests unchanged.

- [ ] **Step 5: Full gate and commit**

```bash
cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-chat
git commit -m "fix(chat): an unreadable row no longer blanks the conversation around it

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `AppStore::namespaces` — enumerate conversations

**Files:**
- Modify: `rust/crates/peerbeam-domain/src/port/appstore.rs`
- Modify: `rust/crates/peerbeam-appstore-fs/src/lib.rs` (the `impl AppStore` block at :67-150)
- Test: `rust/crates/peerbeam-appstore-fs`'s test module

**Interfaces:**
- Produces: `fn namespaces(&self, prefix: &str) -> Result<Vec<String>>` on the `AppStore` trait — every namespace that currently holds at least one record and starts with `prefix`, sorted ascending.

There is exactly **one** implementation of `AppStore` in the whole workspace (`FsAppStore`) and no test fakes, so this is a one-file change plus the trait. A namespace is a directory under the store root, so enumeration is a directory read.

- [ ] **Step 1: Write the failing test** (in `peerbeam-appstore-fs`'s tests)

```rust
    #[test]
    fn namespaces_lists_populated_namespaces_matching_a_prefix() {
        let (store, _tmp) = new_store();
        store.put("chat-pb-alice", "k", b"v").unwrap();
        store.put("chat-pb-bob", "k", b"v").unwrap();
        store.put("chat.outbox", "k", b"v").unwrap();
        store.put("clipboard", "k", b"v").unwrap();

        let mut got = store.namespaces("chat-").unwrap();
        got.sort();
        assert_eq!(got, vec!["chat-pb-alice", "chat-pb-bob"]);
        // The outbox is deliberately `chat.outbox` (a dot, not a dash) so no
        // peer-supplied device id can collide with it — and so a `chat-`
        // prefix scan never picks it up as a conversation.
        assert!(!got.contains(&"chat.outbox".to_string()));
        assert_eq!(store.namespaces("").unwrap().len(), 4);
        assert!(store.namespaces("nothing-").unwrap().is_empty());
    }

    #[test]
    fn a_namespace_emptied_by_delete_is_not_listed() {
        let (store, _tmp) = new_store();
        store.put("chat-pb-gone", "k", b"v").unwrap();
        assert_eq!(store.namespaces("chat-").unwrap(), vec!["chat-pb-gone"]);
        store.delete("chat-pb-gone", "k").unwrap();
        assert!(
            store.namespaces("chat-").unwrap().is_empty(),
            "an empty namespace is not a conversation"
        );
    }
```

If the crate has no `new_store()` helper, add one returning `(FsAppStore, TempDir)` built the same way the existing tests build a store.

- [ ] **Step 2: Run to see it fail**

Run: `cd rust && cargo test -p peerbeam-appstore-fs namespaces`
Expected: FAIL — no method `namespaces`.

- [ ] **Step 3: Add the trait method**

In `appstore.rs`, after `list`:

```rust
    /// Every namespace holding at least one record whose name starts with
    /// `prefix`, sorted ascending. `Ok(vec![])` when none match.
    ///
    /// A namespace that exists but holds no records is not returned: callers
    /// use this to enumerate real conversations, and an empty directory left
    /// by a `clear` is not one.
    fn namespaces(&self, prefix: &str) -> Result<Vec<String>>;
```

- [ ] **Step 4: Implement it in `FsAppStore`**

```rust
    fn namespaces(&self, prefix: &str) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            // No root yet means nothing has ever been stored.
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(e.to_string())),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue; // a non-UTF-8 directory is not one we wrote
            };
            if !name.starts_with(prefix) {
                continue;
            }
            // Populated only: `clear` removes the directory, but a crash
            // between `create_dir_all` and the first write can leave an empty
            // one, and that is not a conversation.
            let populated = std::fs::read_dir(entry.path())
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            if populated {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }
```

Match the error type the file's other methods use for IO failures (read one of `put`/`list` first and mirror it exactly).

- [ ] **Step 5: Run tests, full gate, commit**

```bash
cd rust && cargo test -p peerbeam-appstore-fs && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-domain rust/crates/peerbeam-appstore-fs
git commit -m "feat(appstore): enumerate namespaces by prefix

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Schema — `Status::Staging`, and file fields on `OutboxEntry`

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/record.rs` (`Status` at :19-35)
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (`OutboxEntry` at :40-46)
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (re-exports at :9-15)
- Test: both files' test modules

**Interfaces:**
- Produces:
  - `Status::Staging` — serializes as `"staging"` (the enum carries `#[serde(rename_all = "lowercase")]`).
  - `StagedFile { name: String, size: u64, staged_path: String }`.
  - `OutboxEntry { peer_id, message_id, body, timestamp, kind: Kind, file: Option<StagedFile>, offers_refused: u32 }` — the three new fields all `#[serde(default)]`.
  - Both exported from `peerbeam_chat`.

- [ ] **Step 1: Write the failing tests**

In `record.rs`'s tests:

```rust
    #[test]
    fn staging_serializes_as_lowercase_and_is_not_settleable() {
        let v = serde_json::to_value(Status::Staging).unwrap();
        assert_eq!(v, serde_json::json!("staging"));
        // Staging is before the queue, let alone before a transfer: no
        // wire-driven settle may touch a row that has not been offered yet.
        let rec = file_row(Direction::Out, Status::Staging);
        assert!(!rec.is_settleable_file_row(Direction::Out));
    }
```

(`file_row` is the existing helper in that module.)

In `store.rs`'s tests:

```rust
    #[test]
    fn a_1b_era_outbox_entry_json_still_decodes() {
        // Exactly what 1b wrote — no kind, no file, no offers_refused.
        let raw = br#"{"peer_id":"pb-alice","message_id":"0000000000001",
                       "body":"hi","timestamp":"2026-08-13T10:00:00+00:00"}"#;
        let e = OutboxEntry::decode(raw).expect("legacy entry must still decode");
        assert_eq!(e.body, "hi");
        assert_eq!(e.kind, Kind::Text, "an entry with no kind is text");
        assert!(e.file.is_none());
        assert_eq!(e.offers_refused, 0);
    }

    #[test]
    fn a_file_outbox_entry_round_trips_its_staged_blob() {
        let e = OutboxEntry {
            peer_id: "pb-bob".into(),
            message_id: "0000000000002".into(),
            body: String::new(),
            timestamp: "2026-08-13T10:00:00+00:00".into(),
            kind: Kind::File,
            file: Some(StagedFile {
                name: "report.pdf".into(),
                size: 4096,
                staged_path: "/data/outbox-blobs/0000000000002".into(),
            }),
            offers_refused: 2,
        };
        let back = OutboxEntry::decode(&e.encode()).unwrap();
        assert_eq!(back, e);
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cd rust && cargo test -p peerbeam-chat staging_serializes a_1b_era_outbox a_file_outbox`
Expected: FAIL — no `Status::Staging`, no `StagedFile`, `OutboxEntry` has no such fields.

- [ ] **Step 3: Add `Status::Staging`**

In `record.rs`, add to `Status` (keep every existing variant and their order):

```rust
    /// The file is being copied into the outbox's own storage. Nothing has
    /// been queued or offered yet, so nothing can settle it.
    Staging,
```

Do **not** add it to `is_settleable_file_row`'s in-flight set — that set stays `Transferring | PendingApproval`.

- [ ] **Step 4: Grow `OutboxEntry`**

In `store.rs`:

```rust
/// A file staged into the outbox's own storage, ready to send.
///
/// `staged_path` points at a copy the outbox owns — not at the user's original
/// file. That is the whole point of staging: once this exists, deleting,
/// moving, renaming or editing the source cannot change or break what gets
/// delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedFile {
    pub name: String,
    pub size: u64,
    pub staged_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub peer_id: String,
    pub message_id: String,
    pub body: String,
    pub timestamp: String,
    /// Text (default, so every 1b/2a entry decodes) or File.
    #[serde(default)]
    pub kind: Kind,
    /// Present only when `kind == Kind::File`.
    #[serde(default)]
    pub file: Option<StagedFile>,
    /// How many times an offer actually REACHED the peer and was refused or
    /// timed out at its approval gate. A connection failure never increments
    /// this: nobody saw the offer, nobody was prompted, and keep-forever is
    /// the promise text already makes. See the backstop in Task 6.
    #[serde(default)]
    pub offers_refused: u32,
}
```

Fix every existing `OutboxEntry { .. }` literal the compiler flags (at minimum `enqueue`, and any in tests) by adding `kind: Kind::Text, file: None, offers_refused: 0`.

- [ ] **Step 5: Export and verify**

Add `StagedFile` to `store`'s re-export line in `lib.rs`.

Run: `cd rust && cargo test -p peerbeam-chat`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

```bash
cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-chat
git commit -m "feat(chat): Status::Staging and additive file fields on OutboxEntry

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Staging — own the bytes, bounded

**Files:**
- Create: `rust/crates/peerbeam-chat/src/staging.rs`
- Modify: `rust/crates/peerbeam-chat/Cargo.toml` (add `futures`, `peerbeam-platform`)
- Modify: `rust/crates/peerbeam-chat/src/lib.rs` (`pub mod staging;` + re-exports)
- Modify: `rust/crates/peerbeam-platform/src/lib.rs`, its `Cargo.toml`, and `rust/Cargo.toml` (add `fs4`)
- Modify: `rust/crates/peerbeam-config/src/lib.rs` (`DeviceConfig`)

**Interfaces:**
- Produces:
  - `peerbeam_platform::available_bytes(path: &str) -> Option<u64>`
  - `DeviceConfig.max_queued_file_bytes: u64` (default `17_179_869_184`) and `DeviceConfig.min_free_bytes: u64` (default `536_870_912`)
  - `StagingStore::new(root: String, storage: Arc<dyn StorageProvider>) -> StagingStore`
  - `StagingStore::stage(&self, id: &str, source: &str, limits: StagingLimits, cancel: &TransferControl, progress: &UnboundedSender<u64>) -> Result<StagedFile, StagingError>`
  - `StagingStore::remove(&self, staged_path: &str)`
  - `StagingStore::sweep(&self, keep: &HashSet<String>) -> usize`
  - `StagingLimits { max_bytes: u64, min_free_bytes: u64 }`
  - `StagingError { TooLarge { size, max }, NotEnoughSpace { need, free }, Cancelled, Io(String) }`

The copy is a real streamed loop. Nothing in the workspace does a local reader→writer copy today (`peerbeam-transfer`'s copies all go through a `Link`), and `read_fill` is `pub(crate)` to that crate, so this loop is new code — keep it in `staging.rs` alone.

- [ ] **Step 1: Add the free-space primitive**

`rust/Cargo.toml`, in `[workspace.dependencies]`:

```toml
fs4 = "0.13"
```

`rust/crates/peerbeam-platform/Cargo.toml`:

```toml
fs4 = { workspace = true }
```

`rust/crates/peerbeam-platform/src/lib.rs`:

```rust
/// Bytes currently available on the filesystem holding `path`, or `None` when
/// that cannot be determined.
///
/// Lives here because this crate is already the one place that touches host
/// specifics (`hostname`, `config_dir`, `data_dir`, `download_dir`). `fs4`
/// covers Windows, macOS, Linux and Android behind one safe call — the
/// alternatives were a Unix-only `statvfs` or a raw Windows FFI binding
/// needing `unsafe`, and I12 makes every platform first-class.
#[must_use]
pub fn available_bytes(path: &str) -> Option<u64> {
    // Walk up to the nearest existing ancestor: the staging directory may not
    // be created yet the first time this is asked.
    let mut p = std::path::Path::new(path);
    loop {
        if p.exists() {
            return fs4::available_space(p).ok();
        }
        p = p.parent()?;
    }
}
```

Add a test asserting `available_bytes` on a `tempdir()` returns `Some(n)` with `n > 0`, and that a path under a non-existent subdirectory of that tempdir still returns `Some`.

- [ ] **Step 2: Add the two config knobs**

In `peerbeam-config`'s `DeviceConfig` (it already carries `#[serde(default)]` on the struct, so this is additive for every existing config file):

```rust
    /// Largest file that may be attached in a chat, in bytes.
    ///
    /// Staging is uniform — it runs on every chat send, not only on sends that
    /// queue — so this is a backstop against the absurd, not a product limit.
    /// A low value here would be a capability regression: the plain transfer
    /// path streams a file of any size straight from the source.
    pub max_queued_file_bytes: u64,
    /// Refuse to stage if doing so would leave less than this much free.
    /// Filling the disk to zero can break unrelated applications.
    pub min_free_bytes: u64,
```

Defaults in its `impl Default`: `max_queued_file_bytes: 17_179_869_184`, `min_free_bytes: 536_870_912`. Add a test asserting both defaults and that a config JSON omitting them still loads.

- [ ] **Step 3: Write the failing staging tests**

Create `staging.rs` with a test module covering:

```rust
    #[tokio::test]
    async fn stage_copies_the_bytes_and_survives_the_source_being_deleted() {
        let (staging, _tmp, src_dir) = new_staging();
        let src = src_dir.join("report.pdf");
        std::fs::write(&src, b"the original bytes").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let staged = staging
            .stage("id-1", src.to_str().unwrap(), generous(), &TransferControl::new(), &tx)
            .await
            .expect("staging a small file succeeds");
        assert_eq!(staged.name, "report.pdf");
        assert_eq!(staged.size, 18);

        // The whole reason staging exists.
        std::fs::remove_file(&src).unwrap();
        assert_eq!(std::fs::read(&staged.staged_path).unwrap(), b"the original bytes");
    }

    #[tokio::test]
    async fn stage_refuses_a_file_over_the_cap_without_copying_anything() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.join("big.bin");
        std::fs::write(&src, vec![0u8; 4096]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let err = staging
            .stage(
                "id-2",
                src.to_str().unwrap(),
                StagingLimits { max_bytes: 1024, min_free_bytes: 0 },
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("over the cap must refuse");
        assert!(matches!(err, StagingError::TooLarge { size: 4096, max: 1024 }));
        assert!(
            std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
            "a refused stage leaves no blob behind"
        );
        assert!(src.exists(), "the user's own file is never touched");
    }

    #[tokio::test]
    async fn stage_refuses_when_it_would_breach_the_free_space_floor() {
        let (staging, _tmp, src_dir) = new_staging();
        let src = src_dir.join("a.bin");
        std::fs::write(&src, vec![0u8; 4096]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // A floor no real disk can satisfy, so the check must fire.
        let err = staging
            .stage(
                "id-3",
                src.to_str().unwrap(),
                StagingLimits { max_bytes: u64::MAX, min_free_bytes: u64::MAX },
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("breaching the floor must refuse");
        assert!(matches!(err, StagingError::NotEnoughSpace { .. }));
    }

    #[tokio::test]
    async fn a_cancelled_stage_leaves_no_orphan_blob() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.join("c.bin");
        std::fs::write(&src, vec![7u8; 8 * 1024 * 1024]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        ctrl.cancel(); // cancelled before the first chunk

        let err = staging
            .stage("id-4", src.to_str().unwrap(), generous(), &ctrl, &tx)
            .await
            .expect_err("a cancelled stage does not produce a blob");
        assert!(matches!(err, StagingError::Cancelled));
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn stage_reports_progress_as_it_copies() {
        let (staging, _tmp, src_dir) = new_staging();
        let src = src_dir.join("d.bin");
        std::fs::write(&src, vec![1u8; 512 * 1024]).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        staging
            .stage("id-5", src.to_str().unwrap(), generous(), &TransferControl::new(), &tx)
            .await
            .unwrap();
        drop(tx);
        let mut seen = Vec::new();
        while let Some(n) = rx.recv().await {
            seen.push(n);
        }
        assert!(seen.len() > 1, "a multi-chunk copy reports more than once");
        assert_eq!(*seen.last().unwrap(), 512 * 1024);
    }

    #[tokio::test]
    async fn sweep_deletes_blobs_no_queue_entry_owns() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.join("e.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let keep = staging
            .stage("keep-me", src.to_str().unwrap(), generous(), &TransferControl::new(), &tx)
            .await
            .unwrap();
        let orphan = staging
            .stage("orphan", src.to_str().unwrap(), generous(), &TransferControl::new(), &tx)
            .await
            .unwrap();

        // What a crash between staging and enqueue leaves behind.
        let mut owned = std::collections::HashSet::new();
        owned.insert(keep.staged_path.clone());
        assert_eq!(staging.sweep(&owned), 1);
        assert!(std::path::Path::new(&keep.staged_path).exists());
        assert!(!std::path::Path::new(&orphan.staged_path).exists());
        let _ = tmp;
    }
```

Add the helpers `new_staging() -> (StagingStore, TempDir, PathBuf)` (blob root inside a tempdir, an `FsStorage`, and a separate source dir) and `generous() -> StagingLimits { max_bytes: u64::MAX, min_free_bytes: 0 }`.

- [ ] **Step 4: Run to see them fail**

Run: `cd rust && cargo test -p peerbeam-chat staging`
Expected: FAIL — module does not exist.

- [ ] **Step 5: Implement `staging.rs`**

```rust
//! The outbox's own copy of a file waiting to be sent.
//!
//! A queued file cannot be a path into the user's filesystem: between queueing
//! and delivery they may delete it, move it, rename it, or edit it, and a queue
//! that silently sends different bytes than the ones chosen is worse than one
//! that fails. So the bytes are copied into storage the outbox owns, and the
//! source stops mattering the moment `stage` returns.
//!
//! The copy streams (I10) — no whole file is ever in memory — and is bounded by
//! an explicit size cap and a free-space floor, because staging duplicates
//! whatever it copies.

use std::collections::HashSet;
use std::sync::Arc;

use futures::io::{AsyncReadExt, AsyncWriteExt};
use peerbeam_domain::port::StorageProvider;
use peerbeam_transfer::TransferControl;
use tokio::sync::mpsc::UnboundedSender;

use crate::store::StagedFile;

/// Copy buffer. Matches the transfer engine's own read buffer so a staged copy
/// and a wire send behave alike on the same storage.
const COPY_BUF: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct StagingLimits {
    pub max_bytes: u64,
    pub min_free_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    #[error("{size} bytes is over the {max}-byte limit for a chat attachment")]
    TooLarge { size: u64, max: u64 },
    #[error("staging needs {need} bytes but only {free} are free")]
    NotEnoughSpace { need: u64, free: u64 },
    #[error("staging cancelled")]
    Cancelled,
    #[error("staging failed: {0}")]
    Io(String),
}

pub struct StagingStore {
    root: String,
    storage: Arc<dyn StorageProvider>,
}

impl StagingStore {
    #[must_use]
    pub fn new(root: String, storage: Arc<dyn StorageProvider>) -> StagingStore {
        StagingStore { root, storage }
    }

    fn blob_path(&self, id: &str) -> String {
        format!("{}/{}", self.root.trim_end_matches('/'), id)
    }

    /// Stream `source` into the outbox's storage under `id`.
    ///
    /// Refuses before copying a single byte when the file is over the cap or
    /// when copying it would breach the free-space floor — an honest refusal
    /// now beats filling the disk and failing later. On cancellation or any IO
    /// error the partial blob is removed, so a failed stage never leaves an
    /// orphan for the sweep to find.
    pub async fn stage(
        &self,
        id: &str,
        source: &str,
        limits: StagingLimits,
        cancel: &TransferControl,
        progress: &UnboundedSender<u64>,
    ) -> Result<StagedFile, StagingError> {
        let meta = std::fs::metadata(source).map_err(|e| StagingError::Io(e.to_string()))?;
        if meta.is_dir() {
            return Err(StagingError::Io(
                "folders aren't supported in chat yet — use Send folder".into(),
            ));
        }
        let size = meta.len();
        if size > limits.max_bytes {
            return Err(StagingError::TooLarge { size, max: limits.max_bytes });
        }
        if limits.min_free_bytes > 0 {
            // `None` means we could not measure; proceed rather than refuse a
            // send because a platform would not answer.
            if let Some(free) = peerbeam_platform::available_bytes(&self.root) {
                if free.saturating_sub(size) < limits.min_free_bytes {
                    return Err(StagingError::NotEnoughSpace { need: size, free });
                }
            }
        }
        let name = std::path::Path::new(source)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| StagingError::Io(format!("no file name in {source}")))?;

        let dest = self.blob_path(id);
        match self.copy(source, &dest, cancel, progress).await {
            Ok(()) => Ok(StagedFile { name, size, staged_path: dest }),
            Err(e) => {
                // Never leave a partial blob: the sweep only knows about
                // orphans, and a half-copied file that looks staged would be
                // sent as if it were whole.
                self.remove(&dest);
                Err(e)
            }
        }
    }

    async fn copy(
        &self,
        source: &str,
        dest: &str,
        cancel: &TransferControl,
        progress: &UnboundedSender<u64>,
    ) -> Result<(), StagingError> {
        let mut reader = self
            .storage
            .open_read(source, 0)
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        let mut writer = self
            .storage
            .open_write(dest)
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        let mut buf = vec![0u8; COPY_BUF];
        let mut done: u64 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(StagingError::Cancelled);
            }
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| StagingError::Io(e.to_string()))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .await
                .map_err(|e| StagingError::Io(e.to_string()))?;
            done += n as u64;
            let _ = progress.send(done);
        }
        writer
            .flush()
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        writer
            .close()
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        Ok(())
    }

    /// Delete a staged blob. Best-effort: a blob already gone is success.
    pub fn remove(&self, staged_path: &str) {
        let _ = std::fs::remove_file(staged_path);
    }

    /// Delete every blob no queue entry owns, returning how many went.
    ///
    /// This is what a crash between staging and enqueue leaves behind: bytes on
    /// disk nothing will ever send and nothing will ever delete.
    pub fn sweep(&self, keep: &HashSet<String>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path().to_string_lossy().into_owned();
            if !keep.contains(&path) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}
```

Check `TransferControl`'s real cancellation accessor before writing `is_cancelled()` — read its definition in `peerbeam-transfer` and use whatever it actually exposes.

Add to `peerbeam-chat/Cargo.toml`: `futures = { workspace = true }` and `peerbeam-platform = { path = "../peerbeam-platform" }`. Add `pub mod staging;` and re-export `StagingError`, `StagingLimits`, `StagingStore` from `lib.rs`.

- [ ] **Step 6: Run tests, full gate, commit**

```bash
cd rust && cargo test -p peerbeam-chat && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/Cargo.toml rust/crates/peerbeam-platform rust/crates/peerbeam-config rust/crates/peerbeam-chat
git commit -m "feat(chat): stage a queued file into storage the outbox owns

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `FileDecline` — make a refusal terminal

**Files:**
- Modify: `rust/crates/peerbeam-domain/src/session/negotiation.rs` (beside `CHAT_FEAT_FILEREF` at :82)
- Modify: `rust/crates/peerbeam-domain/src/session/mod.rs` (re-export at :25-27)
- Modify: `rust/crates/peerbeam-chat/src/message.rs`, `src/handler.rs`, `src/lib.rs`
- Modify: `rust/crates/peerbeam-ffi/src/session_exec.rs` (`session_cfg` at :66-75), `src/transfer.rs` (the `!accepted` branch at ~:1985)
- Modify: `rust/bins/peerbeam-cli/src/session_transfer.rs` (`session_cfg` at :67-76)
- Test: `message.rs`, `handler.rs`, `session_transfer.rs` test modules

**Interfaces:**
- Consumes: `MSG_FILE_REF`, `mint_id`, `MessageFlags`, `CapabilitySet` (Tasks 1-4 untouched).
- Produces:
  - `peerbeam_domain::session::CHAT_FEAT_FILEDECLINE: u32 = 1 << 1`
  - `peerbeam_chat::MSG_FILE_DECLINE: u16 = 3`
  - `peerbeam_chat::FileDecline { id: String, timestamp: String }` with `new(id)`, `message_type()`, `to_frame(channel)`, `from_frame(&SessionFrame)`
  - `peerbeam_chat::send_file_decline(handle: &SessionHandle, d: &FileDecline) -> Result<(), SendError>`
  - `ChatHandler` dispatches `MSG_FILE_DECLINE` on the sender's own `Out`/`File` row → `Status::Declined`

**Both surfaces must advertise the bit.** `peerbeam-ffi/src/session_exec.rs:66-75` and `peerbeam-cli/src/session_transfer.rs:67-76` each build their own `CapabilitySet`. 2a's history includes a bug where only one of them advertised, so change both in this task and test both.

- [ ] **Step 1: Write the failing tests**

`message.rs`:

```rust
    #[test]
    fn file_decline_round_trips_and_ships_optional() {
        let d = FileDecline::new("0000000000001");
        let frame = d.to_frame(ChannelId::new(7)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_FILE_DECLINE);
        // OPTIONAL so a peer that does not know type 3 ignores it and keeps
        // the channel (MESSAGE_REGISTRY.md section 6) rather than tearing the
        // conversation down.
        assert!(frame.flags.is_optional());
        assert_eq!(FileDecline::from_frame(&frame).unwrap(), d);
    }

    #[test]
    fn file_decline_rejects_a_frame_of_the_wrong_type() {
        let r = FileRef::new("a.bin", 1).unwrap();
        let frame = r.to_frame(ChannelId::new(7)).unwrap();
        assert!(matches!(
            FileDecline::from_frame(&frame),
            Err(ChatError::WrongType(MSG_FILE_REF))
        ));
    }
```

`handler.rs`:

```rust
    #[tokio::test]
    async fn a_decline_settles_our_own_outgoing_row() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        let r = FileRef::new("report.pdf", 4096).unwrap();
        store
            .append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, r.size, Some("/src/report.pdf".into())),
                Status::Transferring,
            ))
            .unwrap();

        let d = FileDecline::new(&r.id);
        handler.handle(d.to_frame(ChannelId::new(1)).unwrap()).await.unwrap();

        let rec = store.get(&peer, &r.id).unwrap().unwrap();
        assert_eq!(rec.status, Status::Declined);
    }

    #[tokio::test]
    async fn a_decline_naming_a_row_that_is_not_ours_to_decline_is_ignored() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        // Our own INCOMING row: the peer declining a file they sent us is
        // meaningless, and must not rewrite anything.
        let r = FileRef::new("theirs.pdf", 10).unwrap();
        store.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        handler
            .handle(FileDecline::new(&r.id).to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

        assert_eq!(
            store.get(&peer, &r.id).unwrap().unwrap().status,
            Status::PendingApproval,
            "an inbound row is untouched by a decline"
        );
    }

    #[tokio::test]
    async fn a_decline_for_an_unknown_id_is_a_silent_no_op() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        handler
            .handle(FileDecline::new("never-existed").to_frame(ChannelId::new(1)).unwrap())
            .await
            .expect("an unknown id must not fail the channel");
        assert!(store.history(&peer).unwrap().is_empty());
    }
```

`session_transfer.rs` (CLI) and an equivalent in the FFI:

```rust
    #[test]
    fn both_chat_feature_bits_are_advertised() {
        let caps = advertised_caps();
        let f = caps.features(ChannelType::CHAT).expect("CHAT advertised");
        assert!(f & CHAT_FEAT_FILEREF != 0, "file sharing");
        assert!(f & CHAT_FEAT_FILEDECLINE != 0, "decline signalling");
    }
```

Extract whatever `session_cfg` builds into a small `advertised_caps()` helper in each file so the test can read it without constructing a session.

- [ ] **Step 2: Run to see them fail**

Run: `cd rust && cargo test -p peerbeam-chat -p peerbeam-cli -p peerbeam-ffi file_decline both_chat_feature_bits a_decline`
Expected: FAIL — nothing named `FileDecline` or `CHAT_FEAT_FILEDECLINE`.

- [ ] **Step 3: Add the feature bit**

`negotiation.rs`, beside `CHAT_FEAT_FILEREF`:

```rust
/// Feature bit on the CHAT capability: this peer sends a `FileDecline`
/// (chat MessageType 3) when its user turns down an offered file.
///
/// Without it a sender cannot tell "you declined" from "the network dropped",
/// so a refused file would be re-offered forever and re-prompt its receiver
/// every time. A peer that does not advertise this is handled by the sender's
/// bounded backstop instead.
pub const CHAT_FEAT_FILEDECLINE: u32 = 1 << 1;
```

Add it to the `pub use negotiation::{...}` list in `session/mod.rs`.

- [ ] **Step 4: Add the wire type**

`message.rs` — mirror `FileRef` exactly:

```rust
pub const MSG_FILE_DECLINE: u16 = 3;

/// "I turned down the file you offered." Carries only the id of the `FileRef`
/// being refused — everything else about the file is already in both threads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDecline {
    pub id: String,
    pub timestamp: String,
}

impl FileDecline {
    #[must_use]
    pub fn new(id: &str) -> FileDecline {
        FileDecline { id: id.to_string(), timestamp: Utc::now().to_rfc3339() }
    }

    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_FILE_DECLINE)
    }

    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
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

    pub fn from_frame(frame: &SessionFrame) -> Result<FileDecline, ChatError> {
        if frame.message_type.get() != MSG_FILE_DECLINE {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }
}
```

- [ ] **Step 5: Dispatch it in the handler**

In `ChatHandler::handle`'s match (handler.rs:56-103), add an arm **before** the `other =>` fallback:

```rust
        MSG_FILE_DECLINE => {
            let d = FileDecline::from_frame(&frame)?;
            // Only our own OUTGOING file row can be declined, and only while
            // it is still in flight. `settle_file_row` enforces exactly that
            // — the same guard every other wire-driven write goes through, so
            // a peer cannot use a decline to rewrite a text row, an inbound
            // row, or a row that already settled.
            let _ = self
                .store
                .settle_file_row(peer, &d.id, Direction::Out, Status::Declined)
                .map_err(SessionError::from)?;
            Ok(())
        }
```

`settle_file_row` already returns `Result<bool, ChatError>` and no-ops when the guard rejects, so an unknown id and a hostile id are both silent successes.

- [ ] **Step 6: Send it, and advertise the bit**

In `send.rs`, beside `send_file_ref`:

```rust
/// Tell the sender we turned their file down.
pub async fn send_file_decline(
    handle: &SessionHandle,
    d: &FileDecline,
) -> Result<(), SendError> {
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
    let frame = d.to_frame(channel)?;
    handle
        .send_on_channel(channel, FileDecline::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()))
}
```

In **both** `session_cfg`s, change the CHAT capability to:

```rust
        .with(Capability::with_features(
            CHAT,
            CHAT_FEAT_FILEREF | CHAT_FEAT_FILEDECLINE,
        ))
```

Add `Session::supports_file_decline()` alongside the existing `supports_file_ref()` in both files, using the same `features(CHAT).is_some_and(|f| f & CHAT_FEAT_FILEDECLINE != 0)` shape.

- [ ] **Step 7: Send a decline when the receiver declines**

In `peerbeam-ffi/src/transfer.rs`'s `handle_incoming`, inside the existing `if !accepted { ... }` block (which already emits `transfer_cancelled` and settles `Declined`), send the signal before closing the session:

```rust
            if session.supports_file_decline() && self.chat.contains(&session.peer_device, &id).unwrap_or(false) {
                // Best-effort: the row is already Declined locally either way.
                // This is what stops the sender re-offering the same file
                // every drain tick for as long as we both keep running.
                let d = peerbeam_chat::FileDecline::new(&id);
                let _ = peerbeam_chat::send_file_decline(&session.handle, &d).await;
            }
            session.close().await;
```

Do not change the approval gate's decision logic (I6) — only what is sent after a decision already made.

- [ ] **Step 8: Run tests, full gate, commit**

```bash
cd rust && cargo test -p peerbeam-chat -p peerbeam-cli -p peerbeam-ffi && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-domain rust/crates/peerbeam-chat rust/crates/peerbeam-ffi rust/bins/peerbeam-cli
git commit -m "feat(chat): FileDecline makes a refused file terminal

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: The queue and the drain — THE CRUX

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (`enqueue_file`, `record_sent` neighbours)
- Modify: `rust/crates/peerbeam-chat/src/send.rs` (`prepare_file_send`, `flush_to_session`)
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`chat_send_file`, `run_chat_file_send`, a reopen path)
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs`, plus unit tests in `store.rs`/`send.rs`

**Interfaces:**
- Consumes: `StagingStore` (Task 4), `StagedFile`/`OutboxEntry` (Task 3), `FileDecline` (Task 5).
- Produces:
  - `ChatStore::enqueue_file(&self, peer: &DeviceId, r: &FileRef, staged: &StagedFile) -> Result<(), ChatError>`
  - `ChatStore::outbox_bump_refused(&self, message_id: &str) -> Result<u32, ChatError>` — increments and returns the new count
  - `ChatStore::reopen_for_retry(&self, peer: &DeviceId, id: &str) -> Result<bool, ChatError>`
  - `flush_to_session` handles both kinds and honours the in-flight guard

**Read before writing.** `flush_to_session` (send.rs:87-117) currently opens ONE chat channel and pushes every queued `ChatMessage` FIFO, stopping at the first failure. A file entry cannot go down that path: its bytes ride TRANSFER. Text must keep flushing exactly as it does now.

- [ ] **Step 1: The retry re-open, and why the guard stays strict**

`is_settleable_file_row` admits only `Transferring | PendingApproval`, which makes every terminal write once-only. A retry needs to move a `Failed` row back to `Transferring`, which that guard forbids — correctly, because the guard's whole purpose is that a peer-supplied id must not be able to rewrite a settled row.

So the re-open is a **local, sender-initiated** write that no peer input can reach, exactly like the existing `fail_chat_file`. Add to `store.rs`:

```rust
    /// Re-open our own outgoing file row for another delivery attempt.
    ///
    /// Deliberately NOT routed through `settle_file_row`: that guard admits
    /// only in-flight rows, which is what makes a wire-driven settle
    /// once-only, and relaxing it would let a peer resurrect a row it already
    /// settled. This is reachable only from the local drain, on a row we
    /// ourselves queued, with an id we minted. Returns whether it re-opened.
    pub fn reopen_for_retry(&self, peer: &DeviceId, id: &str) -> Result<bool, ChatError> {
        let Some(mut rec) = self.get(peer, id)? else {
            return Ok(false);
        };
        if rec.kind != Kind::File || rec.direction != Direction::Out {
            return Ok(false);
        }
        if !matches!(rec.status, Status::Failed | Status::Pending) {
            return Ok(false); // Sent/Declined are final; Transferring is already live
        }
        rec.status = Status::Transferring;
        self.append(&rec)?;
        Ok(true)
    }
```

Test it directly, and test that a wire-driven settle still cannot do the same:

```rust
    #[test]
    fn a_retry_reopens_a_failed_row_but_a_wire_settle_still_cannot() {
        let (cs, _store, _tmp) = new_store();
        let peer = DeviceId::from("pb-bob".to_string());
        let r = FileRef::new("a.bin", 1).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &r,
            FileMeta::new(&r.name, r.size, None),
            Status::Failed,
        ))
        .unwrap();

        // The wire cannot touch a settled row — this is the security guard.
        assert!(!cs
            .settle_file_row(&peer, &r.id, Direction::Out, Status::Transferring)
            .unwrap());
        assert_eq!(cs.get(&peer, &r.id).unwrap().unwrap().status, Status::Failed);

        // The local retry path can, and only for our own outgoing file row.
        assert!(cs.reopen_for_retry(&peer, &r.id).unwrap());
        assert_eq!(cs.get(&peer, &r.id).unwrap().unwrap().status, Status::Transferring);
        // A settled-Sent row is never re-opened.
        cs.settle_file_row(&peer, &r.id, Direction::Out, Status::Sent).unwrap();
        assert!(!cs.reopen_for_retry(&peer, &r.id).unwrap());
    }
```

- [ ] **Step 2: Enqueue a file, and make the send path uniform**

Add `enqueue_file` to `store.rs` mirroring `enqueue`, writing an `OutboxEntry` with `kind: Kind::File`, `file: Some(staged.clone())`, `body: String::new()`, `offers_refused: 0`.

Change `prepare_file_send` (send.rs:136-161) so it stages first and persists `Status::Staging`, then the caller enqueues and the row becomes `Pending`. Give it the staging store and limits:

```rust
pub async fn prepare_file_send(
    store: &ChatStore,
    staging: &StagingStore,
    peer: &DeviceId,
    path: &str,
    limits: StagingLimits,
    cancel: &TransferControl,
    progress: &UnboundedSender<u64>,
) -> Result<(FileRef, StagedFile), SendError> {
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

    // The row exists before the copy starts, so a multi-GB stage is visible
    // rather than looking like a hung attach.
    store.append(&ChatRecord::file_out(
        peer,
        &r,
        FileMeta::new(&r.name, r.size, Some(path.to_string())),
        Status::Staging,
    ))?;

    let staged = match staging.stage(&r.id, path, limits, cancel, progress).await {
        Ok(s) => s,
        Err(e) => {
            // Nothing was queued, so the row must say so rather than sit on
            // Staging forever.
            let _ = store.set_status(peer, &r.id, Status::Failed);
            return Err(SendError::Session(e.to_string()));
        }
    };
    store.enqueue_file(peer, &r, &staged)?;
    store.set_status(peer, &r.id, Status::Pending)?;
    Ok((r, staged))
}
```

- [ ] **Step 3: Teach the flush about files, with the in-flight guard**

Extend `flush_to_session` so it partitions the peer's entries: text goes down the existing single-chat-channel FIFO exactly as today; file entries are handled one at a time.

```rust
/// Deliver a peer's queued entries over `handle`.
///
/// Text keeps its existing behaviour precisely: one CHAT channel, FIFO, stop
/// at the first failure so nothing is dequeued that did not go.
///
/// Files are different in two ways. Their bytes ride the TRANSFER stream
/// channel, not CHAT, so a large upload cannot block a message behind it. And
/// only ONE file per peer is started per flush: queueing five videos must not
/// start five transfers competing for the same link.
pub async fn flush_to_session(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
) -> Result<Vec<String>, SendError> {
```

Keep the returned `Vec<String>` meaning "message ids actually delivered", because `chat_flush_peer` and `handle_incoming` already emit `chat_status` per returned id.

The file half needs the caller's transfer machinery, which `peerbeam-chat` does not have. So `flush_to_session` returns the **next file entry to send** rather than sending it itself:

```rust
/// The one file entry this flush wants sent next, if any. The caller owns the
/// transfer engine, so it performs the send; this function only decides.
pub struct PendingFile {
    pub entry: OutboxEntry,
    pub file: StagedFile,
}
```

Give `flush_to_session` a sibling `next_file_for(store, peer) -> Result<Option<PendingFile>, ChatError>` returning the oldest `Kind::File` entry, and let the FFI call it. Text delivery stays entirely inside `flush_to_session`.

- [ ] **Step 4: Wire the FFI**

In `transfer.rs`:

- `chat_send_file` becomes: resolve the peer, then spawn a task that stages (emitting `chat_status` `"staging"` progress), enqueues, and immediately attempts a drain for that peer. It returns `{id}` as it does today.
- Add `Manager::chat_file_in_flight: Mutex<HashSet<String>>` keyed by `peer_id`. Before starting a queued file for a peer, insert; on every terminal path, remove. That is the one-file-in-flight guard.
- On a successful send: `outbox_remove(&entry.message_id)` and settle `Sent`.
- On a refusal that **reached** the peer (the receiver declined, or its approval timed out): `outbox_bump_refused`; at `>= 3` treat as terminal — settle `Failed` with a reason naming the backstop, `outbox_remove`, and `staging.remove(&file.staged_path)`.
- On a connection failure: change nothing about the entry. It stays queued forever, exactly like text.
- On a `FileDecline` arriving (Task 5): the handler already settles the row; also `outbox_remove` and delete the blob. Do this in `Manager` where the store and staging are both in hand.

- [ ] **Step 5: Write the end-to-end tests** (`chat_ffi.rs`)

At minimum, mirroring the existing harness in that file:

```rust
async fn a_queued_file_survives_a_restart_and_delivers_when_the_peer_appears() { /* ... */ }
async fn the_uniform_send_path_still_delivers_online_exactly_as_2a_did() { /* ... */ }
async fn five_queued_files_start_one_transfer_not_five() { /* ... */ }
async fn a_declined_file_goes_terminal_and_never_re_offers() { /* ... */ }
async fn three_refusals_go_terminal_but_an_unreachable_peer_never_does() { /* ... */ }
async fn a_4gb_file_at_the_head_of_the_queue_does_not_delay_a_text_message() { /* ... */ }
```

For the 4 GB case use the `"gen:<size>"` synthetic-storage trick the transfer crate's `largefile.rs` already uses rather than writing 4 GB to disk; assert the text message's `chat_status` `"sent"` arrives while the file is still transferring.

The second test is the regression floor: it must assert the same things `chat_send_file_shares_a_file_in_the_thread_end_to_end` asserts today, because Task 6 re-routes that path through the queue.

- [ ] **Step 6: Run everything, full gate, commit**

```bash
cd rust && cargo test -p peerbeam-chat -p peerbeam-ffi && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-chat rust/crates/peerbeam-ffi
git commit -m "feat(chat): queue a file for an offline peer and drain it when they return

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Reconcile every conversation, and sweep orphan blobs at startup

**Files:**
- Modify: `rust/crates/peerbeam-chat/src/store.rs` (add `conversations`)
- Modify: `rust/crates/peerbeam-ffi/src/runtime.rs` (`reconcile_chat` at :265-280, `init` at :291-403)
- Test: `store.rs` tests, `peerbeam-ffi/tests/runtime_ffi.rs`

**Interfaces:**
- Consumes: `AppStore::namespaces` (Task 2), `StagingStore::sweep` (Task 4).
- Produces: `ChatStore::conversations(&self) -> Result<Vec<DeviceId>, ChatError>`.

- [ ] **Step 1: Add `conversations`**

```rust
    /// Every peer this device has a conversation with.
    ///
    /// Derived from the namespaces that actually exist rather than from a
    /// separate index, which could drift from reality and silently hide a
    /// thread. `namespace()` always emits `chat-<id>`, and the outbox is
    /// `chat.outbox` (a dot), so the prefix scan cannot pick it up.
    pub fn conversations(&self) -> Result<Vec<DeviceId>, ChatError> {
        let names = self
            .store
            .namespaces("chat-")
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(names
            .into_iter()
            .filter_map(|ns| ns.strip_prefix("chat-").map(|id| DeviceId::from(id.to_string())))
            .collect())
    }
```

Test that a peer with only a file row appears (the exact case `outbox_peers` misses), and that the outbox namespace never does.

- [ ] **Step 2: Use it at startup, and sweep**

In `runtime.rs`, change `reconcile_chat` to enumerate `chat.conversations()` instead of `chat.outbox_peers()`, keeping its existing per-peer `reconcile_peer` call and logging. Update its doc comment, which currently states the old limitation explicitly.

Then, in `init`, after the `ChatStore` is built and reconciled, sweep orphan blobs:

```rust
    // Bytes staged by a run that crashed between staging and enqueue are owned
    // by nothing and would otherwise sit on disk forever.
    let owned: std::collections::HashSet<String> = chat
        .outbox_pending()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.file.map(|f| f.staged_path))
        .collect();
    let swept = staging.sweep(&owned);
    if swept > 0 {
        tracing::info!(count = swept, "removed orphaned staged files");
    }
```

- [ ] **Step 3: Test, gate, commit**

Add a `runtime_ffi.rs` test that a blob with no queue entry is gone after `pb_init`, and a `store.rs` test for `conversations`.

```bash
cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-chat rust/crates/peerbeam-ffi
git commit -m "feat(ffi): reconcile every conversation and sweep orphaned staged files

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: FFI surface — staging progress, cancel, conversations

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs`, `src/events.rs`, `src/lib.rs`
- Test: `rust/crates/peerbeam-ffi/tests/chat_ffi.rs`

**Interfaces:**
- Produces:
  - `pb_chat_conversations(json) -> {peers:[{peer_id, last_timestamp, unread_hint}]}` — `{}` takes no arguments
  - `pb_chat_cancel(json) -> {cancelled}` taking `{peer_id, message_id}` — cancels a staging or queued file, deletes the blob, dequeues, settles `Failed`/`cancelled`
  - `chat_status` events gain a `progress` key while a row is `staging`: `{message_id, peer_id, status:"staging", progress:{done, total}}`

Mirror `pb_chat_send_file`'s exact shape for both exports (`guard(|| error::envelope((|| runtime::manager()?.method(&read_json(json)?))()))`, with a `# Safety` doc comment).

- [ ] **Step 1: Write the failing tests** — a staging file emits at least one `chat_status` with `status == "staging"` and a growing `progress.done`; `pb_chat_cancel` on a queued file removes its outbox entry, deletes the blob, and settles the row `failed`; `pb_chat_conversations` lists a peer whose only row is a file.
- [ ] **Step 2: Run to see them fail.**
- [ ] **Step 3: Add `events::chat_staging(peer_id, message_id, done, total)`** beside `chat_status_detail`, emitting the same `chat_status` type with the extra `progress` object so no surface needs a new event kind.
- [ ] **Step 4: Implement both `Manager` methods and both exports.**
- [ ] **Step 5: Rebuild the cdylib before trusting Flutter** — `cargo build -p peerbeam-ffi`.
- [ ] **Step 6: Full gate and commit.**

```bash
cd rust && cargo build -p peerbeam-ffi && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/crates/peerbeam-ffi
git commit -m "feat(ffi): staging progress, cancel a queued file, list conversations

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: CLI — queueing, staging progress, cancel

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/chat.rs` (`send_file` at :443-599, `history` at :604-631, `render_file_line` at :154-163)
- Modify: `rust/bins/peerbeam-cli/src/cli.rs` (`ChatAction`)
- Test: `src/chat.rs` tests, `tests/cli_parse.rs`

`send_file` is a **separate function** from `send` — there is no shared `--file` branch to edit.

- [ ] **Step 1: Write failing tests** — `chat send --file` to an unreachable peer now **queues** (exit 0, row `pending`, an outbox entry exists) instead of returning `CliError::Connection`; `render_file_line` renders `staging` and `pending` rows; `chat cancel <peer> <id>` parses and removes a queued entry.
- [ ] **Step 2: Run to see them fail.**
- [ ] **Step 3: Rewrite `send_file`'s failure path** — stage, enqueue, then attempt one opportunistic dial-and-send exactly as text's `send` does. An unreachable peer prints `queued for {name} (offline — a running daemon/watch will deliver)`, matching text's wording. Keep the `supports_file_ref` refusal a hard error: that is a peer who can never receive it, not a peer who is merely away.
- [ ] **Step 4: Report staging progress** with `ctx.bar(size, &name)` — it is already a no-op unless `ctx.progress`, so JSON mode stays clean. Add a `{"event":"chat_staging","id","done","total"}` line for JSON consumers.
- [ ] **Step 5: Add `chat cancel`** to `ChatAction` with `peer` and `id` positionals.
- [ ] **Step 6: Render the new statuses** — `status_str` already round-trips through serde, so `staging` appears automatically; verify `render_file_line` reads sensibly for it and for `pending`.
- [ ] **Step 7: Full gate and commit.**

```bash
cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add rust/bins/peerbeam-cli
git commit -m "feat(cli): queue a chat file for an offline peer, with staging progress

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Flutter — Staging and Queued bubbles, cancel, Conversations

**Files:**
- Modify: `flutter/lib/sdk/models.dart` (`ChatStatusValue`, `ChatMessage.copyWith`)
- Modify: `flutter/lib/sdk/peerbeam.dart`, `flutter/lib/sdk/ffi/bindings.dart`
- Modify: `flutter/lib/data/chat_repository.dart`
- Modify: `flutter/lib/features/chat/chat_screen.dart` (`_FileBody`, `_deliveryGlyph`, `_statusLabel`)
- Modify: `flutter/lib/features/home/home_screen.dart`
- Modify: `flutter/test/sdk/fake_peerbeam.dart`
- Test: `flutter/test/chat_screen_test.dart`, `test/data/repository_test.dart`, `test/sdk/chat_test.dart`

**The literal matters.** `Status` serializes via `rename_all = "lowercase"` with no word splitting, so the new constant is:

```dart
  static const staging = 'staging';
```

`test/sdk/chat_test.dart` already has a test pinning every status literal — extend it, so a future rename breaks a test rather than silently breaking the UI.

- [ ] **Step 1: Write failing Dart tests** — a `staging` row renders "Staging…" with a determinate bar driven by the event's `progress`, and offers Cancel; a `pending` file row renders "Queued" and offers Cancel; a `sent` row offers neither; the Conversations list shows a peer that discovery cannot see; `ChatStatusValue.staging == 'staging'`.
- [ ] **Step 2: Run to see them fail.**
- [ ] **Step 3: Add the binding** — `_chatCancel`/`_chatConversations` fields, their `lookupFunction` lines, and the one-line `_withArg` wrappers, mirroring `_chatSendFile` exactly. Then `chatCancel`/`chatConversations` on `PeerBeamApi`, the real `PeerBeam`, and `FakePeerBeam`.
- [ ] **Step 4: Carry staging progress** — `ChatStatus` gains an optional `progress` record; `ChatRepository._onStatus` stores it per message id so `_FileBody` can render a determinate bar. Extend `ChatMessage.copyWith` to take `kind`/`fileName`/`fileSize` too — it currently only accepts `status` and `localPath`, which is enough today but not once a staging row learns its size late.
- [ ] **Step 5: Render the states** — `_statusLabel` gains `staging => 'Staging…'` and changes `pending` to `'Queued'` for a file row (text keeps 'Waiting'); `_deliveryGlyph` maps `staging` to `Icons.schedule`.
- [ ] **Step 6: Conversations list** — a section on Home built from `chatConversations()`, each row opening `ChatScreen` with the peer's real device id. This supersedes 2a's removal of saved-device chat: the id here is the authenticated one both halves already agree on, so the write-only-thread bug cannot recur.
- [ ] **Step 7: Both gates and commit.**

```bash
cd rust && cargo build -p peerbeam-ffi
cd ../flutter && flutter analyze && flutter test
cd ../rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add flutter/lib flutter/test
git commit -m "feat(flutter): staging and queued file bubbles, cancel, conversations list

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Docs

**Files:** `docs/MESSAGE_REGISTRY.md`, `docs/CLI.md`

- [ ] **Step 1: Registry** — record `FileDecline = 3` as implemented for Chat (0x0101) in the same bullet style the existing entries use, and `CHAT_FEAT_FILEDECLINE = 1 << 1` as the second assigned CHAT feature bit, noting a peer must advertise it before one is sent. Keep `Receipt`/`Reaction`/`Edit` reserved and unrenumbered.
- [ ] **Step 2: CLI.md** — document that `chat send --file` now **queues** for an unreachable peer instead of failing, the staging step and its two limits, `chat cancel`, and the fact that a peer lacking `CHAT_FEAT_FILEREF` is still a hard refusal rather than a queue.
- [ ] **Step 3: State the behaviour change** — 2a's docs say attaching to an unreachable peer fails. Find every such statement and correct it; a doc that describes the previous increment's behaviour is exactly the failure this project has already shipped once.
- [ ] **Step 4: Verify every claim against the built binary**, then commit.

```bash
cd rust && cargo fmt --check
git add docs/MESSAGE_REGISTRY.md docs/CLI.md
git commit -m "docs: file-in-chat 2b — queueing, staging, decline signalling

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage.** Staging with its cap/floor/eviction/sweep → T4 (+T7 sweep). One queue, additive `OutboxEntry`, one-file-in-flight, uniform send path → T3 + T6. `FileDecline` + feature bit + backstop → T5 + T6. Conversations list + reconcile fix → T2 + T7 + T10. Forward-compat containment → T1. `Status::Staging` and the lifecycle → T3, surfaced in T9/T10. Staging progress and cancellability → T4 (mechanism), T8 (events/export), T9/T10 (surfaces). Every test named in the spec's Testing section appears in T1, T4, T6, T7, T8, or T10.

**Ordering.** T1 must precede T3 because T3 adds a variant older binaries cannot decode. T2 must precede T7. T3 precedes T4/T6. T4 and T5 are independent of each other. T6 depends on T3/T4/T5. T8-T10 depend on T6. T11 last.

**Deferred items from 2a, explicitly resolved.** The once-only settle vs retry conflict → T6 Step 1 (`reopen_for_retry`, guard untouched). No `Cancelled` variant → kept; cancel lands on `Failed`/`cancelled`. Saved-device chat removal → superseded by T10's Conversations list. `reconcile_chat` text-only enumeration → T7. Forward-compat containment → T1. `validate_name`'s permissiveness → unchanged and out of scope; `display_name` already defuses bidi/control characters on the display path, and `sanitize_file_name` remains the disk authority.

**Type consistency.** `StagedFile { name, size, staged_path }` is used identically in T3, T4, T6. `StagingLimits { max_bytes, min_free_bytes }` matches `DeviceConfig { max_queued_file_bytes, min_free_bytes }` at the one call site that builds it. `Status::Staging` serializes `"staging"` in Rust (T3), is read as `ChatStatusValue.staging` in Dart (T10), and rendered by `status_str` in the CLI (T9). `CHAT_FEAT_FILEDECLINE` is advertised in both `session_cfg`s (T5) and read via `supports_file_decline()` in both.

**Known gap to watch during execution.** T6 Step 3 splits flush into "text sent here, file decided here" because `peerbeam-chat` has no transfer engine. If the implementer finds a cleaner seam that keeps text delivery byte-identical, take it — but text's path must not change shape, since two silent-message-loss bugs have already shipped through it.
