//! An AppStore-backed conversation store: one namespace per peer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::AppStore;

use crate::message::{ChatError, ChatMessage};
use crate::record::{ChatRecord, Direction, Kind, Status};

/// The AppStore namespace for a conversation with `peer`.
#[must_use]
pub fn namespace(peer: &DeviceId) -> String {
    format!("chat-{}", peer.0)
}

/// The AppStore namespace holding all undelivered outbound messages (across all
/// peers), keyed by message id (time-ordered), so `list` returns FIFO order.
///
/// Deliberately `.` — not `-` — as the 5th character. [`namespace`] always
/// produces `chat-<peer_id>` (dash), so as long as this constant's first five
/// characters are anything other than `chat-`, no per-peer conversation
/// namespace can ever collide with this one, *for any peer id string
/// whatsoever* — not just the ones tested. That matters because device ids
/// are peer-supplied over the wire (the handshake in
/// `peerbeam_transfer::auth` takes `device_id` verbatim from the peer's own
/// Hello), so a malicious or buggy peer can claim any id it likes, including
/// the literal string `"outbox"`. Before this constant used `.`, such a peer's
/// conversation namespace (`chat-outbox`) was byte-identical to this one,
/// silently corrupting the outbox (see
/// `outbox_namespace_never_collides_with_a_peer_literally_named_outbox`
/// below) — every past and future queued message would stay `Pending`
/// forever, on disk, with no error surfaced anywhere. Do not change this back
/// to a `-`.
pub const OUTBOX_NS: &str = "chat.outbox";

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

/// One queued outbound message awaiting delivery.
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

impl OutboxEntry {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    fn decode(bytes: &[u8]) -> Result<OutboxEntry, ChatError> {
        serde_json::from_slice(bytes).map_err(|e| ChatError::Serialization(e.to_string()))
    }
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

    /// One record from the conversation with `peer`, or `None` if absent.
    ///
    /// [`contains`](Self::contains) answers only "is this key taken", which is
    /// enough for receiver-side dedup but *not* enough for any caller that is
    /// about to mutate the record: a key can be occupied by a completely
    /// different row than the caller expects (a transfer id is peer-supplied,
    /// and a chat message id is a wire field the peer already knows). A caller
    /// that intends to write must load the record and check that it is the one
    /// it means — see `Manager::chat_settle` in `peerbeam-ffi`.
    pub fn get(&self, peer: &DeviceId, id: &str) -> Result<Option<ChatRecord>, ChatError> {
        let ns = namespace(peer);
        match self
            .store
            .get(&ns, id)
            .map_err(|e| ChatError::Serialization(e.to_string()))?
        {
            Some(bytes) => ChatRecord::decode(&bytes).map(Some),
            None => Ok(None),
        }
    }

    /// Persist an outgoing message as Pending and enqueue it to the outbox.
    pub fn enqueue(&self, peer: &DeviceId, msg: &ChatMessage) -> Result<(), ChatError> {
        self.append(&ChatRecord::out(peer, msg, Status::Pending))?;
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: msg.id.clone(),
            body: msg.body.clone(),
            timestamp: msg.timestamp.clone(),
            kind: Kind::Text,
            file: None,
            offers_refused: 0,
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
        for (key, value) in raw {
            match OutboxEntry::decode(&value) {
                Ok(entry) => out.push(entry),
                // Same rationale as `history`, but the blast radius is wider:
                // both `outbox_for` and `outbox_peers` read through this, so
                // one unreadable row must not take down delivery for every
                // other peer waiting behind it in the shared outbox.
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "skipping unreadable outbox entry");
                }
            }
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

    /// Record where a file record's bytes live on THIS device: the saved path
    /// on the receiver (the sender's own path is set at `prepare_file_send`
    /// time). Upserts in place, so `kind`, `status`, `name` and `size` all
    /// survive — the same read-modify-append shape as
    /// [`set_status`](Self::set_status), and for the same reason: rebuilding
    /// the record would silently drop whatever the caller did not think to
    /// carry over.
    ///
    /// A missing record, or one carrying no [`FileMeta`](crate::FileMeta) at
    /// all, is a no-op rather than an error — a completion event can outlive
    /// its row, and a text row has no path to set.
    pub fn set_file_path(
        &self,
        peer: &DeviceId,
        id: &str,
        local_path: &str,
    ) -> Result<(), ChatError> {
        let Some(mut rec) = self.get(peer, id)? else {
            return Ok(());
        };
        let Some(file) = rec.file.as_mut() else {
            return Ok(());
        };
        file.local_path = Some(local_path.to_string());
        self.append(&rec)
    }

