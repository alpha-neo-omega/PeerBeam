//! The Notes channel's MessageHandler: decode → gate → merge → answer once.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::gate::may_sync_notes;
use crate::message::{NoteBatch, MSG_NOTE_BATCH};
use crate::store::NoteStore;

/// Called with the batches this device owes the peer in reply, once an incoming
/// exchange has finished arriving.
///
/// A callback rather than a session handle, so this crate stays free of the
/// transport: the FFI and the CLI each already know how to open a channel and
/// send, and neither needs a second implementation living here.
pub type ReplySink = Arc<dyn Fn(Vec<NoteBatch>) + Send + Sync>;

/// Serves inbound Notes-channel frames for one session.
pub struct NotesHandler {
    store: NoteStore,
    trust: Arc<dyn TrustStore>,
    peer: Arc<OnceLock<DeviceId>>,
    reply: ReplySink,
}

impl NotesHandler {
    /// Build a handler and the peer slot the caller must `set` after the
    /// handshake, before any frame is dispatched.
    #[must_use]
    pub fn new(
        store: NoteStore,
        trust: Arc<dyn TrustStore>,
        reply: ReplySink,
    ) -> (Arc<NotesHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(NotesHandler {
            store,
            trust,
            peer: peer.clone(),
            reply,
        });
        (handler, peer)
    }
}

