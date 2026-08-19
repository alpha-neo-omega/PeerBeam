//! An AppStore-backed conversation store: one namespace per peer.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::AppStore;

use crate::message::{ChatError, ChatMessage, FileDecline, FileRef};
use crate::record::{ChatRecord, Direction, FileMeta, Kind, Status, StoredReaction};

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

    /// The namespace holding landings that arrived before their row.
    fn landing_ns(peer: &str) -> String {
        format!("chat.landing-{peer}")
    }

    /// Persist a record under its conversation namespace, keyed by its id.
    ///
    /// An incoming file row is reconciled against any **landing already known
    /// for its id** before it is written. The two facts arrive on different
    /// channels — the peer's claim on CHAT, what actually lands on TRANSFER —
    /// and nothing orders them. When the transfer's meta wins the race, the
    /// landing has nowhere to go yet; it is parked, and applied here. Without
    /// that, whether the user is shown the real name or the sender's chosen one
    /// depends on which frame arrives first, and on Windows the claim won.
    pub fn append(&self, rec: &ChatRecord) -> Result<(), ChatError> {
        let mut rec = rec.clone();
        if rec.is_settleable_file_row(Direction::In) {
            if let Some((name, size)) = self.take_pending_landing(&rec.peer_id, &rec.id)? {
                if let Some(file) = rec.file.as_mut() {
                    file.name = crate::display_name(&name);
                    file.size = size;
                }
            }
        }
        let ns = format!("chat-{}", rec.peer_id);
        self.store
            .put(&ns, &rec.id, &rec.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// Park a landing whose row does not exist yet.
    fn park_pending_landing(
        &self,
        peer: &str,
        id: &str,
        name: &str,
        size: u64,
    ) -> Result<(), ChatError> {
        let blob = serde_json::to_vec(&(name, size))
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        self.store
            .put(&Self::landing_ns(peer), id, &blob)
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// Take a parked landing, if one is waiting for this id.
    fn take_pending_landing(
        &self,
        peer: &str,
        id: &str,
    ) -> Result<Option<(String, u64)>, ChatError> {
        let ns = Self::landing_ns(peer);
        let Ok(Some(blob)) = self.store.get(&ns, id) else {
            return Ok(None);
        };
        // Removed whether or not it decodes: a landing that cannot be read is
        // not going to become readable, and leaving it would re-apply nothing
        // forever.
        let _ = self.store.delete(&ns, id);
        Ok(serde_json::from_slice::<(String, u64)>(&blob).ok())
    }

    /// Every peer this device has a conversation with, ascending by id.
    ///
    /// Derived from the namespaces that actually exist rather than from a
    /// separate index, which could drift from reality and silently hide a
    /// thread — the failure this exists to prevent is a conversation nothing
    /// at startup can name.
    ///
    /// [`namespace`] always emits `chat-<id>` (a dash) and the outbox is
    /// [`OUTBOX_NS`] — `chat.outbox`, whose fifth character is a dot precisely
    /// so the two spaces can never overlap — so this prefix scan cannot pick
    /// the outbox up. A peer that claims the device id `outbox` still appears,
    /// correctly, as its own conversation: that is `chat-outbox`, a different
    /// namespace entirely.
    ///
    /// [`AppStore::namespaces`] reports only *populated* namespaces, so an
    /// empty directory — left by a `clear`, or by a crash between
    /// `create_dir_all` and the first record — is not mistaken for a thread.
    pub fn conversations(&self) -> Result<Vec<DeviceId>, ChatError> {
        let names = self
            .store
            .namespaces("chat-")
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(names
            .into_iter()
            .filter_map(|ns| {
                ns.strip_prefix("chat-")
                    .map(|id| DeviceId::from(id.to_string()))
            })
            .collect())
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

    /// Queue an already-staged file for delivery to `peer`, and mark our own
    /// row `Pending`. Returns whether it was queued: `false` means the row is
    /// gone — the conversation was deleted while we staged — and **nothing at
    /// all** was written.
    ///
    /// The conversation row already exists — [`begin_file_send`] wrote it
    /// `Staging` before the copy started, so a multi-GB stage is visible rather
    /// than looking like a hung attach — which is why this upserts that row
    /// instead of rebuilding one: a rebuild would drop the `local_path` the
    /// sender's own "Open" depends on.
    ///
    /// The row's size is corrected to the bytes **actually staged**. A source
    /// being appended to while we copied makes the metadata and the blob
    /// disagree, and the blob is what will be delivered; showing the other
    /// number would be a row that lies about its own file.
    ///
    /// # Two orderings, both load-bearing
    ///
    /// **The row is READ before the entry is written**, and a row that has gone
    /// queues nothing. A queue entry whose record does not exist is the one
    /// shape the drain reads as "nothing will ever settle this": it offers the
    /// `FileRef` to the peer, then finds no row to re-open, then releases the
    /// entry and deletes the staged blob (`run_queued_file` and
    /// `drop_queued_file` in `peerbeam_ffi::transfer`). The user would lose the
    /// only copy the queue owns *and* leave the peer holding an approval prompt
    /// for a stream that never comes. Writing the entry first and then
    /// discovering the row is missing — which is what this used to do — creates
    /// exactly that state and cannot undo it, because by then the entry is on
    /// disk and any drain tick may have taken it. So the check comes first and
    /// the caller is told, which lets [`stage_file_send`] delete the blob it
    /// staged and honour the delete completely.
    ///
    /// **The entry is written before the row**, once we have committed to
    /// queueing. A crash between the two leaves a queued entry whose row still
    /// reads `Staging`, which the drain re-opens and sends
    /// ([`reopen_for_retry`](Self::reopen_for_retry)). The reverse order would
    /// leave a `Pending` row with nothing queued to deliver it — a file the
    /// user is told is waiting that nothing will ever send.
    ///
    /// [`begin_file_send`]: crate::begin_file_send
    /// [`stage_file_send`]: crate::stage_file_send
    pub fn enqueue_file(
        &self,
        peer: &DeviceId,
        r: &FileRef,
        staged: &StagedFile,
    ) -> Result<bool, ChatError> {
        let Some(mut rec) = self.get(peer, &r.id)? else {
            return Ok(false);
        };
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: r.id.clone(),
            body: String::new(),
            timestamp: r.timestamp.clone(),
            kind: Kind::File,
            file: Some(staged.clone()),
            offers_refused: 0,
        };
        self.store
            .put(OUTBOX_NS, &r.id, &entry.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        rec.status = Status::Pending;
        if let Some(file) = rec.file.as_mut() {
            file.name = crate::display_name(&staged.name);
            file.size = staged.size;
        }
        self.append(&rec)?;
        Ok(true)
    }

    /// Queue "I turned your file down" for a sender we could not tell live.
    ///
    /// A decline is best-effort by design — the sender's own bounded backstop
    /// is what guarantees a refused file stops being re-offered — so this is
    /// only the path taken when the direct send failed, i.e. the sender dropped
    /// while our approval prompt was open. Returns whether it was queued.
    ///
    /// **It refuses to overwrite an occupied outbox key.** `d.id` is the
    /// *sender's* `FileRef.id`: a value the peer chose. The outbox is keyed by
    /// message id, so a peer that picked the id of a message we already have
    /// queued for it would otherwise replace that message with this decline and
    /// we would never deliver it — silent message loss, driven from the wire.
    pub fn enqueue_decline(&self, peer: &DeviceId, d: &FileDecline) -> Result<bool, ChatError> {
        let occupied = self
            .store
            .get(OUTBOX_NS, &d.id)
            .map_err(|e| ChatError::Serialization(e.to_string()))?
            .is_some();
        if occupied {
            return Ok(false);
        }
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: d.id.clone(),
            body: String::new(),
            timestamp: d.timestamp.clone(),
            kind: Kind::Decline,
            file: None,
            offers_refused: 0,
        };
        self.store
            .put(OUTBOX_NS, &d.id, &entry.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(true)
    }

    /// Count one offer that **reached** the peer and was refused (or timed out)
    /// at its approval gate, returning the new total.
    ///
    /// The bounded backstop this feeds exists for peers too old to send a
    /// `FileDecline`: without it a refused file is re-offered on every drain
    /// tick, re-prompting its receiver forever. What it counts is deliberately
    /// narrow — see the caller in `peerbeam-ffi`. A connection failure never
    /// gets here: nobody saw the offer, nobody was prompted, and keep-forever is
    /// the promise text already makes.
    ///
    /// A missing entry counts nothing and returns 0: the file is no longer
    /// queued, so there is no budget left to spend on it.
    pub fn outbox_bump_refused(&self, message_id: &str) -> Result<u32, ChatError> {
        let Some(bytes) = self
            .store
            .get(OUTBOX_NS, message_id)
            .map_err(|e| ChatError::Serialization(e.to_string()))?
        else {
            return Ok(0);
        };
        let mut entry = OutboxEntry::decode(&bytes)?;
        entry.offers_refused = entry.offers_refused.saturating_add(1);
        let now = entry.offers_refused;
        self.store
            .put(OUTBOX_NS, message_id, &entry.encode())
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(now)
    }

    /// Re-open our own outgoing file row for another delivery attempt.
    ///
    /// Deliberately NOT routed through
    /// [`settle_file_row`](Self::settle_file_row): that guard admits only
    /// in-flight rows, which is exactly what makes a wire-driven settle
    /// once-only, and relaxing it would let a peer resurrect a row it had
    /// already settled. This is reachable only from the local drain, on a row we
    /// ourselves queued, under an id we minted — the same shape as
    /// `Manager::fail_chat_file`, which already writes unguarded for the same
    /// reason. Returns whether it re-opened.
    ///
    /// The states it accepts are every non-final state an outgoing file row can
    /// be sitting in when its entry is still queued:
    ///
    /// * `Pending` — queued, never attempted (the ordinary case);
    /// * `Failed` — a previous attempt did not deliver;
    /// * `Staging` — a crash landed between `enqueue_file`'s entry write and its
    ///   row write; the blob is complete (staging only returns after the copy
    ///   succeeded), so this is deliverable and must not be stranded;
    /// * `Interrupted` — a restart's reconcile settled a row whose transfer died
    ///   with the process, while its entry stayed queued.
    ///
    /// `Sent` and `Declined` are final — a delivered file is not re-sent, and a
    /// refused one is not re-offered — and `Transferring` means an attempt is
    /// already live.
    pub fn reopen_for_retry(&self, peer: &DeviceId, id: &str) -> Result<bool, ChatError> {
        let Some(mut rec) = self.get(peer, id)? else {
            return Ok(false);
        };
        if rec.kind != Kind::File || rec.direction != Direction::Out {
            return Ok(false);
        }
        if !matches!(
            rec.status,
            Status::Pending | Status::Failed | Status::Staging | Status::Interrupted
        ) {
            return Ok(false);
        }
        rec.status = Status::Transferring;
        self.append(&rec)?;
        Ok(true)
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

    /// Every staged blob the queue currently owns — or an error when that set
    /// cannot be established **completely**.
    ///
    /// This is the one outbox reader that does *not* skip a row it cannot
    /// decode, and the difference is the whole reason it exists as a separate
    /// call rather than a `filter_map` over
    /// [`outbox_pending`](Self::outbox_pending).
    ///
    /// Every other reader exists to **deliver**. There, skipping an unreadable
    /// entry costs that one message and saves every other message queued
    /// behind it in the shared outbox — containment is a strict improvement.
    /// This one exists to decide what to **delete**:
    /// [`StagingStore::sweep`](crate::StagingStore::sweep) removes every blob
    /// its `keep` set does not name, so a set that is merely *incomplete* here
    /// does not lose a row — it destroys the bytes of a file the user queued,
    /// permanently, while that file's conversation row still says it is
    /// waiting to be sent.
    ///
    /// The sharp case is an outbox that is *wholly* unreadable: `outbox_pending`
    /// answers `Ok(vec![])` for it, which is indistinguishable from a queue
    /// that is genuinely empty, and handing that to `sweep` would delete every
    /// staged file on the device — at startup, before the user has done
    /// anything. So the answer here is `Err` the moment a single entry fails to
    /// decode: an entry we cannot read may well own a blob, and there is no way
    /// to learn which one.
    ///
    /// The failure mode is therefore *leaking* bytes, never destroying them —
    /// orphans survive until the outbox is readable again. That asymmetry is
    /// deliberate: an orphan costs disk, a wrongly-swept blob costs the user
    /// their file.
    pub fn outbox_owned_blobs(&self) -> Result<HashSet<String>, ChatError> {
        let raw = self
            .store
            .list(OUTBOX_NS)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        let mut owned = HashSet::with_capacity(raw.len());
        for (key, value) in raw {
            let entry = OutboxEntry::decode(&value).map_err(|e| {
                ChatError::Serialization(format!(
                    "outbox entry {key} is unreadable, so the set of staged files \
                     still owned cannot be established: {e}"
                ))
            })?;
            if let Some(file) = entry.file {
                owned.insert(file.staged_path);
            }
        }
        Ok(owned)
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
    /// (`kind`, `file`) survive; rebuilding from `OutboxEntry`'s four original
    /// fields would silently drop them.
    ///
    /// The fallback below — no record at all under this id — used to hardcode
    /// `kind: Text, file: None`, which was harmless only while `enqueue` was the
    /// sole producer of an `OutboxEntry` and therefore every entry really was
    /// text. `enqueue_file` makes that branch reachable, and a hardcoded text
    /// rebuild would turn a delivered *file* into a text row with no name, no
    /// size and no file metadata at all. So the rebuild honours the entry's own
    /// `kind`/`file`.
    ///
    /// The rebuilt file row deliberately carries **no** `local_path`: the only
    /// path this entry knows is the staged blob's, which is deleted the moment
    /// the send settles, so recording it would give the row an "Open" that
    /// points at nothing. The sender's real path lives on the row written at
    /// `begin_file_send` time — which is why this is a fallback for a row that
    /// has gone missing, not the normal path.
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
        let (kind, file) = match (entry.kind, entry.file.as_ref()) {
            (Kind::File, Some(staged)) => (
                Kind::File,
                Some(FileMeta::new(&staged.name, staged.size, None)),
            ),
            // A `File` entry with no staged blob cannot describe a file; record
            // what is certain (that something was delivered under this id)
            // rather than inventing metadata.
            (Kind::File, None) => (Kind::Text, None),
            // A decline is a status change on someone else's row, never a row of
            // its own — `flush_to_session` never calls this for one. If some
            // future caller does, writing nothing is the honest outcome.
            (Kind::Decline, _) => return Ok(()),
            (Kind::Text, _) => (Kind::Text, None),
        };
        self.append(&ChatRecord {
            id: entry.message_id.clone(),
            peer_id: entry.peer_id.clone(),
            direction: Direction::Out,
            timestamp: entry.timestamp.clone(),
            body: entry.body.clone(),
            status: Status::Sent,
            kind,
            file,
            read_at: None,
            reactions: Vec::new(),
        })
    }

    /// Apply a read watermark: mark every one of **our own outgoing** messages
    /// up to and including `read_through` as read, and report how many rows
    /// that changed.
    ///
    /// Only outgoing rows, because a receipt is the peer telling us it read
    /// something *we* sent — the same direction check
    /// [`settle_file_row`](Self::settle_file_row) makes, and for the same
    /// reason: a peer must not be able to rewrite a row it sent us. Scoped to
    /// that peer's namespace, so a receipt cannot reach another conversation.
    ///
    /// Idempotent and monotonic: a row already marked read is left alone, so
    /// re-applying the same watermark — or an older one arriving late out of
    /// order — changes nothing and cannot move a read row back to unread.
    ///
    /// Comparison is on the id, which [`crate::mint_id`] makes lexicographically
    /// time-ordered, so one id names a prefix of the conversation. A row whose
    /// id did not come from `mint_id` (only reachable from another
    /// implementation) simply compares as the string it is; it can be missed by
    /// a watermark but never wrongly marked, since the bound is inclusive and
    /// one-sided.
    pub fn apply_receipt(
        &self,
        peer: &DeviceId,
        read_through: &str,
        at: &str,
    ) -> Result<usize, ChatError> {
        let mut changed = 0;
        for mut rec in self.history(peer)? {
            if rec.direction != Direction::Out
                || rec.read_at.is_some()
                || rec.id.as_str() > read_through
            {
                continue;
            }
            rec.read_at = Some(at.to_string());
            self.append(&rec)?;
            changed += 1;
        }
        Ok(changed)
    }

    /// Add or withdraw a reaction on one message, in place.
    ///
    /// Authorization is the record, not the id — the same rule
    /// [`settle_file_row`](Self::settle_file_row) applies, and for the same
    /// reason: `target_id` arrives from the peer, so a bare key match is not
    /// permission to write. The lookup is scoped to that peer's own namespace,
    /// so a peer can only ever react inside its own conversation, and a
    /// `target_id` naming a message in a different conversation finds nothing
    /// here rather than reaching across.
    ///
    /// **Idempotent in both directions**, because the wire message states the
    /// intended end state rather than toggling: adding a reaction that is
    /// already present changes nothing, and withdrawing one that is absent
    /// changes nothing. A duplicated or replayed frame is therefore harmless,
    /// which a toggle could not promise.
    ///
    /// A missing record is a silent no-op — a reaction can outlive the message
    /// it names, once that message has been deleted.
    ///
    /// Returns whether history actually changed, so a caller can avoid
    /// emitting an event for a write that did not happen.
    pub fn apply_reaction(
        &self,
        peer: &DeviceId,
        target_id: &str,
        emoji: &str,
        by: Direction,
        remove: bool,
    ) -> Result<bool, ChatError> {
        let Some(mut rec) = self.get(peer, target_id)? else {
            return Ok(false);
        };
        // One reaction per (emoji, side): a second identical one from the same
        // side is the same statement, not a louder one.
        let existing = rec
            .reactions
            .iter()
            .position(|r| r.emoji == emoji && r.by == by);
        match (existing, remove) {
            (Some(i), true) => {
                rec.reactions.remove(i);
            }
            (None, false) => rec.reactions.push(StoredReaction {
                emoji: emoji.to_string(),
                by,
                timestamp: Utc::now().to_rfc3339(),
            }),
            // Already in the requested state; no write, no event.
            _ => return Ok(false),
        }
        self.append(&rec)?;
        Ok(true)
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
            // The row has not been created yet — the peer's CHAT frame is still
            // in flight. Park what actually lands so `append` can apply it, or
            // the sender's claim would stand unchallenged at the approval
            // prompt, which is the whole thing this reconcile prevents.
            self.park_pending_landing(&peer.0, id, name, size)?;
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

    /// How many records the conversation with `peer` still holds, counted by
    /// **stored key** — a row this build cannot decode counts exactly like one
    /// it can.
    ///
    /// [`history`](Self::history) deliberately skips a row it cannot read, so
    /// counting through it would under-report a namespace holding rows from a
    /// newer schema. The one caller is the surface's "and this many were kept"
    /// report after a [`delete_conversation`](Self::delete_conversation): every
    /// row still present there is one the delete chose to keep, so a count that
    /// silently omitted the undecodable ones would tell the user nothing was
    /// kept while the thread stayed listed with a row in it.
    pub fn record_count(&self, peer: &DeviceId) -> Result<usize, ChatError> {
        let ns = namespace(peer);
        self.store
            .list(&ns)
            .map(|rows| rows.len())
            .map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// Forget this device's copy of the conversation with `peer`, **keeping
    /// every record that still backs a queued outbound message**. Returns how
    /// many records were removed.
    ///
    /// Local only: nothing goes on the wire, and the peer keeps its own copy.
    /// This is "forget this thread here", never "unsend".
    ///
    /// # Why this is not [`AppStore::clear`]
    ///
    /// Clearing the namespace would destroy the queue, and would do it *later*,
    /// silently, somewhere else. A queued outbound file is delivered by the
    /// FFI's drain, which re-opens the record the entry is named after
    /// ([`reopen_for_retry`](Self::reopen_for_retry)); a **missing** record is
    /// read there — correctly — as "nothing will ever settle this", so the
    /// entry is released and its staged blob deleted (`row_may_still_deliver`
    /// and `drop_queued_file` in `peerbeam_ffi::transfer`). Since staging holds
    /// the only copy the queue owns, and the user's own file may well be gone
    /// by then, the bytes would not come back.
    ///
    /// So the records backing queued entries stay, and staged blobs are never
    /// touched here at all. What the user gets is the honest outcome in both
    /// directions: nothing still to be sent is lost, and the thread does not
    /// mysteriously return days later — whatever survives is queued *right
    /// now*, and is visible immediately.
    ///
    /// # Which records survive, and why that rule lives elsewhere
    ///
    /// [`KeepRule`] decides, and its doc carries the long form: what a queued
    /// send loses when its record goes, why a file still being *staged* counts
    /// as queued even though no outbox entry names it yet, and why the set is
    /// established strictly or not at all. It is deliberately not spelled out
    /// again here — [`delete_messages`](Self::delete_messages) answers to the
    /// same rule, and a second copy of it is how the original defect would come
    /// back.
    ///
    /// [`AppStore::clear`]: peerbeam_domain::port::AppStore::clear
    pub fn delete_conversation(&self, peer: &DeviceId) -> Result<usize, ChatError> {
        let keep = KeepRule::establish(self.store.as_ref(), peer)?;
        let ns = namespace(peer);
        let rows = self
            .store
            .list(&ns)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        let mut removed = 0;
        for (key, value) in rows {
            // Asked of the value already in hand, so the rule costs no extra
            // store round trip.
            if keep.keeps(&key, &value) {
                continue;
            }
            // Deliberately keyed off the STORED KEY, not off a decoded record:
            // a row this build cannot read is still the user's to delete, and
            // `history` would silently skip it — leaving a thread that
            // reappears at the next namespace scan with nothing in it the user
            // can see or remove.
            if self
                .store
                .delete(&ns, &key)
                .map_err(|e| ChatError::Serialization(e.to_string()))?
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Delete the named messages from this conversation's local history.
    ///
    /// Returns `(removed, kept)`: `kept` are the ids that were asked for but
    /// must survive, because a queued send still depends on them.
    ///
    /// **Local only**, exactly like
    /// [`delete_conversation`](Self::delete_conversation): nothing goes on the
    /// wire, and the peer keeps its own copy. This is "forget these messages
    /// here", never "unsend".
    ///
    /// # The same rule, not a similar one
    ///
    /// Both deletes answer to [`KeepRule`], and must. Selecting every message
    /// in a thread *is* a conversation delete by another name, so two rules
    /// that could disagree would be the same data-loss bug arriving through
    /// whichever one was written second — and the one that costs a file is
    /// silent for minutes afterwards, in a background drain tick, with nothing
    /// on screen connecting it to the delete that caused it.
    ///
    /// # What the caller gets back
    ///
    /// `kept` is the *ids*, not a count: the user pointed at particular
    /// messages, so the surface can name the ones it could not take and say
    /// why. A conversation delete has no such list to give — it asked for
    /// everything — which is why that one answers with a count instead.
    ///
    /// An id the namespace does not hold is neither removed nor kept. It is
    /// simply not there, and calling it kept would tell the user something is
    /// still waiting to send a message that does not exist. An id repeated in
    /// the request is answered once.
    ///
    /// Deletion is by **stored key**, decodable or not, exactly as
    /// `delete_conversation` does it: a row this build cannot read is still the
    /// user's to delete, and is in fact the one they can see nothing of.
    pub fn delete_messages(
        &self,
        peer: &DeviceId,
        ids: &[String],
    ) -> Result<(usize, Vec<String>), ChatError> {
        // Established before anything is deleted, and for the whole request:
        // a rule read halfway through would let the rows deleted before it
        // failed stay deleted, which is precisely the outcome the strict read
        // exists to refuse.
        let keep = KeepRule::establish(self.store.as_ref(), peer)?;
        let ns = namespace(peer);
        let mut removed = 0;
        let mut kept = Vec::new();
        let mut seen = HashSet::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            // Fetched one key at a time rather than listed: a selection is a
            // handful of rows out of a thread that may hold thousands, and the
            // "not there at all" case falls out of the same read that hands the
            // keep rule its value.
            let Some(value) = self
                .store
                .get(&ns, id)
                .map_err(|e| ChatError::Serialization(e.to_string()))?
            else {
                continue;
            };
            if keep.keeps(id, &value) {
                kept.push(id.clone());
                continue;
            }
            if self
                .store
                .delete(&ns, id)
                .map_err(|e| ChatError::Serialization(e.to_string()))?
            {
                removed += 1;
            }
        }
        Ok((removed, kept))
    }
}

/// The one rule deciding which of a conversation's stored rows a local delete
/// must leave behind, shared by [`ChatStore::delete_conversation`] and
/// [`ChatStore::delete_messages`].
///
/// It exists as a type rather than as a helper each of them calls because a
/// *second* implementation of it is exactly how the data loss below comes
/// back, and the two legs in [`keeps`](Self::keeps) are answered together so
/// that a caller cannot honour one and forget the other.
///
/// # What it protects, and from what
///
/// A queued outbound message is delivered by the FFI's drain, which re-opens
/// the conversation record its outbox entry is named after
/// ([`reopen_for_retry`](ChatStore::reopen_for_retry)). A **missing** record is
/// read there — correctly — as "nothing will ever settle this", so the entry is
/// released and its staged blob deleted (`row_may_still_deliver` and
/// `drop_queued_file` in `peerbeam_ffi::transfer`). Staging holds the only copy
/// the queue owns and the user's own file may well be gone by then, so the
/// bytes do not come back. Deleting such a row therefore destroys a file some
/// minutes later, from a background tick, with nothing on screen connecting the
/// two.
///
/// # Two legs, and why they travel together
///
/// 1. **An outbox entry names it** — the ordinary case: a message queued for a
///    peer that is not there.
/// 2. **It still reads [`Status::Staging`]** — `begin_file_send` writes the row
///    synchronously and the copy (minutes, for a multi-GB file) runs before
///    `enqueue_file` queues anything, so for that whole window the outbox
///    cannot vouch for a row that is nonetheless waiting to be sent. Delete it
///    and the finished copy queues an entry with no record behind it: leg 1's
///    disaster, reached by another road. It is also what makes the promise the
///    surface shows true in the sense the user means it — they attached a file,
///    it is being copied, it is waiting to be sent.
///
/// A rule that answered only leg 1 would look correct against every settled
/// thread a test is likely to build. That is the shape the original defect had.
///
/// # The set is established completely, or not at all
///
/// [`establish`](Self::establish) hard-fails on any outbox entry that will not
/// decode — the same asymmetry, and for the same reason, as
/// [`outbox_owned_blobs`](ChatStore::outbox_owned_blobs), whose doc has the
/// long form. Every *other* outbox reader exists to deliver, where skipping one
/// unreadable row costs that message and saves every message queued behind it.
/// This one decides what to **delete**: a keep set that is merely incomplete
/// does not lose a row, it takes the record out from under a queue entry and
/// hands the next drain tick the verdict above. The sharp case is a
/// wholly-unreadable outbox, which the lenient readers report as `Ok(vec![])` —
/// indistinguishable from a queue that is genuinely empty, and enough to strand
/// every queued file on the device. Refusing costs the user a delete they can
/// retry; guessing costs them a file.
struct KeepRule {
    /// Message ids the shared outbox holds for this one peer.
    queued: HashSet<String>,
}

impl KeepRule {
    /// Read the shared outbox and establish the rule for `peer`'s conversation,
    /// or refuse with [`ChatError::QueueUnreadable`].
    fn establish(store: &dyn AppStore, peer: &DeviceId) -> Result<Self, ChatError> {
        let raw = store
            .list(OUTBOX_NS)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        let mut queued = HashSet::new();
        for (key, value) in raw {
            let entry = OutboxEntry::decode(&value).map_err(|e| {
                ChatError::QueueUnreadable(format!(
                    "outbox entry {key} is unreadable, so the records still backing \
                     queued messages cannot be established: {e}"
                ))
            })?;
            // An unreadable entry is refused above whoever it belongs to: its
            // peer is exactly what could not be read, so "it is probably not
            // this conversation's" is not something this can know.
            //
            // A queued DECLINE is deliberately not kept. Its `message_id` is
            // the *sender's* `FileRef` id, which in our own namespace names the
            // INBOUND row we refused — so keeping it holds a file the user
            // turned down alive forever, reported to them as "1 queued message
            // was kept and will still be sent", and leaves the thread listed
            // and undeletable until that peer comes back. Nothing needs it:
            // `flush_to_session` builds the `FileDecline` entirely from the
            // entry and never reads the record.
            //
            // Text is kept, even though `record_sent` could rebuild its row
            // from the entry alone. That rebuild is exactly the problem: the
            // row would vanish now and REAPPEAR when the message is finally
            // delivered, which is the "thread that mysteriously returns days
            // later" a delete exists to rule out.
            if entry.peer_id == peer.0 && entry.kind != Kind::Decline {
                queued.insert(entry.message_id);
            }
        }
        Ok(KeepRule { queued })
    }

    /// Whether the stored row `(key, value)` must survive the delete — both
    /// legs, in one answer.
    ///
    /// Takes the raw stored value rather than a decoded record so it can be
    /// answered from whatever the caller already holds (a `list` pair, or a
    /// `get` for one key) at no extra store round trip, and so a row this build
    /// cannot read is judged by the same call as one it can. Such a row is not
    /// staging as far as anyone here can tell, and says so: whether it *was*
    /// staging is precisely what could not be read, and keeping every
    /// unreadable row instead would leave the user a thread they can neither
    /// see into nor delete.
    fn keeps(&self, key: &str, value: &[u8]) -> bool {
        self.queued.contains(key)
            || matches!(ChatRecord::decode(value), Ok(rec) if rec.status == Status::Staging)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::FileRef;
    use crate::record::{ChatRecord, Direction, FileMeta, Kind, Status};
    use crate::{ChatMessage, StagingStore};
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
    fn a_receipt_marks_our_outgoing_messages_up_to_the_watermark() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m1 = ChatMessage::new("first").unwrap();
        let m2 = ChatMessage::new("second").unwrap();
        let m3 = ChatMessage::new("third").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m1)).unwrap();
        cs.append(&ChatRecord::sent(&peer, &m2)).unwrap();
        cs.append(&ChatRecord::sent(&peer, &m3)).unwrap();

        assert_eq!(
            cs.apply_receipt(&peer, &m2.id, "2026-01-01T00:00:00Z")
                .unwrap(),
            2
        );
        let hist = cs.history(&peer).unwrap();
        assert!(hist[0].read_at.is_some(), "first is below the watermark");
        assert!(
            hist[1].read_at.is_some(),
            "the watermark itself is inclusive"
        );
        assert!(hist[2].read_at.is_none(), "third is above the watermark");
    }

    #[test]
    fn a_receipt_never_touches_what_the_peer_sent_us() {
        // A receipt is the peer saying it read something *we* wrote. Letting it
        // mark its own inbound rows would let a peer rewrite its own messages
        // in our history.
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let theirs = ChatMessage::new("from bob").unwrap();
        cs.append(&ChatRecord::received(&peer, &theirs)).unwrap();

        assert_eq!(
            cs.apply_receipt(&peer, &theirs.id, "2026-01-01T00:00:00Z")
                .unwrap(),
            0
        );
        assert!(cs.history(&peer).unwrap()[0].read_at.is_none());
    }

    #[test]
    fn a_receipt_is_idempotent_and_cannot_move_a_row_back_to_unread() {
        // Watermarks arrive out of order over a lossy link. An older one must
        // not un-read anything, and a repeat must change nothing.
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m1 = ChatMessage::new("first").unwrap();
        let m2 = ChatMessage::new("second").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m1)).unwrap();
        cs.append(&ChatRecord::sent(&peer, &m2)).unwrap();

        assert_eq!(
            cs.apply_receipt(&peer, &m2.id, "2026-01-01T00:00:02Z")
                .unwrap(),
            2
        );
        let first_at = cs.history(&peer).unwrap()[0].read_at.clone();

        // A repeat, and then a stale older watermark.
        assert_eq!(
            cs.apply_receipt(&peer, &m2.id, "2026-01-01T00:00:03Z")
                .unwrap(),
            0
        );
        assert_eq!(
            cs.apply_receipt(&peer, &m1.id, "2026-01-01T00:00:01Z")
                .unwrap(),
            0
        );

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist[0].read_at, first_at, "the first read time stands");
        assert!(hist[1].read_at.is_some(), "still read");
    }

    #[test]
    fn a_receipt_cannot_reach_another_conversation() {
        let (cs, _store, _dir) = new_store();
        let bob = DeviceId::from("pb-bob");
        let eve = DeviceId::from("pb-eve");
        let m = ChatMessage::new("private").unwrap();
        cs.append(&ChatRecord::sent(&bob, &m)).unwrap();

        assert_eq!(
            cs.apply_receipt(&eve, &m.id, "2026-01-01T00:00:00Z")
                .unwrap(),
            0
        );
        assert!(cs.history(&bob).unwrap()[0].read_at.is_none());
    }

    #[test]
    fn a_reaction_attaches_to_the_message_it_names() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        assert!(cs
            .apply_reaction(&peer, &m.id, "\u{1F44D}", Direction::In, false)
            .unwrap());
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist[0].reactions.len(), 1);
        assert_eq!(hist[0].reactions[0].emoji, "\u{1F44D}");
        assert_eq!(hist[0].reactions[0].by, Direction::In);
    }

    #[test]
    fn adding_the_same_reaction_twice_changes_nothing() {
        // The wire message states the end state rather than toggling, so a
        // duplicated or replayed frame must be inert. A toggle would turn the
        // second delivery into a removal and leave the two devices disagreeing.
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        assert!(cs
            .apply_reaction(&peer, &m.id, "\u{1F389}", Direction::In, false)
            .unwrap());
        assert!(
            !cs.apply_reaction(&peer, &m.id, "\u{1F389}", Direction::In, false)
                .unwrap(),
            "second identical add reported a write"
        );
        assert_eq!(cs.history(&peer).unwrap()[0].reactions.len(), 1);
    }

    #[test]
    fn withdrawing_a_reaction_that_is_not_there_changes_nothing() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        assert!(
            !cs.apply_reaction(&peer, &m.id, "\u{1F44D}", Direction::In, true)
                .unwrap(),
            "removing an absent reaction reported a write"
        );
        assert!(cs.history(&peer).unwrap()[0].reactions.is_empty());
    }

    #[test]
    fn each_side_reacts_independently_with_the_same_emoji() {
        // One reaction per (emoji, side): both participants liking the same
        // message are two reactions, not one overwriting the other.
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        cs.apply_reaction(&peer, &m.id, "\u{1F44D}", Direction::In, false)
            .unwrap();
        cs.apply_reaction(&peer, &m.id, "\u{1F44D}", Direction::Out, false)
            .unwrap();
        assert_eq!(cs.history(&peer).unwrap()[0].reactions.len(), 2);

        // ...and withdrawing one leaves the other standing.
        cs.apply_reaction(&peer, &m.id, "\u{1F44D}", Direction::In, true)
            .unwrap();
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist[0].reactions.len(), 1);
        assert_eq!(hist[0].reactions[0].by, Direction::Out);
    }

    #[test]
    fn a_peer_cannot_react_into_another_conversation() {
        // The lookup is scoped to the reacting peer's own namespace, so a
        // target id naming someone else's message finds nothing rather than
        // reaching across. This is the failure worth pinning: `target_id`
        // arrives from the peer.
        let (cs, _store, _dir) = new_store();
        let bob = DeviceId::from("pb-bob");
        let eve = DeviceId::from("pb-eve");
        let m = ChatMessage::new("private").unwrap();
        cs.append(&ChatRecord::sent(&bob, &m)).unwrap();

        assert!(
            !cs.apply_reaction(&eve, &m.id, "\u{1F440}", Direction::In, false)
                .unwrap(),
            "eve reacted to a message in bob's conversation"
        );
        assert!(cs.history(&bob).unwrap()[0].reactions.is_empty());
    }

    #[test]
    fn a_reaction_to_a_message_that_is_gone_is_a_no_op() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        assert!(!cs
            .apply_reaction(&peer, "no-such-id", "\u{1F44D}", Direction::In, false)
            .unwrap());
    }

    #[test]
    fn a_row_written_before_reactions_existed_decodes_with_none() {
        // Upgrade safety: history written by an older build has no `reactions`
        // key at all, and must read as "no reactions" rather than failing the
        // conversation.
        let legacy = br#"{"id":"m1","peer_id":"pb-bob","direction":"out","timestamp":"2026-01-01T00:00:00Z","body":"hi","status":"sent"}"#;
        let rec = ChatRecord::decode(legacy).unwrap();
        assert!(rec.reactions.is_empty());
        assert_eq!(rec.body, "hi");
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
    /// **The claim must lose whichever frame arrives first.**
    ///
    /// The peer's claim rides CHAT and what actually lands rides TRANSFER;
    /// nothing orders the two. When the landing wins the race there is no row
    /// to reconcile yet, and an earlier version simply dropped it — so on a
    /// platform where that ordering happened to flip, the approval prompt
    /// showed the sender's chosen name for a file that was going to arrive
    /// under a very different one. This is the same assertion as the test
    /// below, with the two steps swapped.
    #[test]
    fn a_landing_that_arrives_before_its_row_is_still_applied() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let mut r = FileRef::new("holiday.jpg", 184_320).unwrap();
        r.name = "holiday.jpg".into();

        // TRANSFER first: nothing to reconcile yet, so it must be parked.
        let wrote = cs
            .set_file_row_landing(&peer, &r.id, Direction::In, "invoice-2026.pdf.exe", 4_096)
            .unwrap();
        assert!(!wrote, "there is no row yet, so nothing was written");

        // CHAT second: the row is created, and must carry what lands.
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        let meta = cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap();
        assert_eq!(
            meta.name, "invoice-2026.pdf.exe",
            "the peer's claim outranked what actually lands"
        );
        assert_eq!(meta.size, 4_096, "and its size");
    }

    /// A parked landing is consumed once, not replayed onto a later row that
    /// happens to reuse the id.
    #[test]
    fn a_parked_landing_applies_once_and_is_then_gone() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let mut r = FileRef::new("holiday.jpg", 1).unwrap();
        r.name = "holiday.jpg".into();

        cs.set_file_row_landing(&peer, &r.id, Direction::In, "real.bin", 4_096)
            .unwrap();
        cs.append(&ChatRecord::file_in(&peer, &r)).unwrap();
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap().name,
            "real.bin"
        );

        // A second row under the same id must not inherit it again.
        let mut again = FileRef::new("second.jpg", 7).unwrap();
        again.name = "second.jpg".into();
        again.id = r.id.clone();
        cs.append(&ChatRecord::file_in(&peer, &again)).unwrap();
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap().name,
            "second.jpg",
            "a consumed landing was replayed"
        );
    }

    /// Parking is per peer **and** per id: one peer's landing must never be
    /// applied to another peer's row.
    #[test]
    fn a_parked_landing_belongs_to_one_peer_only() {
        let (cs, _store, _dir) = new_store();
        let bob = DeviceId::from("pb-bob");
        let eve = DeviceId::from("pb-eve");
        let mut r = FileRef::new("holiday.jpg", 1).unwrap();
        r.name = "holiday.jpg".into();

        cs.set_file_row_landing(&bob, &r.id, Direction::In, "bobs-real.bin", 4_096)
            .unwrap();

        // Eve's row shares the id but not the peer.
        cs.append(&ChatRecord::file_in(&eve, &r)).unwrap();
        assert_eq!(
            cs.get(&eve, &r.id).unwrap().unwrap().file.unwrap().name,
            "holiday.jpg",
            "another peer's landing was applied"
        );

        // Bob's own row still gets it.
        cs.append(&ChatRecord::file_in(&bob, &r)).unwrap();
        assert_eq!(
            cs.get(&bob, &r.id).unwrap().unwrap().file.unwrap().name,
            "bobs-real.bin"
        );
    }

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
            read_at: None,
            reactions: Vec::new(),
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

    // ── the queue: enqueue_file / enqueue_decline / the backstop counter /
    // the retry re-open ─────────────────────────────────────────────────────

    /// The staged file a test queues, plus the row `begin_file_send` would have
    /// written for it: `Staging`, sized from the source's metadata, carrying the
    /// sender's own path.
    fn staged_row(
        cs: &ChatStore,
        peer: &DeviceId,
        source_size: u64,
        blob_size: u64,
    ) -> (FileRef, StagedFile) {
        let r = FileRef::new("report.pdf", source_size).unwrap();
        cs.append(&ChatRecord::file_out(
            peer,
            &r,
            FileMeta::new(&r.name, source_size, Some("/home/me/report.pdf".into())),
            Status::Staging,
        ))
        .unwrap();
        let staged = StagedFile {
            name: "report.pdf".into(),
            size: blob_size,
            staged_path: format!("/data/outbox-blobs/{}", r.id),
        };
        (r, staged)
    }

    #[test]
    fn enqueue_file_queues_the_blob_and_marks_our_row_pending() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let (r, staged) = staged_row(&cs, &peer, 4096, 4096);

        assert!(
            cs.enqueue_file(&peer, &r, &staged).unwrap(),
            "the row is there, so it queues"
        );

        let queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, Kind::File);
        assert_eq!(queued[0].message_id, r.id);
        assert_eq!(queued[0].body, "", "a file entry carries no text body");
        assert_eq!(queued[0].offers_refused, 0);
        assert_eq!(queued[0].file.as_ref().unwrap(), &staged);

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "upsert, not a second row");
        assert_eq!(hist[0].status, Status::Pending, "queued, not yet sent");
        assert_eq!(hist[0].kind, Kind::File);
        assert_eq!(
            hist[0].file.as_ref().unwrap().local_path.as_deref(),
            Some("/home/me/report.pdf"),
            "a rebuild would drop the path the sender's own Open depends on"
        );
    }

    /// The bytes that will be delivered are the blob's, so the row must show the
    /// blob's size — not the metadata read before the copy, which a source being
    /// appended to (a log, a download in progress) makes wrong.
    #[test]
    fn enqueue_file_corrects_the_rows_size_to_what_was_actually_staged() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let (r, staged) = staged_row(&cs, &peer, 4096, 9001);

        assert!(cs.enqueue_file(&peer, &r, &staged).unwrap());

        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap().size,
            9001,
            "the row must describe the blob, not the pre-copy metadata"
        );
    }

    #[test]
    fn outbox_bump_refused_counts_only_while_the_entry_is_still_queued() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let (r, staged) = staged_row(&cs, &peer, 1, 1);
        assert!(cs.enqueue_file(&peer, &r, &staged).unwrap());

        assert_eq!(cs.outbox_bump_refused(&r.id).unwrap(), 1);
        assert_eq!(cs.outbox_bump_refused(&r.id).unwrap(), 2);
        assert_eq!(
            cs.outbox_for(&peer).unwrap()[0].offers_refused,
            2,
            "the count is persisted, so it survives a restart"
        );

        // Dequeued (or never queued): nothing to count, and no phantom entry.
        cs.outbox_remove(&r.id).unwrap();
        assert_eq!(cs.outbox_bump_refused(&r.id).unwrap(), 0);
        assert_eq!(cs.outbox_bump_refused("never-existed").unwrap(), 0);
        assert!(cs.outbox_for(&peer).unwrap().is_empty());
    }

    /// The queued decline: a refusal the sender never heard, delivered later
    /// over the machinery text already uses.
    #[test]
    fn enqueue_decline_queues_a_decline_and_never_overwrites_a_queued_message() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");

        let d = FileDecline::new("0000000000009");
        assert!(cs.enqueue_decline(&peer, &d).unwrap());
        let queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, Kind::Decline);
        assert_eq!(queued[0].message_id, "0000000000009");
        assert_eq!(queued[0].timestamp, d.timestamp);
        assert!(queued[0].file.is_none());

        // The id is the SENDER's — a value the peer chose. A peer that picks the
        // id of a message we already have queued must not be able to replace it.
        let mine = ChatMessage::new("a message of ours awaiting delivery").unwrap();
        cs.enqueue(&peer, &mine).unwrap();
        let collide = FileDecline::new(&mine.id);
        assert!(
            !cs.enqueue_decline(&peer, &collide).unwrap(),
            "a decline must never take an occupied outbox key"
        );
        let still = cs.outbox_for(&peer).unwrap();
        let ours = still
            .iter()
            .find(|e| e.message_id == mine.id)
            .expect("our own queued message survives");
        assert_eq!(ours.kind, Kind::Text);
        assert_eq!(ours.body, "a message of ours awaiting delivery");
    }

    /// The brief's crux: a retry must move a `Failed` row back to
    /// `Transferring`, which the settle guard forbids — correctly, because that
    /// guard is what stops a peer reusing a known message id as its
    /// `transfer_id` to rewrite a settled row. The guard is NOT relaxed; the
    /// retry is a separate, local, sender-only path.
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
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Failed
        );

        // The local retry path can, and only for our own outgoing file row.
        assert!(cs.reopen_for_retry(&peer, &r.id).unwrap());
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Transferring
        );
        // A settled-Sent row is never re-opened.
        cs.settle_file_row(&peer, &r.id, Direction::Out, Status::Sent)
            .unwrap();
        assert!(!cs.reopen_for_retry(&peer, &r.id).unwrap());
        assert_eq!(cs.get(&peer, &r.id).unwrap().unwrap().status, Status::Sent);
    }

    /// Every state a still-queued outgoing file row can legitimately be found
    /// in must re-open, and nothing else may.
    #[test]
    fn reopen_for_retry_accepts_exactly_the_non_final_outgoing_file_rows() {
        let (cs, _store, _tmp) = new_store();
        let peer = DeviceId::from("pb-bob".to_string());
        let seed = |status: Status| {
            let r = FileRef::new("a.bin", 1).unwrap();
            cs.append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, r.size, None),
                status,
            ))
            .unwrap();
            r.id
        };

        // Deliverable: queued, previously failed, crashed mid-enqueue, or
        // settled `Interrupted` by a restart while its entry stayed queued.
        for status in [
            Status::Pending,
            Status::Failed,
            Status::Staging,
            Status::Interrupted,
        ] {
            let id = seed(status);
            assert!(
                cs.reopen_for_retry(&peer, &id).unwrap(),
                "{status:?} is still deliverable and must re-open"
            );
            assert_eq!(
                cs.get(&peer, &id).unwrap().unwrap().status,
                Status::Transferring
            );
        }
        // Final, or already live.
        for status in [Status::Sent, Status::Declined, Status::Transferring] {
            let id = seed(status);
            assert!(
                !cs.reopen_for_retry(&peer, &id).unwrap(),
                "{status:?} must not re-open"
            );
            assert_eq!(cs.get(&peer, &id).unwrap().unwrap().status, status);
        }

        // Never another row's business: a text row, an inbound file row, or an
        // id with no row at all.
        let text = ChatMessage::new("hi").unwrap();
        cs.append(&ChatRecord::sent(&peer, &text)).unwrap();
        assert!(!cs.reopen_for_retry(&peer, &text.id).unwrap());
        assert_eq!(
            cs.get(&peer, &text.id).unwrap().unwrap().status,
            Status::Sent
        );

        let theirs = FileRef::new("theirs.bin", 1).unwrap();
        cs.append(&ChatRecord::file_in(&peer, &theirs)).unwrap();
        assert!(!cs.reopen_for_retry(&peer, &theirs.id).unwrap());
        assert_eq!(
            cs.get(&peer, &theirs.id).unwrap().unwrap().status,
            Status::PendingApproval
        );

        assert!(!cs.reopen_for_retry(&peer, "never-existed").unwrap());
    }

    /// TRAP 1, directly. `record_sent`'s fallback fires only when no record
    /// exists under the entry's id; before `enqueue_file` existed nothing could
    /// produce a non-text entry, so a hardcoded `Kind::Text, file: None` rebuild
    /// was unreachable. It is reachable now, and a delivered file rebuilt as
    /// text would lose its name, its size and its file metadata entirely.
    #[test]
    fn record_sents_fallback_rebuilds_a_file_entry_as_a_file_row() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        // A queued file whose conversation row has gone missing (the only way
        // to reach the fallback at all).
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: "0000000000042".into(),
            body: String::new(),
            timestamp: "2026-08-13T10:00:00+00:00".into(),
            kind: Kind::File,
            file: Some(StagedFile {
                name: "report.pdf".into(),
                size: 4096,
                staged_path: "/data/outbox-blobs/0000000000042".into(),
            }),
            offers_refused: 0,
        };
        assert!(
            cs.get(&peer, &entry.message_id).unwrap().is_none(),
            "the fallback only fires with no existing row"
        );

        cs.record_sent(&entry).unwrap();

        let rec = cs.get(&peer, &entry.message_id).unwrap().expect("row");
        assert_eq!(rec.kind, Kind::File, "a file must not be rebuilt as text");
        assert_eq!(rec.status, Status::Sent);
        assert_eq!(rec.direction, Direction::Out);
        let meta = rec.file.expect("a rebuilt file row carries its metadata");
        assert_eq!(meta.name, "report.pdf");
        assert_eq!(meta.size, 4096);
        assert!(
            meta.local_path.is_none(),
            "the staged blob is deleted on settle; an Open pointing at it would dangle"
        );
    }

    /// A decline is a status change on a row that already exists, never a row of
    /// its own — so the fallback must not invent one.
    #[test]
    fn record_sents_fallback_never_invents_a_row_for_a_decline() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let entry = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: "0000000000043".into(),
            body: String::new(),
            timestamp: "2026-08-13T10:00:00+00:00".into(),
            kind: Kind::Decline,
            file: None,
            offers_refused: 0,
        };
        cs.record_sent(&entry).unwrap();
        assert!(cs.history(&peer).unwrap().is_empty());
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

    // ── conversations() — startup can now name every thread, not only the
    // ones with something queued. ───────────────────────────────────────────

    /// The exact gap `outbox_peers` left. A thread whose only unsettled row is
    /// a **file** has nothing queued as *text*, so the old startup
    /// reconciliation could not name it and its `Transferring` row spun
    /// forever. Also pins the two namespace confusions: the shared outbox is
    /// never a conversation, while a peer that claims the device id `outbox`
    /// is one — they are different namespaces (`chat.outbox` vs `chat-outbox`)
    /// and must stay that way.
    #[test]
    fn conversations_lists_every_thread_including_a_file_only_one_and_never_the_outbox() {
        let (cs, _store, _tmp) = new_store();

        // (a) A peer whose only row is an in-flight file, with NOTHING queued
        //     — invisible to `outbox_peers`, which is the whole bug.
        let file_only = DeviceId::from("pb-file-only");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        cs.append(&ChatRecord::file_out(
            &file_only,
            &r,
            FileMeta::new(&r.name, 4096, None),
            Status::Transferring,
        ))
        .unwrap();

        // (b) A peer with queued text — the only kind `outbox_peers` could see.
        let texter = DeviceId::from("pb-texter");
        cs.enqueue(&texter, &ChatMessage::new("queued").unwrap())
            .unwrap();

        // (c) A peer that has claimed the device id "outbox". Device ids are
        //     peer-supplied over the wire, so any peer can present this.
        let impostor = DeviceId::from("outbox");
        cs.append(&ChatRecord::received(
            &impostor,
            &ChatMessage::new("hi from a peer named outbox").unwrap(),
        ))
        .unwrap();

        let mut got: Vec<String> = cs
            .conversations()
            .unwrap()
            .into_iter()
            .map(|d| d.0)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "outbox".to_string(),
                "pb-file-only".to_string(),
                "pb-texter".to_string()
            ]
        );

        // The gap, stated directly rather than implied: the old enumeration
        // could only ever have named the peer with queued text.
        let queued: Vec<String> = cs
            .outbox_peers()
            .unwrap()
            .into_iter()
            .map(|d| d.0)
            .collect();
        assert_eq!(queued, vec!["pb-texter".to_string()]);

        // The shared outbox namespace really is populated (b queued into it),
        // so its absence above is a discrimination, not an empty directory.
        assert!(!cs.outbox_pending().unwrap().is_empty());
        assert!(
            !got.iter()
                .any(|id| namespace(&DeviceId::from(id.clone())) == OUTBOX_NS),
            "no conversation may map back onto the outbox namespace"
        );
        assert!(!got.contains(&OUTBOX_NS.to_string()));
        // The impostor's own thread is a real conversation, under its own
        // namespace, and reconciling it does not touch the outbox.
        assert_eq!(namespace(&impostor), "chat-outbox");
        assert_eq!(cs.history(&impostor).unwrap().len(), 1);

        // And the payoff: the file-only thread is now reachable, so a restart
        // settles the row that nothing will ever finish.
        assert_eq!(cs.reconcile_peer(&file_only).unwrap(), 1);
        assert_eq!(
            cs.get(&file_only, &r.id).unwrap().unwrap().status,
            Status::Interrupted
        );
    }

    // ── outbox_owned_blobs() — the strict reader that stands between a
    // corrupted outbox and every staged file on the device. ─────────────────

    /// A staging store rooted in `dir`, plus a blob written directly into it.
    /// Written rather than staged so this stays a synchronous test: `sweep`
    /// only cares that a file sits in the root, not how it got there.
    fn staged_blob(dir: &std::path::Path, id: &str) -> (StagingStore, String) {
        let root = dir.join("outbox-blobs");
        std::fs::create_dir_all(&root).unwrap();
        let blob = root.join(id);
        std::fs::write(&blob, b"the only copy of a queued file").unwrap();
        (
            StagingStore::new(
                root.to_string_lossy().into_owned(),
                Arc::new(peerbeam_storage_fs::FsStorage::new()),
            ),
            blob.to_string_lossy().into_owned(),
        )
    }

    /// The healthy case, first — otherwise "refuses to sweep" could be
    /// satisfied by a guard that never sweeps anything at all. A readable
    /// outbox reports exactly the blobs it owns: a text entry contributes no
    /// phantom path, and a blob nobody queued is correctly an orphan.
    #[test]
    fn outbox_owned_blobs_reports_exactly_what_the_queue_owns() {
        let (cs, _store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");

        let r = FileRef::new("report.pdf", 30).unwrap();
        let (staging, owned_path) = staged_blob(tmp.path(), &r.id);
        let (_, orphan_path) = staged_blob(tmp.path(), "0000000000000-orphan");
        cs.append(&ChatRecord::file_out(
            &peer,
            &r,
            FileMeta::new(&r.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(
            cs.enqueue_file(
                &peer,
                &r,
                &StagedFile {
                    name: "report.pdf".into(),
                    size: 30,
                    staged_path: owned_path.clone(),
                },
            )
            .unwrap(),
            "the row seeded above is there, so it queues"
        );
        // A text entry owns no blob and must not invent one.
        cs.enqueue(&peer, &ChatMessage::new("also queued").unwrap())
            .unwrap();

        let owned = cs.outbox_owned_blobs().expect("a readable outbox answers");
        assert_eq!(owned.len(), 1, "one file queued, one blob owned");
        assert!(owned.contains(&owned_path));

        assert_eq!(staging.sweep(&owned), 1, "the unowned blob is an orphan");
        assert!(std::path::Path::new(&owned_path).exists());
        assert!(!std::path::Path::new(&orphan_path).exists());
    }

    /// A genuinely empty outbox is a complete answer, not a refusal — an empty
    /// `keep` is correct there, and every blob really is an orphan. This is
    /// what makes the refusal below meaningful: the two cases are told apart,
    /// rather than the sweep simply being disabled.
    #[test]
    fn a_genuinely_empty_outbox_answers_with_an_empty_set_and_sweeps() {
        let (cs, _store, tmp) = new_store();
        let (staging, orphan) = staged_blob(tmp.path(), "0000000000001");

        let owned = cs
            .outbox_owned_blobs()
            .expect("an empty queue is a complete answer, not a failure");
        assert!(owned.is_empty());
        assert_eq!(staging.sweep(&owned), 1);
        assert!(!std::path::Path::new(&orphan).exists());
    }

    /// THE TRAP, directly. `sweep` deletes every blob its `keep` set does not
    /// name, and the ordinary outbox readers deliberately **skip** a row they
    /// cannot decode — so a wholly-unreadable outbox reads back as
    /// `Ok(vec![])`, indistinguishable from a queue that is genuinely empty.
    /// Feeding that to `sweep` at startup would delete the only copy of every
    /// file the user queued, while each conversation row still said the file
    /// was waiting to be sent.
    ///
    /// `outbox_owned_blobs` refuses instead, and a refusal means the caller
    /// sweeps nothing this run. The last block proves the assertion is not
    /// vacuous: the naive wiring really does destroy the same bytes.
    #[test]
    fn an_unreadable_outbox_refuses_rather_than_under_report_and_no_blob_is_swept() {
        let (cs, store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");

        // Two files queued for an offline peer, their only copies on disk.
        let mut blobs = Vec::new();
        let mut staging = None;
        for name in ["report.pdf", "photo.jpg"] {
            let r = FileRef::new(name, 30).unwrap();
            let (s, path) = staged_blob(tmp.path(), &r.id);
            cs.append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, 30, None),
                Status::Staging,
            ))
            .unwrap();
            assert!(
                cs.enqueue_file(
                    &peer,
                    &r,
                    &StagedFile {
                        name: name.into(),
                        size: 30,
                        staged_path: path.clone(),
                    },
                )
                .unwrap(),
                "the row seeded above is there, so it queues"
            );
            blobs.push(path);
            staging = Some(s);
        }
        let staging = staging.expect("two blobs staged");
        assert_eq!(cs.outbox_owned_blobs().unwrap().len(), 2);

        // Now make every outbox row undecodable — what a newer schema looks
        // like to an older binary, applied to the whole namespace.
        for (key, _) in store.list(OUTBOX_NS).unwrap() {
            store
                .put(OUTBOX_NS, &key, b"{\"from\":\"the-future\"}")
                .unwrap();
        }

        // The delivery reader contains the damage, exactly as designed — and
        // in doing so becomes unable to say whether anything is queued at all.
        let lenient = cs
            .outbox_pending()
            .expect("containment: skipping a bad row never fails the call");
        assert!(
            lenient.is_empty(),
            "every row was skipped, so this reads as 'nothing is queued'"
        );

        // The startup decision, in exactly the shape `runtime::init` makes it:
        // sweep on a complete answer, sweep NOTHING on a refusal. Written as
        // the real decision rather than as a bare `expect_err` so that a
        // regression is observed where it hurts — blobs actually deleted —
        // and not merely as a `Result` variant changing shape.
        let swept = match cs.outbox_owned_blobs() {
            Ok(owned) => staging.sweep(&owned),
            Err(e) => {
                assert!(matches!(e, ChatError::Serialization(_)), "{e:?}");
                0
            }
        };
        assert_eq!(
            swept, 0,
            "an unreadable outbox must authorise no deletion at all"
        );
        for path in &blobs {
            assert!(
                std::path::Path::new(path).exists(),
                "a queued file's only copy must survive a corrupted outbox"
            );
        }

        // Not vacuous: the naive wiring — `sweep` fed from the lenient reader
        // via `unwrap_or_default()` — deletes both. This is what the refusal
        // above prevents, and it runs last so it cannot mask the assertions.
        let naive: HashSet<String> = lenient
            .into_iter()
            .filter_map(|e| e.file.map(|f| f.staged_path))
            .collect();
        assert!(naive.is_empty());
        assert_eq!(
            staging.sweep(&naive),
            2,
            "the lenient reader would have destroyed both queued files"
        );
    }

    // ── delete_conversation — "delete the history, keep the queue" ──────────

    /// THE TRAP this method exists to avoid, at store level: the record backing
    /// a still-queued file **must survive**, because the drain reads a missing
    /// record as "nothing will ever settle this" and throws the file away.
    ///
    /// Everything else in the thread goes — settled text, a delivered file, the
    /// peer's own messages — and the staged blob is not touched, since the
    /// queue still owns it.
    #[test]
    fn delete_conversation_keeps_the_records_backing_queued_messages_and_removes_the_rest() {
        let (cs, _store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");

        // Settled history: our text, their text, a delivered file.
        let mine = ChatMessage::new("mine, delivered").unwrap();
        cs.append(&ChatRecord::sent(&peer, &mine)).unwrap();
        let theirs = ChatMessage::new("theirs").unwrap();
        cs.append(&ChatRecord::received(&peer, &theirs)).unwrap();
        let done = FileRef::new("delivered.pdf", 10).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &done,
            FileMeta::new(&done.name, 10, None),
            Status::Sent,
        ))
        .unwrap();

        // One file still queued for this (offline) peer, its bytes staged.
        let queued = FileRef::new("waiting.mkv", 30).unwrap();
        let (_staging, blob) = staged_blob(tmp.path(), &queued.id);
        cs.append(&ChatRecord::file_out(
            &peer,
            &queued,
            FileMeta::new(&queued.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(
            cs.enqueue_file(
                &peer,
                &queued,
                &StagedFile {
                    name: "waiting.mkv".into(),
                    size: 30,
                    staged_path: blob.clone(),
                },
            )
            .unwrap(),
            "the row seeded above is there, so it queues"
        );
        // …and one queued TEXT message, which is queued just the same.
        let pending_text = ChatMessage::new("not sent yet").unwrap();
        cs.enqueue(&peer, &pending_text).unwrap();

        // Another peer's thread must be untouched by any of this.
        let other = DeviceId::from("pb-carol");
        let elsewhere = ChatMessage::new("different thread").unwrap();
        cs.append(&ChatRecord::sent(&other, &elsewhere)).unwrap();

        assert_eq!(cs.history(&peer).unwrap().len(), 5);
        let removed = cs.delete_conversation(&peer).unwrap();
        assert_eq!(removed, 3, "the three settled records, and only those");

        let left = cs.history(&peer).unwrap();
        let ids: Vec<&str> = left.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            2,
            "exactly the two records still backing queued entries: {ids:?}"
        );
        assert!(
            ids.contains(&queued.id.as_str()),
            "the queued file's record"
        );
        assert!(
            ids.contains(&pending_text.id.as_str()),
            "the queued text's record"
        );

        // The queue itself is untouched — entries and bytes both.
        let still_queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(still_queued.len(), 2, "no entry is dequeued by a delete");
        assert!(
            std::path::Path::new(&blob).exists(),
            "a queued file's only copy must survive deleting its conversation"
        );
        assert_eq!(
            cs.outbox_owned_blobs().unwrap().len(),
            1,
            "and the queue still owns it, so no sweep will collect it either"
        );

        // Blast radius is one conversation.
        assert_eq!(cs.history(&other).unwrap().len(), 1);
    }

    /// With nothing queued there is nothing to protect, so the thread goes
    /// completely — and `conversations` stops listing it, which is what makes
    /// the row disappear from the UI instead of returning empty.
    #[test]
    fn delete_conversation_with_nothing_queued_removes_every_record() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        for n in 0..4 {
            let m = ChatMessage::new(&format!("message {n}")).unwrap();
            cs.append(&ChatRecord::sent(&peer, &m)).unwrap();
        }
        let theirs = FileRef::new("theirs.bin", 5).unwrap();
        let mut received = ChatRecord::file_in(&peer, &theirs);
        received.status = Status::Received;
        cs.append(&received).unwrap();

        assert_eq!(cs.delete_conversation(&peer).unwrap(), 5);
        assert!(cs.history(&peer).unwrap().is_empty());
        assert!(
            !cs.conversations().unwrap().contains(&peer),
            "an emptied thread is not a thread: the row must not come back"
        );

        // Deleting again removes nothing and says so, rather than erroring at a
        // surface that legitimately raced itself.
        assert_eq!(cs.delete_conversation(&peer).unwrap(), 0);
    }

    /// A record this build cannot decode is still the user's to delete.
    /// `history` skips such a row, so keying the delete off decoded records
    /// would leave it behind — and one invisible leftover is enough for
    /// `conversations` to keep listing a thread the user just deleted, with
    /// nothing in it they can see or remove.
    #[test]
    fn delete_conversation_removes_a_row_this_build_cannot_read() {
        let (cs, store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("readable").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();
        store
            .put(
                &namespace(&peer),
                "0000000000009",
                b"{\"from\":\"the-future\"}",
            )
            .unwrap();

        assert_eq!(
            cs.history(&peer).unwrap().len(),
            1,
            "the bad row is skipped"
        );
        assert_eq!(
            cs.delete_conversation(&peer).unwrap(),
            2,
            "both are removed"
        );
        assert!(
            !cs.conversations().unwrap().contains(&peer),
            "nothing is left behind to keep the thread alive"
        );
    }

    /// **THE STAGING WINDOW.** `begin_file_send` writes the row `Staging`
    /// synchronously and the copy — minutes, for a multi-GB file — runs before
    /// `enqueue_file` puts anything in the outbox. An outbox-only keep set does
    /// not name that row for the whole of that window, so a delete landing in
    /// it removed the row; the copy would then finish into a queued entry with
    /// no record, which is exactly what the drain reads as "nothing will ever
    /// settle this" — it offers the file to the peer and *then* releases the
    /// entry and deletes the only copy of the bytes.
    ///
    /// The user was told "anything still waiting to be sent is kept and will
    /// still be sent". A file they attached ten seconds ago is precisely that,
    /// in every sense they mean it.
    #[test]
    fn delete_conversation_keeps_a_file_that_is_still_being_staged() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");

        // Ordinary settled history around it, all of it removable.
        for body in ["one", "two"] {
            cs.append(&ChatRecord::sent(&peer, &ChatMessage::new(body).unwrap()))
                .unwrap();
        }
        // And a file whose bytes are still being copied: a row, and no entry.
        let staging = FileRef::new("holiday.mp4", 8_000_000_000).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &staging,
            FileMeta::new(
                &staging.name,
                staging.size,
                Some("/home/me/holiday.mp4".into()),
            ),
            Status::Staging,
        ))
        .unwrap();
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "nothing is queued until the copy finishes — that IS the window"
        );

        assert_eq!(
            cs.delete_conversation(&peer).unwrap(),
            2,
            "the settled history goes"
        );

        let left = cs.history(&peer).unwrap();
        assert_eq!(left.len(), 1, "and the staging row stays: {left:?}");
        assert_eq!(left[0].id, staging.id);
        assert_eq!(left[0].status, Status::Staging);
        assert_eq!(
            left[0].file.as_ref().unwrap().local_path.as_deref(),
            Some("/home/me/holiday.mp4"),
            "kept whole — the delete does not rewrite what it keeps"
        );

        // The consequence, not just the row: the copy can still finish into it,
        // which is the only reason keeping it matters.
        let staged = StagedFile {
            name: "holiday.mp4".into(),
            size: 8_000_000_000,
            staged_path: format!("/data/outbox-blobs/{}", staging.id),
        };
        assert!(
            cs.enqueue_file(&peer, &staging, &staged).unwrap(),
            "the finished copy queues normally, exactly as if nothing happened"
        );
        assert_eq!(cs.outbox_for(&peer).unwrap().len(), 1);
        assert_eq!(
            cs.get(&peer, &staging.id).unwrap().unwrap().status,
            Status::Pending
        );
    }

    /// The other half of the same defect, and the one that must hold even
    /// though the keep above makes it nearly unreachable: `enqueue_file` must
    /// never leave an entry behind a record that has gone.
    ///
    /// Asserting the **outbox is empty** is the whole assertion. A `false`
    /// return on its own would pass just as well against the old order — write
    /// the entry, then discover the row is missing, then silently `Ok` — which
    /// is the state that costs the user the file.
    #[test]
    fn enqueue_file_queues_nothing_when_its_row_has_been_deleted() {
        let (cs, store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let (r, staged) = staged_row(&cs, &peer, 4096, 4096);

        // The row goes while the copy is still running.
        assert!(store.delete(&namespace(&peer), &r.id).unwrap());

        assert!(
            !cs.enqueue_file(&peer, &r, &staged).unwrap(),
            "a conversation that is gone cannot take a queued file"
        );
        assert!(
            cs.outbox_pending().unwrap().is_empty(),
            "and NOTHING was queued — not an entry the caller is left to regret"
        );
        assert!(
            cs.get(&peer, &r.id).unwrap().is_none(),
            "nor is a row invented to hang it off"
        );
    }

    /// A queued DECLINE must not keep an inbound row alive.
    ///
    /// Its `message_id` is the **sender's** `FileRef` id, which in our own
    /// namespace names the row we *refused*. Keeping it left the thread listed
    /// and undeletable until that peer came back — possibly never — and told
    /// the user "1 queued message was kept and will still be sent" about a file
    /// they turned down and never sent.
    #[test]
    fn delete_conversation_does_not_keep_an_inbound_row_for_a_queued_decline() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");

        // Bob offered a file, we declined, and Bob had already dropped — so the
        // refusal is queued for whenever he returns.
        let theirs = FileRef::new("theirs.iso", 900).unwrap();
        let mut declined = ChatRecord::file_in(&peer, &theirs);
        declined.status = Status::Declined;
        cs.append(&declined).unwrap();
        assert!(cs
            .enqueue_decline(&peer, &FileDecline::new(&theirs.id))
            .unwrap());

        assert_eq!(
            cs.delete_conversation(&peer).unwrap(),
            1,
            "the declined row is the user's to delete"
        );
        assert!(cs.history(&peer).unwrap().is_empty());
        assert!(
            !cs.conversations().unwrap().contains(&peer),
            "the thread goes, rather than staying listed until the peer returns"
        );

        // The decline itself is untouched — still queued, still a decline. It
        // needs no record: `flush_to_session` builds the `FileDecline` from the
        // entry alone (which `tests/roundtrip.rs` pins end to end).
        let queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].kind, Kind::Decline);
        assert_eq!(queued[0].message_id, theirs.id);
    }

    /// Seed a conversation for `peer` whose namespace holds a settled INBOUND
    /// file row keyed by `r` — a row keyed by an id **the peer chose**, since
    /// an inbound row's key in our own namespace is the sender's `FileRef.id`.
    ///
    /// That is what makes an id collision across two conversations ordinary
    /// rather than exotic, and it is the shape [`KeepRule`]'s peer filter
    /// exists for.
    fn seed_inbound_file(cs: &ChatStore, peer: &DeviceId, r: &FileRef) {
        let mut received = ChatRecord::file_in(peer, r);
        received.status = Status::Received;
        cs.append(&received).unwrap();
    }

    /// **THE PEER FILTER.** The outbox is shared by every conversation, so a
    /// keep set built without asking whose entry it is looking at keeps the
    /// wrong rows.
    ///
    /// A `message_id` is unique only within the peer that minted it, and an
    /// inbound row's key in our namespace is the **sender's** id — so two
    /// conversations naming the same id is a value one of the peers chose, not
    /// a coincidence worth discounting. Without the filter, Bob's queued entry
    /// keeps CAROL's row alive: her thread survives every delete, stays listed
    /// and undeletable until Bob comes back, and the user is told a queued
    /// message was kept for a conversation with nothing queued in it.
    ///
    /// Both peers hold a **decodable** queued entry here, deliberately. A
    /// second peer with nothing in the outbox — or with nothing the decoder can
    /// read — leaves the filtered and unfiltered rules indistinguishable, which
    /// is exactly how this went unpinned.
    #[test]
    fn delete_conversation_ignores_another_peers_queued_entry() {
        let (cs, _store, tmp) = new_store();
        let bob = DeviceId::from("pb-bob");
        let carol = DeviceId::from("pb-carol");

        // Bob has a file queued, so the outbox holds a decodable entry keyed by
        // its message id.
        let shared = FileRef::new("waiting.mkv", 30).unwrap();
        let (_staging, blob) = staged_blob(tmp.path(), &shared.id);
        cs.append(&ChatRecord::file_out(
            &bob,
            &shared,
            FileMeta::new(&shared.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(cs
            .enqueue_file(
                &bob,
                &shared,
                &StagedFile {
                    name: "waiting.mkv".into(),
                    size: 30,
                    staged_path: blob,
                },
            )
            .unwrap());

        // Carol sent us a file whose own id happens to be that same value, and
        // separately has a text of ours queued for her — a decodable entry of
        // her own, so this is not "one peer has a queue and one does not".
        seed_inbound_file(&cs, &carol, &shared);
        let hers = ChatMessage::new("see you soon").unwrap();
        cs.enqueue(&carol, &hers).unwrap();
        assert_eq!(cs.history(&carol).unwrap().len(), 2);

        assert_eq!(
            cs.delete_conversation(&carol).unwrap(),
            1,
            "the row Bob's queue happens to name is still Carol's to delete"
        );

        let left = cs.history(&carol).unwrap();
        assert_eq!(left.len(), 1, "only her own queued text survives: {left:?}");
        assert_eq!(left[0].id, hers.id);

        // Bob's conversation is untouched, and so is the entry behind it.
        assert_eq!(cs.history(&bob).unwrap().len(), 1);
        assert_eq!(cs.outbox_for(&bob).unwrap().len(), 1);
    }

    /// The same filter, in selection form — and here it is `kept` that lies.
    /// Naming Carol's row as kept tells the user a file they turned down is
    /// still on its way out, and refuses to delete it on that basis.
    #[test]
    fn delete_messages_ignores_another_peers_queued_entry() {
        let (cs, _store, tmp) = new_store();
        let bob = DeviceId::from("pb-bob");
        let carol = DeviceId::from("pb-carol");

        let shared = FileRef::new("waiting.mkv", 30).unwrap();
        let (_staging, blob) = staged_blob(tmp.path(), &shared.id);
        cs.append(&ChatRecord::file_out(
            &bob,
            &shared,
            FileMeta::new(&shared.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(cs
            .enqueue_file(
                &bob,
                &shared,
                &StagedFile {
                    name: "waiting.mkv".into(),
                    size: 30,
                    staged_path: blob,
                },
            )
            .unwrap());

        seed_inbound_file(&cs, &carol, &shared);
        let hers = ChatMessage::new("see you soon").unwrap();
        cs.enqueue(&carol, &hers).unwrap();

        let (removed, kept) = cs
            .delete_messages(&carol, &[shared.id.clone(), hers.id.clone()])
            .unwrap();
        assert_eq!(removed, 1, "Bob's queue does not protect Carol's row");
        assert_eq!(
            kept,
            vec![hers.id.clone()],
            "and only what CAROL's queue backs is reported kept: {kept:?}"
        );
        assert!(cs.get(&carol, &shared.id).unwrap().is_none());
        assert_eq!(cs.outbox_for(&bob).unwrap().len(), 1, "Bob still queued");
    }

    /// THE OTHER TRAP: the keep set must be **complete**. The lenient outbox
    /// readers skip an entry they cannot decode, so a wholly-unreadable outbox
    /// reads back as "nothing is queued" — and a delete driven by that would
    /// remove the record under a queued file, which is precisely the state the
    /// drain reads as "nothing will ever settle this" before deleting the
    /// staged bytes.
    ///
    /// So this refuses, and the last block proves the assertion is not vacuous:
    /// the lenient keep set really does authorise removing that record.
    #[test]
    fn delete_conversation_refuses_when_the_outbox_cannot_be_read_completely() {
        let (cs, store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");

        let queued = FileRef::new("waiting.mkv", 30).unwrap();
        let (_staging, blob) = staged_blob(tmp.path(), &queued.id);
        cs.append(&ChatRecord::file_out(
            &peer,
            &queued,
            FileMeta::new(&queued.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(
            cs.enqueue_file(
                &peer,
                &queued,
                &StagedFile {
                    name: "waiting.mkv".into(),
                    size: 30,
                    staged_path: blob,
                },
            )
            .unwrap(),
            "the row seeded above is there, so it queues"
        );

        // Every outbox row becomes undecodable — a newer schema, as seen by an
        // older binary, applied to the whole namespace.
        for (key, _) in store.list(OUTBOX_NS).unwrap() {
            store
                .put(OUTBOX_NS, &key, b"{\"from\":\"the-future\"}")
                .unwrap();
        }

        let err = cs
            .delete_conversation(&peer)
            .expect_err("an incomplete keep set must not authorise a delete");
        assert!(matches!(err, ChatError::QueueUnreadable(_)), "{err:?}");
        assert_eq!(
            cs.history(&peer).unwrap().len(),
            1,
            "and the record backing the queued file is still there"
        );

        // Not vacuous: the lenient reader — the one every delivery path uses —
        // reports an empty queue here, which would have let the record go.
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "the lenient reader cannot tell this from an empty queue"
        );
    }

    // ── delete_messages — the same keep rule, applied to a selection ────────

    /// Seed `n` settled outgoing text rows and return their ids, oldest first.
    fn seed_settled(cs: &ChatStore, peer: &DeviceId, n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let m = ChatMessage::new(&format!("message {i}")).unwrap();
                cs.append(&ChatRecord::sent(peer, &m)).unwrap();
                m.id
            })
            .collect()
    }

    /// The ordinary case: some of the thread goes, the rest stays exactly as it
    /// was, and nothing is kept because nothing is waiting to be sent.
    #[test]
    fn delete_messages_removes_only_the_named_rows() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let ids = seed_settled(&cs, &peer, 5);

        let (removed, kept) = cs
            .delete_messages(&peer, &[ids[1].clone(), ids[3].clone()])
            .unwrap();
        assert_eq!(removed, 2);
        assert!(kept.is_empty(), "nothing was queued, so nothing is kept");

        let left: Vec<String> = cs
            .history(&peer)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(left, vec![ids[0].clone(), ids[2].clone(), ids[4].clone()]);
        // The survivors are untouched, not merely present: a delete does not
        // rewrite what it leaves.
        assert_eq!(
            cs.get(&peer, &ids[0]).unwrap().unwrap().body,
            "message 0",
            "the rows around the selection are left whole"
        );
    }

    /// **THE TRAP, in selection form.** Picking the bubble of a file that is
    /// still queued must not take the record out from under its outbox entry:
    /// the drain reads a missing record as "nothing will ever settle this" and
    /// throws the only staged copy away. So it is kept, the caller is *told*
    /// which id was kept — that is what lets the surface say why — and the
    /// queue is left able to deliver it.
    #[test]
    fn delete_messages_keeps_a_queued_file_and_reports_it_by_id() {
        let (cs, _store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = seed_settled(&cs, &peer, 2);

        // TWO queued files, not one. A `kept` that reported only its first id —
        // and a surface that then said "1 kept" about two — is invisible
        // against a one-element list, so every id it is asked about is asked
        // about twice over.
        let mut queued = Vec::new();
        let mut blobs = Vec::new();
        for name in ["waiting.mkv", "also-waiting.iso"] {
            let r = FileRef::new(name, 30).unwrap();
            let (_staging, blob) = staged_blob(tmp.path(), &r.id);
            cs.append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, 30, None),
                Status::Staging,
            ))
            .unwrap();
            assert!(
                cs.enqueue_file(
                    &peer,
                    &r,
                    &StagedFile {
                        name: name.into(),
                        size: 30,
                        staged_path: blob.clone(),
                    },
                )
                .unwrap(),
                "the row seeded above is there, so it queues"
            );
            queued.push(r);
            blobs.push(blob);
        }

        let (removed, kept) = cs
            .delete_messages(
                &peer,
                &[
                    settled[0].clone(),
                    queued[0].id.clone(),
                    queued[1].id.clone(),
                ],
            )
            .unwrap();
        assert_eq!(removed, 1, "the settled message, and only that");
        assert_eq!(
            kept,
            vec![queued[0].id.clone(), queued[1].id.clone()],
            "BOTH queued files are named, not just counted and not just the \
             first: {kept:?}"
        );

        // The consequence, not just the rows. `reopen_for_retry` is the exact
        // step the drain takes before sending a queued file; a `false` here is
        // the state in which it releases the entry and deletes the bytes.
        for r in &queued {
            assert!(
                cs.reopen_for_retry(&peer, &r.id).unwrap(),
                "the queue must still be able to deliver every file it kept"
            );
        }
        assert_eq!(cs.outbox_for(&peer).unwrap().len(), 2, "still queued");
        for blob in &blobs {
            assert!(
                std::path::Path::new(blob).exists(),
                "a queued file's only copy must survive deleting its bubble"
            );
        }
        assert_eq!(
            cs.outbox_owned_blobs().unwrap().len(),
            2,
            "and the queue still owns them, so no sweep will collect them"
        );
    }

    /// A queued DECLINE must not be kept here either — and this is where it
    /// bites hardest, because `delete_messages` reports the ids it kept.
    ///
    /// The decline's `message_id` is the **sender's** `FileRef` id, which in
    /// our own namespace names the INBOUND row we refused. Keeping it puts that
    /// id in `kept`, so the surface tells the user a file they **turned down**
    /// is still on its way out, and refuses to delete the bubble on that basis.
    #[test]
    fn delete_messages_does_not_keep_an_inbound_row_for_a_queued_decline() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = seed_settled(&cs, &peer, 1);

        // Bob offered a file, we declined, and Bob had already dropped — so the
        // refusal is queued for whenever he returns.
        let theirs = FileRef::new("theirs.iso", 900).unwrap();
        let mut declined = ChatRecord::file_in(&peer, &theirs);
        declined.status = Status::Declined;
        cs.append(&declined).unwrap();
        assert!(cs
            .enqueue_decline(&peer, &FileDecline::new(&theirs.id))
            .unwrap());

        let (removed, kept) = cs
            .delete_messages(&peer, &[settled[0].clone(), theirs.id.clone()])
            .unwrap();
        assert_eq!(removed, 2, "the declined row is the user's to delete");
        assert!(
            kept.is_empty(),
            "nothing the user turned down is 'still being sent': {kept:?}"
        );
        assert!(cs.history(&peer).unwrap().is_empty());

        // The decline itself is untouched — still queued, still a decline. It
        // needs no record: `flush_to_session` builds the `FileDecline` from the
        // entry alone.
        let queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].kind, Kind::Decline);
        assert_eq!(queued[0].message_id, theirs.id);
    }

    /// **THE STAGING WINDOW, in selection form.** `begin_file_send` writes the
    /// row `Staging` synchronously and the copy — minutes, for a multi-GB
    /// file — runs before `enqueue_file` puts anything in the outbox. For that
    /// whole window an outbox-only keep rule does not name the row, so deleting
    /// its bubble would leave the finished copy to queue an entry with no
    /// record behind it: the trap above, reached by another road.
    #[test]
    fn delete_messages_keeps_a_file_that_is_still_being_staged() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = seed_settled(&cs, &peer, 1);

        let staging = FileRef::new("holiday.mp4", 8_000_000_000).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &staging,
            FileMeta::new(
                &staging.name,
                staging.size,
                Some("/home/me/holiday.mp4".into()),
            ),
            Status::Staging,
        ))
        .unwrap();
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "nothing is queued until the copy finishes — that IS the window"
        );

        let (removed, kept) = cs
            .delete_messages(&peer, &[settled[0].clone(), staging.id.clone()])
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(kept, vec![staging.id.clone()]);

        let row = cs.get(&peer, &staging.id).unwrap().expect("the row stays");
        assert_eq!(row.status, Status::Staging);
        assert_eq!(
            row.file.as_ref().unwrap().local_path.as_deref(),
            Some("/home/me/holiday.mp4"),
            "kept whole — the delete does not rewrite what it keeps"
        );
        // And the copy can still finish into it, which is the only reason
        // keeping it matters.
        assert!(
            cs.enqueue_file(
                &peer,
                &staging,
                &StagedFile {
                    name: "holiday.mp4".into(),
                    size: 8_000_000_000,
                    staged_path: format!("/data/outbox-blobs/{}", staging.id),
                },
            )
            .unwrap(),
            "the finished copy queues normally, exactly as if nothing happened"
        );
    }

    /// An id in no namespace at all is neither removed nor kept. `kept` means
    /// "refused because something still needs it" — reporting an id that was
    /// never there would tell the user a message they cannot see is still on
    /// its way out.
    #[test]
    fn delete_messages_ignores_an_id_that_is_not_there() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let ids = seed_settled(&cs, &peer, 2);
        // A real row, but in someone else's thread: naming it here must not
        // reach across conversations either.
        let other = DeviceId::from("pb-carol");
        let elsewhere = seed_settled(&cs, &other, 1);

        let (removed, kept) = cs
            .delete_messages(
                &peer,
                &[
                    ids[0].clone(),
                    "0000000000000-never-existed".to_string(),
                    elsewhere[0].clone(),
                ],
            )
            .unwrap();
        assert_eq!(removed, 1, "only the one row that was actually there");
        assert!(kept.is_empty(), "an absent id is not 'kept': {kept:?}");
        assert_eq!(cs.history(&other).unwrap().len(), 1, "blast radius is one");

        // A repeated id is answered once, so a caller that sent the same
        // selection twice over cannot inflate its own report.
        //
        // The duplicate is a KEPT id on purpose. Duplicating a removable one
        // proves nothing: the row is gone by the second pass, so the second
        // answer is "not there" whether or not anything deduplicates — the
        // dedup is unobservable and deleting it leaves the suite green. A kept
        // id is answered from a row that is still there both times, so a
        // missing dedup shows up directly as the count the user is given:
        // a double-tapped selection reporting "2 kept" for one file.
        let queued = ChatMessage::new("not sent yet").unwrap();
        cs.enqueue(&peer, &queued).unwrap();
        let (removed, kept) = cs
            .delete_messages(
                &peer,
                &[ids[1].clone(), queued.id.clone(), queued.id.clone()],
            )
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            kept,
            vec![queued.id.clone()],
            "one entry for one file, however many times it was asked for"
        );
    }

    /// A row this build cannot decode is still the user's to delete — and it is
    /// the one they can see nothing of, since `history` skips it. Keying the
    /// delete off decoded records would leave it behind for good.
    #[test]
    fn delete_messages_removes_a_selected_row_this_build_cannot_read() {
        let (cs, store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let readable = seed_settled(&cs, &peer, 1);
        store
            .put(
                &namespace(&peer),
                "0000000000009",
                b"{\"from\":\"the-future\"}",
            )
            .unwrap();

        let (removed, kept) = cs
            .delete_messages(&peer, &["0000000000009".to_string()])
            .unwrap();
        assert_eq!(removed, 1, "the unreadable row goes when it is named");
        assert!(
            kept.is_empty(),
            "it is not staging as far as anyone can tell: {kept:?}"
        );
        assert!(
            store
                .get(&namespace(&peer), "0000000000009")
                .unwrap()
                .is_none(),
            "read back through the RAW store, which no decode can hide"
        );
        assert_eq!(
            cs.history(&peer).unwrap().len(),
            1,
            "and the readable row beside it is untouched"
        );
        assert_eq!(cs.history(&peer).unwrap()[0].id, readable[0]);
    }

    /// **THE OTHER TRAP.** The keep rule must be **complete**. The lenient
    /// outbox readers skip an entry they cannot decode, so a wholly-unreadable
    /// outbox reads back as "nothing is queued" — and a delete driven by that
    /// removes the record under a queued file, which is precisely the state the
    /// drain reads as "nothing will ever settle this" before deleting the
    /// staged bytes.
    ///
    /// So the whole call refuses, exactly as `delete_conversation` does, and
    /// **nothing is deleted on the way to refusing** — including the plain
    /// settled message in the same selection, which is why the rule is
    /// established before the first delete rather than per row.
    #[test]
    fn delete_messages_refuses_when_the_outbox_cannot_be_read_completely() {
        let (cs, store, tmp) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = seed_settled(&cs, &peer, 1);

        let queued = FileRef::new("waiting.mkv", 30).unwrap();
        let (_staging, blob) = staged_blob(tmp.path(), &queued.id);
        cs.append(&ChatRecord::file_out(
            &peer,
            &queued,
            FileMeta::new(&queued.name, 30, None),
            Status::Staging,
        ))
        .unwrap();
        assert!(cs
            .enqueue_file(
                &peer,
                &queued,
                &StagedFile {
                    name: "waiting.mkv".into(),
                    size: 30,
                    staged_path: blob,
                },
            )
            .unwrap());

        // Every outbox row becomes undecodable — a newer schema, as seen by an
        // older binary, applied to the whole namespace.
        for (key, _) in store.list(OUTBOX_NS).unwrap() {
            store
                .put(OUTBOX_NS, &key, b"{\"from\":\"the-future\"}")
                .unwrap();
        }

        let err = cs
            .delete_messages(&peer, &[settled[0].clone(), queued.id.clone()])
            .expect_err("an incomplete keep rule must not authorise a delete");
        assert!(matches!(err, ChatError::QueueUnreadable(_)), "{err:?}");
        assert_eq!(
            cs.history(&peer).unwrap().len(),
            2,
            "and nothing was deleted on the way to refusing"
        );

        // Not vacuous: the lenient reader — the one every delivery path uses —
        // reports an empty queue here, which would have let the record go.
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "the lenient reader cannot tell this from an empty queue"
        );
    }

    /// A queued TEXT message is kept just like a queued file. Nothing would
    /// lose bytes here — `record_sent` can rebuild a text row from its entry
    /// alone — but the row would vanish now and REAPPEAR when the message is
    /// finally delivered, which is the message that comes back from the dead a
    /// delete exists to rule out.
    #[test]
    fn delete_messages_keeps_a_queued_text_message() {
        let (cs, _store, _dir) = new_store();
        let peer = DeviceId::from("pb-bob");
        let settled = seed_settled(&cs, &peer, 1);
        let pending = ChatMessage::new("not sent yet").unwrap();
        cs.enqueue(&peer, &pending).unwrap();

        let (removed, kept) = cs
            .delete_messages(&peer, &[settled[0].clone(), pending.id.clone()])
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(kept, vec![pending.id.clone()]);
        assert_eq!(cs.outbox_for(&peer).unwrap().len(), 1, "still queued");
    }
}