    /// Guarded terminal-status write for a transfer's own chat row: settles
    /// `(peer, id)` to `status` only when the stored record passes
    /// [`ChatRecord::is_settleable_file_row`] for `expected_direction` — see
    /// its doc for why a bare key match at a peer-supplied transfer id is not
    /// authorization to write. Shared by every surface that bridges a
    /// transfer's terminal outcome onto a chat row (both the FFI's and the
    /// CLI's receive/send paths), so this authorization decision has exactly
    /// one implementation.
    ///
    /// A missing record, the wrong kind/direction, or an already-settled row
    /// is a silent no-op — no write. Returns whether a write happened, so a
    /// caller that also wants to record a local path can order it correctly:
    /// see [`set_file_row_path`](Self::set_file_row_path), which **must**
    /// run before this — both read the same in-flight leg of the guard, so
    /// once the row reads a terminal status this closes it to further writes.
    pub fn settle_file_row(
        &self,
        peer: &DeviceId,
        id: &str,
        expected_direction: Direction,
        status: Status,
    ) -> Result<bool, ChatError> {
        let Some(rec) = self.get(peer, id)? else {
            return Ok(false);
        };
        if !rec.is_settleable_file_row(expected_direction) {
            return Ok(false);
        }
        self.set_status(peer, id, status)?;
        Ok(true)
    }

    /// Guarded local-path write for a received chat file — the same
    /// authorization guard as [`settle_file_row`](Self::settle_file_row).
    /// **Must be called before** `settle_file_row`, not after: see that
    /// method's doc for why the ordering is load-bearing, not a style choice.
    /// A missing record, the wrong kind/direction, or an already-settled row
    /// is a silent no-op. Returns whether a write happened.
    pub fn set_file_row_path(
        &self,
        peer: &DeviceId,
        id: &str,
        expected_direction: Direction,
        local_path: &str,
    ) -> Result<bool, ChatError> {
        let Some(rec) = self.get(peer, id)? else {
            return Ok(false);
        };
        if !rec.is_settleable_file_row(expected_direction) {
            return Ok(false);
        }
        self.set_file_path(peer, id, local_path)?;
        Ok(true)
    }

    /// Guarded write of what a transfer says is **actually landing** — the
    /// same authorization guard as [`settle_file_row`](Self::settle_file_row),
    /// and the same ordering requirement: it **must run before** that call,
    /// because a settled row is deliberately closed to further writes.
    ///
    /// A receiving row's `name`/`size` start out as the peer's **`FileRef`
    /// claim**, made on the CHAT channel. The bytes ride a *separate* TRANSFER
    /// stream whose own `TransferMeta` decides what is written to disk, and the
    /// two are correlated **by id alone** — nothing anywhere forces them to
    /// agree. Without this reconciliation a peer can offer
    /// `holiday.jpg · 180 KB` in the conversation while streaming
    /// `invoice-2026.pdf.exe`, leaving a row permanently labelled with the
    /// first while its "open" target is the second: the bubble would lie about
    /// the file directly above the Accept button the user pressed.
    ///
    /// `name` is stored through [`display_name`](crate::display_name), like
    /// every other name a record carries. An empty `name` means the caller
    /// learned nothing and is a no-op — never a blanked row. Returns whether a
    /// write happened.
    pub fn set_file_row_landing(
        &self,
        peer: &DeviceId,
        id: &str,
        expected_direction: Direction,
        name: &str,
        size: u64,
    ) -> Result<bool, ChatError> {
        if name.is_empty() {
            return Ok(false);
        }
        let Some(mut rec) = self.get(peer, id)? else {
            return Ok(false);
        };
        if !rec.is_settleable_file_row(expected_direction) {
            return Ok(false);
        }
        let Some(file) = rec.file.as_mut() else {
            return Ok(false); // a File row always has one; belt and braces
        };
        file.name = crate::display_name(name);
        file.size = size;
        self.append(&rec)?;
        Ok(true)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::FileRef;
    use crate::record::{ChatRecord, Direction, FileMeta, Kind, Status};
    use crate::ChatMessage;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Arc;

    /// Builds a fresh store for a test, returning the `ChatStore` alongside
    /// the raw `Arc<dyn AppStore>` and the `TempDir` that backs it. The raw
    /// handle lets a test write a hand-crafted (e.g. deliberately
    /// undecodable) value directly, bypassing `ChatStore`'s own encode paths —
    /// something no test could do through `ChatStore` alone.
    fn new_store() -> (ChatStore, Arc<dyn AppStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[9u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> =
            Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app.clone()), app, dir)
    }

