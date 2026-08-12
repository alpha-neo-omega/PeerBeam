//! An AppStore-backed conversation store: one namespace per peer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::AppStore;

use crate::message::{ChatError, ChatMessage};
use crate::record::{ChatRecord, Direction, Status};

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ChatRecord, Direction, Status};
    use crate::ChatMessage;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
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
        let (cs, _dir) = store();

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
}