#[async_trait]
impl MessageHandler for NotesHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::NOTES
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("notes peer not bound".into()));
        };
        if frame.message_type.get() != MSG_NOTE_BATCH {
            // MESSAGE_REGISTRY.md §6: an unknown OPTIONAL type is skipped and
            // the channel continues.
            return Ok(());
        }

        // **Gated on the way in as well as the way out.** Unlike chat — where a
        // message that has already arrived is in hand and refusing to store it
        // would lose the user's data — a note batch is someone else's data being
        // written into this device's store, so a peer that may not sync notes
        // must not be able to put any there. Asked per batch, so revoking stops
        // the next one rather than the next reconnect.
        if !may_sync_notes(self.trust.as_ref(), peer) {
            return Ok(());
        }

        let batch =
            NoteBatch::from_frame(&frame).map_err(|e| SessionError::FrameDecode(e.to_string()))?;
        for note in &batch.notes {
            // A note that loses the conflict is a no-op; `merge` says so.
            self.store
                .merge(note)
                .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
        }

        // Answer once, after the last batch of an incoming exchange — and never
        // to a reply, which is what makes a sync two passes instead of an
        // endless volley.
        if !batch.more && !batch.reply {
            let mine = self
                .store
                .all()
                .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
            (self.reply)(NoteBatch::split(mine, true));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::Note;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{Permission, PermissionSet, TrustRecord};
    use peerbeam_domain::port::{AppStore, EncryptionProvider};
    use peerbeam_domain::session::{ChannelId, MessageFlags};
    use std::sync::Mutex;

    struct Trust(Option<TrustRecord>);

    impl TrustStore for Trust {
        fn record(&self, _record: TrustRecord) -> peerbeam_domain::Result<()> {
            Ok(())
        }
        fn lookup(&self, _device: &DeviceId) -> peerbeam_domain::Result<Option<TrustRecord>> {
            Ok(self.0.clone())
        }
        fn is_trusted(&self, _device: &DeviceId) -> bool {
            self.0.is_some()
        }
    }

    fn trust(notes_permitted: bool) -> Arc<dyn TrustStore> {
        Arc::new(Trust(Some(TrustRecord {
            device: DeviceId::from("pb-bob"),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved: true,
            permissions: PermissionSet::granted_on_approval()
                .set(Permission::Notes, notes_permitted),
            expires_at: None,
            mine: false,
            auto_accept: false,
        })))
    }

    fn store() -> (NoteStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (NoteStore::new(app), dir)
    }

    /// Every set of batches the handler asked to have sent, in order.
    type Sent = Arc<Mutex<Vec<Vec<NoteBatch>>>>;

    /// Builds a handler over `store`, returning the replies it was asked to
    /// send so a test can assert on the exchange rather than on the network.
    fn handler(store: NoteStore, trust: Arc<dyn TrustStore>) -> (Arc<NotesHandler>, Sent) {
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let seen = sent.clone();
        let sink: ReplySink = Arc::new(move |b| seen.lock().unwrap().push(b));
        let (h, slot) = NotesHandler::new(store, trust, sink);
        let _ = slot.set(DeviceId::from("pb-bob"));
        (h, sent)
    }

    fn note(id: &str, body: &str, at: &str, deleted: bool) -> Note {
        Note {
            id: id.to_string(),
            title: String::new(),
            body: body.to_string(),
            updated_at: at.to_string(),
            deleted,
        }
    }

    fn frame(batch: &NoteBatch) -> SessionFrame {
        batch.to_frame(ChannelId::new(1)).unwrap()
    }

    #[tokio::test]
    async fn an_incoming_batch_is_merged_and_answered_once() {
        let (ns, _dir) = store();
        ns.put(&note("mine", "ours", "2026-01-01T00:00:00Z", false))
            .unwrap();
        let (h, sent) = handler(ns.clone(), trust(true));

        let incoming = NoteBatch::new(
            vec![note("theirs", "from bob", "2026-01-01T00:00:01Z", false)],
            false,
            false,
        );
        h.handle(frame(&incoming)).await.unwrap();

        assert_eq!(ns.list().unwrap().len(), 2, "their note was not merged");
        let replies = sent.lock().unwrap();
        assert_eq!(replies.len(), 1, "exactly one answer");
        assert!(
            replies[0].iter().all(|b| b.reply),
            "an answer must be flagged as one"
        );
    }

    #[tokio::test]
    async fn a_reply_is_never_answered() {
        // Otherwise two devices would volley forever, each answering the other's
        // answer. This is what makes a sync exactly two passes.
        let (ns, _dir) = store();
        let (h, sent) = handler(ns, trust(true));

        let incoming = NoteBatch::new(vec![], false, true);
        h.handle(frame(&incoming)).await.unwrap();

        assert!(sent.lock().unwrap().is_empty(), "a reply was answered");
    }

    #[tokio::test]
    async fn an_intermediate_batch_is_merged_but_not_yet_answered() {
        let (ns, _dir) = store();
        let (h, sent) = handler(ns.clone(), trust(true));

        h.handle(frame(&NoteBatch::new(
            vec![note("a", "first", "2026-01-01T00:00:00Z", false)],
            true,
            false,
        )))
        .await
        .unwrap();
        assert_eq!(ns.list().unwrap().len(), 1);
        assert!(
            sent.lock().unwrap().is_empty(),
            "answered before the exchange finished arriving"
        );

        h.handle(frame(&NoteBatch::new(
            vec![note("b", "second", "2026-01-01T00:00:01Z", false)],
            false,
            false,
        )))
        .await
        .unwrap();
        assert_eq!(ns.list().unwrap().len(), 2);
        assert_eq!(sent.lock().unwrap().len(), 1, "answered once, at the end");
    }

    #[tokio::test]
    async fn a_peer_without_the_permission_writes_nothing_and_gets_nothing() {
        // Gated on the way IN, unlike chat: a batch is someone else's data being
        // written into this device's store, and a device the user did not grant
        // notes to must not be able to put any there — nor to learn what is here
        // by provoking an answer.
        let (ns, _dir) = store();
        let (h, sent) = handler(ns.clone(), trust(false));

        h.handle(frame(&NoteBatch::new(
            vec![note("theirs", "unwanted", "2026-01-01T00:00:00Z", false)],
            false,
            false,
        )))
        .await
        .unwrap();

        assert!(
            ns.list().unwrap().is_empty(),
            "an ungranted peer wrote a note"
        );
        assert!(
            sent.lock().unwrap().is_empty(),
            "an ungranted peer was told what notes exist here"
        );
    }

    #[tokio::test]
    async fn an_unknown_notes_message_type_is_skipped_and_the_channel_survives() {
        let (ns, _dir) = store();
        let (h, _sent) = handler(ns, trust(true));
        let f = SessionFrame::new(
            ChannelId::new(1),
            peerbeam_domain::session::MessageType::new(99),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            bytes::Bytes::from_static(b"{}"),
        );
        assert!(h.handle(f).await.is_ok());
    }

    #[tokio::test]
    async fn an_incoming_tombstone_deletes_a_note_we_still_hold() {
        let (ns, _dir) = store();
        ns.put(&note("n1", "here", "2026-01-01T00:00:00Z", false))
            .unwrap();
        let (h, _sent) = handler(ns.clone(), trust(true));

        h.handle(frame(&NoteBatch::new(
            vec![note("n1", "", "2026-01-01T00:00:05Z", true)],
            false,
            false,
        )))
        .await
        .unwrap();

        assert!(ns.list().unwrap().is_empty(), "the deletion did not apply");
        assert!(ns.get("n1").unwrap().unwrap().deleted);
    }
}