    #[test]
    fn append_then_history_is_chronological_and_survives_reopen() {
        let (cs, _store, dir) = new_store();
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
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").unwrap();
        assert!(!cs.contains(&peer, &m.id).unwrap());
        cs.append(&ChatRecord::received(&peer, &m)).unwrap();
        assert!(cs.contains(&peer, &m.id).unwrap());
    }

    #[test]
    fn conversations_are_isolated_by_peer() {
        let (cs, _store, _dir) = new_store();
        let a = DeviceId::from("pb-a");
        let b = DeviceId::from("pb-b");
        cs.append(&ChatRecord::sent(&a, &ChatMessage::new("to-a").unwrap()))
            .unwrap();
        assert_eq!(cs.history(&a).unwrap().len(), 1);
        assert_eq!(cs.history(&b).unwrap().len(), 0);
    }

    #[test]
    fn namespace_is_chat_dash_peer() {
        assert_eq!(namespace(&DeviceId::from("pb-x")), "chat-pb-x");
    }

    #[test]
    fn enqueue_persists_pending_record_and_outbox_entry() {
        let (cs, _store, _dir) = new_store();
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
        let (cs, _store, _dir) = new_store();
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

        let mut peers: Vec<String> = cs
            .outbox_peers()
            .unwrap()
            .into_iter()
            .map(|d| d.0)
            .collect();
        peers.sort();
        assert_eq!(peers, vec!["pb-a".to_string(), "pb-b".to_string()]);

        let a_only = cs.outbox_for(&a).unwrap();
        assert_eq!(a_only.len(), 2);
        assert_eq!(a_only[0].message_id, m1.id);
        assert_eq!(a_only[1].message_id, m3.id);
    }

    // Regression test for the outbox/conversation namespace collision: a peer
    // whose device id is literally "outbox" (device ids are peer-supplied
    // over the wire, so any peer can claim this) must never be able to
    // corrupt the shared outbox namespace, and `outbox_pending` must keep
    // succeeding (not error out) with exactly the unrelated peers' queued
    // entries. This is a REAL correctness assertion, not just "doesn't
    // panic": before the `OUTBOX_NS` fix, `namespace(&DeviceId::from("outbox"))`
    // and `OUTBOX_NS` were the same string (`"chat-outbox"`), so the `append`
    // below would have landed a `ChatRecord` inside the outbox namespace;
    // `OutboxEntry::decode` would then hard-fail on it (no `message_id`
    // field), and `outbox_pending` would return `Err`, taking down every
    // caller that swallows that error via `.unwrap_or_default()` (the drain
    // loop, flush-on-connect, the opportunistic flush) — permanently and
    // silently.
    #[test]
    fn outbox_namespace_never_collides_with_a_peer_literally_named_outbox() {
        let (cs, _store, _dir) = new_store();

        // A normal peer with a message queued for offline delivery.
        let normal_peer = DeviceId::from("pb-bob");
        let msg = ChatMessage::new("queued for bob").unwrap();
        cs.enqueue(&normal_peer, &msg).unwrap();

        // A peer that has claimed the device id "outbox" — nothing stops a
        // peer from presenting any id it likes in its Hello. It has its own
        // (unrelated) conversation history, e.g. a message it sent us.
        let outbox_named_peer = DeviceId::from("outbox");
        cs.append(&ChatRecord::received(
            &outbox_named_peer,
            &ChatMessage::new("hi from a peer named outbox").unwrap(),
        ))
        .unwrap();

        // The real outbox must still be readable at all (the old bug made
        // this an `Err`) and must contain exactly — and only — the normal
        // peer's queued entry.
        let pending = cs
            .outbox_pending()
            .expect("outbox_pending must succeed even when a peer is literally named \"outbox\"");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].peer_id, "pb-bob");
        assert_eq!(pending[0].message_id, msg.id);
        assert_eq!(pending[0].body, "queued for bob");

        // And the "outbox"-named peer's own conversation is intact and
        // isolated — unaffected by (and not polluting) the real outbox.
        let their_history = cs.history(&outbox_named_peer).unwrap();
        assert_eq!(their_history.len(), 1);
        assert_eq!(their_history[0].body, "hi from a peer named outbox");
    }

    #[test]
    fn record_sent_flips_pending_to_sent_in_place_and_remove_dequeues() {
        let (cs, _store, _dir) = new_store();
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

    #[test]
    fn set_status_updates_in_place() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("a.bin", 3).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        cs.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring))
            .unwrap();
        cs.set_status(&peer, &r.id, Status::Sent).unwrap();
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "upsert, not a second row");
        assert_eq!(hist[0].status, Status::Sent);
        assert_eq!(hist[0].kind, Kind::File, "kind survives a status change");
        // Absent id is a no-op, not an error.
        assert!(cs.set_status(&peer, "nope", Status::Failed).is_ok());
    }

    /// `get` is what lets a caller check *which* row occupies a key before
    /// writing to it — `contains` cannot tell an Out/Text row from an
    /// In/File one.
    #[test]
    fn get_returns_the_whole_record_or_none() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        assert!(cs.get(&peer, "nope").unwrap().is_none());

        let m = ChatMessage::new("hello").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();
        let got = cs.get(&peer, &m.id).unwrap().expect("record present");
        assert_eq!(got.kind, Kind::Text);
        assert_eq!(got.direction, Direction::Out);
        assert_eq!(got.status, Status::Sent);
        assert_eq!(got.body, "hello");
    }

    #[test]
    fn set_file_path_upserts_in_place_and_no_ops_on_absent_or_text_rows() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap(); // In/File/PendingApproval

        cs.set_file_path(&peer, &r.id, "/home/me/Downloads/report.pdf")
            .unwrap();

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "upsert, not a second row");
        let meta = hist[0].file.clone().expect("file meta survives");
        assert_eq!(
            meta.local_path.as_deref(),
            Some("/home/me/Downloads/report.pdf")
        );
        assert_eq!(meta.name, "report.pdf", "name survives");
        assert_eq!(meta.size, 4096, "size survives");
        assert_eq!(
            hist[0].status,
            Status::PendingApproval,
            "status is not touched"
        );

        // A missing record is a no-op, not an error.
        assert!(cs.set_file_path(&peer, "nope", "/tmp/x").is_ok());
        // A text record has no path to set: no-op, and no phantom FileMeta.
        let m = ChatMessage::new("hi").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();
        assert!(cs.set_file_path(&peer, &m.id, "/tmp/x").is_ok());
        let text = cs.get(&peer, &m.id).unwrap().unwrap();
        assert!(text.file.is_none(), "a text row gains no file metadata");
        assert_eq!(text.body, "hi");
    }

    #[test]
    fn reconcile_marks_mid_flight_records_interrupted() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let a = FileRef::new("a.bin", 1).unwrap();
        let b = FileRef::new("b.bin", 1).unwrap();
        let m = |r: &FileRef| FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        cs.append(&ChatRecord::file_out(
            &peer,
            &a,
            m(&a),
            Status::Transferring,
        ))
        .unwrap();
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
    /// silently drop the additive kind/file for a Text record too. `enqueue` only
    /// ever produces Text entries, so this alone cannot catch a File-dropping bug
    /// (see `outbox_round_trip_preserves_a_file_records_additive_fields` for that) —
    /// it exists to keep the plain Text round trip covered.
    #[test]
    fn outbox_round_trip_preserves_additive_record_fields() {
        let (cs, _store, _dir) = new_store();
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

    /// Discriminating regression guard for `record_sent`: `ChatStore::enqueue` only
    /// accepts a `ChatMessage`, so nothing that goes through the real outbox
    /// pipeline can ever be `Kind::File` — a test that only exercises `enqueue` +
    /// `outbox_for` + `record_sent` (as the Text test above does) would pass
    /// identically whether `record_sent` preserves additive fields or blindly
    /// reconstructs a Text record from `OutboxEntry`'s four fields, because both
    /// produce a Text/None result either way.
    ///
    /// So this test bypasses `enqueue`: it persists a `Kind::File` conversation
    /// record directly, then hand-builds an `OutboxEntry` that points at that same
    /// record (same peer, same id) and feeds it to `record_sent` — reproducing the
    /// real shape a future file-transfer flush would have, without waiting for that
    /// feature to exist. Only the read-existing-record fix can pass this; the
    /// reconstruct-always version provably cannot (verified by hand: temporarily
    /// reverting `record_sent` to always rebuild `ChatRecord { .. kind: Kind::Text,
    /// file: None }` makes this test fail on the `kind`/`file` assertions below).
    #[test]
    fn outbox_round_trip_preserves_a_file_records_additive_fields() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some("/tmp/report.pdf".into()),
        };
        cs.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring))
            .unwrap();

        // Hand-built outbox entry pointing at that same File record — `enqueue`
        // cannot produce this shape today, since it only takes a `ChatMessage`.
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: r.id.clone(),
            body: String::new(),
            timestamp: r.timestamp.clone(),
            kind: Kind::Text,
            file: None,
            offers_refused: 0,
        };
        cs.record_sent(&entry).unwrap();

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "upsert, not a second row");
        assert_eq!(hist[0].id, r.id);
        assert_eq!(hist[0].status, Status::Sent);
        assert_eq!(hist[0].kind, Kind::File, "kind must survive record_sent");
        let file = hist[0]
            .file
            .clone()
            .expect("file meta must survive record_sent");
        assert_eq!(
            file.name, "report.pdf",
            "a defaulted FileMeta could not pass this"
        );
        assert_eq!(file.size, 4096);
        assert_eq!(file.local_path.as_deref(), Some("/tmp/report.pdf"));
    }

    // ── settle_file_row / set_file_row_path — the receive-side chat bridge's
    // authorization guard, shared by every surface (FFI + CLI) that mirrors a
    // transfer's terminal outcome onto a chat row. ─────────────────────────

    /// The positive case a legitimate receive must still hit: a chat file
    /// offer (`In`/`File`/`PendingApproval`) settles to `Received` and picks
    /// up where it landed — proving the guard cannot pass merely by refusing
    /// everything.
    #[test]
    fn settle_file_row_settles_a_genuine_receive_and_records_its_path() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap(); // In/File/PendingApproval

        // Ordering matters: path before status, mirroring the real caller.
        let path_written = cs
            .set_file_row_path(&peer, &r.id, Direction::In, "/home/me/Downloads/report.pdf")
            .unwrap();
        assert!(path_written);
        let settled = cs
            .settle_file_row(&peer, &r.id, Direction::In, Status::Received)
            .unwrap();
        assert!(settled);

        let rec = cs.get(&peer, &r.id).unwrap().expect("row");
        assert_eq!(rec.status, Status::Received);
        assert_eq!(
            rec.file.unwrap().local_path.as_deref(),
            Some("/home/me/Downloads/report.pdf")
        );
    }

    /// The equally-legitimate send-side case: `Out`/`File`/`Transferring`
    /// settles to `Sent` (no path — the sender's path was already set at
    /// `prepare_file_send` time).
    #[test]
    fn settle_file_row_settles_a_genuine_send_completion() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some("/tmp/report.pdf".into()),
        };
        cs.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring))
            .unwrap();

        let settled = cs
            .settle_file_row(&peer, &r.id, Direction::Out, Status::Sent)
            .unwrap();
        assert!(settled);
        assert_eq!(cs.get(&peer, &r.id).unwrap().unwrap().status, Status::Sent);
    }

    /// The ordinary case, by far the most common: a ".receive completes" a
    /// plain (non-chat) transfer whose id has no row at all. Must be a
    /// silent no-op — no row is ever invented.
    #[test]
    fn settle_file_row_is_silent_when_no_row_exists() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = cs
            .settle_file_row(
                &peer,
                "tx-plain-transfer-id",
                Direction::In,
                Status::Received,
            )
            .unwrap();
        assert!(!settled);
        assert!(cs.get(&peer, "tx-plain-transfer-id").unwrap().is_none());
    }

    /// The hostile case (a): an already-paired peer opens an ordinary
    /// transfer whose peer-supplied `transfer_id` collides with the id of our
    /// own OUTBOUND TEXT message in that thread. Without the guard this would
    /// stamp our own "sent" text as `Received`. Must be a complete no-op.
    #[test]
    fn settle_file_row_is_silent_for_a_hostile_collision_with_a_text_row() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let msg = ChatMessage::new("hello there").unwrap();
        cs.append(&ChatRecord::sent(&peer, &msg)).unwrap(); // Out/Text/Sent

        let settled = cs
            .settle_file_row(&peer, &msg.id, Direction::In, Status::Received)
            .unwrap();
        assert!(!settled, "a text row must never be settled as a file row");

        let rec = cs.get(&peer, &msg.id).unwrap().expect("row still present");
        assert_eq!(rec.kind, Kind::Text, "kind must be untouched");
        assert_eq!(rec.status, Status::Sent, "status must be untouched");
        assert_eq!(rec.body, "hello there", "body must be untouched");
    }

    /// The hostile case (b): a peer-supplied `transfer_id` collides with the
    /// id of a FILE row we already settled (e.g. one we `Declined`). Without
    /// the guard this would flip a declined file back to `Received` while
    /// keeping its old metadata. Must be a complete no-op.
    #[test]
    fn settle_file_row_is_silent_for_a_hostile_collision_with_an_already_settled_file_row() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("suspicious.exe", 4096).unwrap();
        let mut declined = ChatRecord::file_in(&peer, &r);
        declined.status = Status::Declined;
        cs.append(&declined).unwrap();

        let settled = cs
            .settle_file_row(&peer, &r.id, Direction::In, Status::Received)
            .unwrap();
        assert!(!settled, "an already-settled row must never be re-settled");

        let rec = cs.get(&peer, &r.id).unwrap().expect("row still present");
        assert_eq!(
            rec.status,
            Status::Declined,
            "a declined file must not flip to Received"
        );
    }

    /// A direction mismatch is exactly as hostile as a kind mismatch: our own
    /// OUTBOUND file share must not be settleable as if it were a receive.
    #[test]
    fn settle_file_row_is_silent_for_a_direction_mismatch() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some("/tmp/report.pdf".into()),
        };
        cs.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring))
            .unwrap();

        let settled = cs
            .settle_file_row(&peer, &r.id, Direction::In, Status::Received)
            .unwrap();
        assert!(!settled);
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Transferring,
            "an outbound row must not be settled by a wrong-direction (receive) call"
        );
    }

    // ── set_file_row_landing — the FileRef-claim vs. TransferMeta-reality
    // reconciliation. ────────────────────────────────────────────────────────

    /// The mismatch this exists for: the peer's CHAT-channel `FileRef` says
    /// one thing, the TRANSFER stream lands another, and the two are
    /// correlated by id alone. The settled row must describe what is on disk,
    /// not what was advertised — and the name must be render-safe.
    #[test]
    fn set_file_row_landing_replaces_the_peers_claim_with_what_actually_landed() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        // What the peer put in the conversation.
        let mut r = FileRef::new("holiday.jpg", 184_320).unwrap();
        r.name = "holiday.jpg".into();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        // What the transfer actually wrote — a different name, a different
        // size, and a bidi override for good measure.
        let wrote = cs
            .set_file_row_landing(
                &peer,
                &r.id,
                Direction::In,
                "invoice-2026.pdf\u{202E}exe.",
                4_096,
            )
            .unwrap();
        assert!(wrote);
        // Ordering: landing first, then settle (both share the in-flight leg).
        assert!(cs
            .set_file_row_path(&peer, &r.id, Direction::In, "/home/me/Downloads/x")
            .unwrap());
        assert!(cs
            .settle_file_row(&peer, &r.id, Direction::In, Status::Received)
            .unwrap());

        let meta = cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap();
        assert_eq!(meta.name, "invoice-2026.pdf\u{FFFD}exe.");
        assert_eq!(meta.size, 4_096);
        assert_eq!(meta.local_path.as_deref(), Some("/home/me/Downloads/x"));
    }

    /// It carries the identical guard as its siblings: a text row is never a
    /// transfer's business, an already-settled row is final, and a direction
    /// mismatch means the peer is reaching for our own outbound row. All three
    /// are silent no-ops.
    #[test]
    fn set_file_row_landing_is_silent_for_every_row_that_is_not_its_own() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");

        // (a) A text row.
        let msg = ChatMessage::new("hello there").unwrap();
        cs.append(&ChatRecord::sent(&peer, &msg)).unwrap();
        assert!(!cs
            .set_file_row_landing(&peer, &msg.id, Direction::In, "evil.exe", 1)
            .unwrap());
        let text = cs.get(&peer, &msg.id).unwrap().unwrap();
        assert_eq!(text.body, "hello there");
        assert!(text.file.is_none(), "no phantom FileMeta");

        // (b) An already-settled (declined) file row.
        let declined_ref = FileRef::new("suspicious.exe", 4096).unwrap();
        let mut declined = ChatRecord::file_in(&peer, &declined_ref);
        declined.status = Status::Declined;
        cs.append(&declined).unwrap();
        assert!(!cs
            .set_file_row_landing(&peer, &declined_ref.id, Direction::In, "evil.exe", 1)
            .unwrap());
        assert_eq!(
            cs.get(&peer, &declined_ref.id)
                .unwrap()
                .unwrap()
                .file
                .unwrap()
                .name,
            "suspicious.exe"
        );

        // (c) Our own OUTBOUND row, reached for by a receive.
        let out_ref = FileRef::new("mine.pdf", 10).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &out_ref,
            FileMeta::new(&out_ref.name, out_ref.size, Some("/tmp/mine.pdf".into())),
            Status::Transferring,
        ))
        .unwrap();
        assert!(!cs
            .set_file_row_landing(&peer, &out_ref.id, Direction::In, "evil.exe", 1)
            .unwrap());
        assert_eq!(
            cs.get(&peer, &out_ref.id)
                .unwrap()
                .unwrap()
                .file
                .unwrap()
                .name,
            "mine.pdf"
        );

        // (d) No row at all — the ordinary transfer, by far the common case.
        assert!(!cs
            .set_file_row_landing(&peer, "tx-plain", Direction::In, "x.bin", 1)
            .unwrap());
        assert!(cs.get(&peer, "tx-plain").unwrap().is_none());
    }

    /// "Learned nothing" must never blank a row: an empty name is a no-op, so
    /// a caller whose peek failed can call unconditionally.
    #[test]
    fn set_file_row_landing_ignores_an_empty_name() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        assert!(!cs
            .set_file_row_landing(&peer, &r.id, Direction::In, "", 0)
            .unwrap());
        let meta = cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap();
        assert_eq!(meta.name, "report.pdf");
        assert_eq!(meta.size, 4096);
    }

    /// `set_file_row_path` shares the identical guard: a text row (no `file`
    /// to set a path on, and the wrong `kind` regardless) must not gain one.
    #[test]
    fn set_file_row_path_is_silent_for_a_text_row() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let msg = ChatMessage::new("hi").unwrap();
        cs.append(&ChatRecord::sent(&peer, &msg)).unwrap();

        let written = cs
            .set_file_row_path(&peer, &msg.id, Direction::In, "/tmp/x")
            .unwrap();
        assert!(!written);
        assert!(cs.get(&peer, &msg.id).unwrap().unwrap().file.is_none());
    }

    /// The ordering requirement, proven directly: once a row is already
    /// settled (`Received`), `set_file_row_path` must refuse to touch it —
    /// it shares the same in-flight leg of the guard as `settle_file_row`, so
    /// calling it AFTER settling (the wrong order) silently does nothing
    /// instead of overwriting an already-final row.
    #[test]
    fn set_file_row_path_is_silent_once_the_row_is_already_settled() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap();
        cs.settle_file_row(&peer, &r.id, Direction::In, Status::Received)
            .unwrap();

        let written = cs
            .set_file_row_path(&peer, &r.id, Direction::In, "/late/path")
            .unwrap();
        assert!(!written);
        assert!(
            cs.get(&peer, &r.id)
                .unwrap()
                .unwrap()
                .file
                .unwrap()
                .local_path
                .is_none(),
            "a path written after settling must not land"
        );
    }

    // ── Containment — one bad row must not poison a whole namespace. A
    // future schema addition (e.g. a `Status` variant a 2a binary cannot
    // decode) must lose only the row it lands on, never the rest of the
    // conversation or the whole outbox. ────────────────────────────────────

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
            .put(
                &namespace(&peer),
                "0000000000002",
                b"{\"status\":\"from-the-future\"}",
            )
            .unwrap();
        cs.append(&later).unwrap();

        let got = cs
            .history(&peer)
            .expect("one bad row must not fail the conversation");
        let bodies: Vec<&str> = got.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(bodies, vec!["before", "after"]);
    }

    #[test]
    fn outbox_pending_skips_an_undecodable_entry_so_delivery_survives() {
        let (cs, store, _tmp) = new_store();
        let peer = DeviceId::from("pb-bob".to_string());
        cs.enqueue(&peer, &ChatMessage::new("real").unwrap())
            .unwrap();
        store
            .put(OUTBOX_NS, "0000000000009", b"not an outbox entry")
            .unwrap();

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
}
