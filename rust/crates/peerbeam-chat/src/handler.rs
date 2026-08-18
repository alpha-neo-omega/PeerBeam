//! The Chat channel's MessageHandler: decode → dedup → persist → notify.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::{
    ChatMessage, FileDecline, FileRef, Reaction, Receipt, MSG_FILE_DECLINE, MSG_FILE_REF,
    MSG_REACTION, MSG_RECEIPT, MSG_TEXT,
};
use crate::record::{ChatRecord, Direction, Status};
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
    /// Build a handler + the peer slot the caller must `set` after the
    /// handshake (before the session run loop dispatches any frame).
    #[must_use]
    pub fn new(
        store: ChatStore,
        sink: ReceivedSink,
    ) -> (Arc<ChatHandler>, Arc<OnceLock<DeviceId>>) {
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
        // Peer must be bound before any frame is dispatched (the caller binds it
        // right after the handshake, before spawning the run loop). If somehow
        // unbound, treat as a channel error rather than panicking.
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("chat peer not bound".into()));
        };
        match frame.message_type.get() {
            MSG_TEXT => {
                let msg = ChatMessage::from_frame(&frame)?; // ChatError -> SessionError
                                                            // Dedup by id (idempotent re-delivery).
                if self
                    .store
                    .contains(peer, &msg.id)
                    .map_err(SessionError::from)?
                {
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
                if self
                    .store
                    .contains(peer, &r.id)
                    .map_err(SessionError::from)?
                {
                    return Ok(());
                }
                let rec = ChatRecord::file_in(peer, &r);
                self.store.append(&rec).map_err(SessionError::from)?;
                (self.sink)(rec);
                Ok(())
            }
            // MUST stay above the `other =>` fallback: a `FileDecline` ships
            // OPTIONAL, so an arm placed below it would be swallowed as an
            // "unknown optional type", return `Ok`, and settle nothing —
            // silently, with no error anywhere to notice.
            MSG_FILE_DECLINE => {
                // The peer turned down a file WE offered. Only our own OUTGOING
                // file row can be declined, and only while it is still in
                // flight. `settle_file_row` enforces exactly that — the same
                // guard every other wire-driven write goes through, so a peer
                // cannot use a decline to rewrite a text row, an inbound row,
                // or a row that already settled. A decline naming any of those,
                // or an id we have never seen, is a silent success: it neither
                // writes nor fails the channel.
                //
                // No dedup and no sink: the write is idempotent (a second
                // decline finds the row already settled and no-ops), and a
                // status change on an existing row is not a new record to
                // surface.
                let d = FileDecline::from_frame(&frame)?;
                let _ = self
                    .store
                    .settle_file_row(peer, &d.id, Direction::Out, Status::Declined)
                    .map_err(SessionError::from)?;
                Ok(())
            }
            // MUST stay above the `other =>` fallback, for the same reason
            // `MSG_FILE_DECLINE` does: a `Reaction` ships OPTIONAL, so an arm
            // below it would be swallowed as an "unknown optional type",
            // return `Ok`, and apply nothing — silently.
            MSG_REACTION => {
                // The peer reacted to a message in this conversation, so the
                // reaction is theirs: `Direction::In`. `apply_reaction`
                // authorizes against the stored record inside this peer's own
                // namespace, so a `target_id` naming a message in a different
                // conversation — or one we have deleted — finds nothing and is
                // a silent success rather than a channel failure.
                //
                // No dedup and no sink, exactly as for a decline: the write is
                // idempotent because the message states its intended end state,
                // and a change to an existing row is not a new record to
                // surface.
                let r = Reaction::from_frame(&frame)?;
                let _ = self
                    .store
                    .apply_reaction(peer, &r.target_id, &r.emoji, Direction::In, r.remove)
                    .map_err(SessionError::from)?;
                Ok(())
            }
            // Above the fallback, like every OPTIONAL type before it: an arm
            // below would be swallowed as "unknown optional", return Ok, and
            // apply nothing.
            MSG_RECEIPT => {
                // The peer read our messages up to a watermark. `apply_receipt`
                // marks only our own OUTGOING rows inside this peer's
                // namespace, so a receipt can neither reach another
                // conversation nor rewrite a row the peer itself sent. A
                // watermark naming an id we do not have marks whatever is below
                // it and is otherwise a silent success.
                //
                // No sink: this changes existing rows rather than adding one.
                let r = Receipt::from_frame(&frame)?;
                let _ = self
                    .store
                    .apply_receipt(peer, &r.read_through, &r.timestamp)
                    .map_err(SessionError::from)?;
                Ok(())
            }
            // MESSAGE_REGISTRY.md §6 — unknown type: OPTIONAL means skip and keep
            // the channel; required means fail this channel only. (Increment 0.)
            other => {
                if frame.flags.is_optional() {
                    // Ignored on purpose: a newer peer sent an additive message
                    // this build does not implement. (No log — this crate has no
                    // tracing dependency and one is not worth adding for a
                    // skipped frame.)
                    return Ok(());
                }
                Err(SessionError::FrameDecode(format!(
                    "unsupported chat message type {other} (required)"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{FileMeta, Kind, Status};
    use bytes::Bytes;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType};
    use std::sync::Mutex;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn store(seed: u8) -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[seed; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app), dir)
    }

    /// A handler plus everything a test needs to see what it did: the peer slot
    /// to bind, the store behind it, a receiver of every record handed to the
    /// sink, and the tempdir that must outlive both.
    fn new_handler() -> (
        Arc<ChatHandler>,
        Arc<OnceLock<DeviceId>>,
        ChatStore,
        UnboundedReceiver<ChatRecord>,
        tempfile::TempDir,
    ) {
        let (cs, dir) = store(10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink: ReceivedSink = Arc::new(move |rec| {
            let _ = tx.send(rec);
        });
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        (handler, peer_slot, cs, rx, dir)
    }

    #[tokio::test]
    async fn handle_rejects_frame_when_peer_unbound() {
        let (cs, _dir) = store(1);
        let sink: ReceivedSink = Arc::new(|_rec| {});
        let (handler, _peer_slot) = ChatHandler::new(cs, sink);
        let msg = ChatMessage::new("hi").unwrap();
        let frame = msg.to_frame(ChannelId::new(1)).unwrap();
        let err = handler.handle(frame).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
    }

    #[tokio::test]
    async fn handle_persists_and_notifies_once_bound() {
        let (cs, _dir) = store(2);
        let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cl = received.clone();
        let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        let msg = ChatMessage::new("hello").unwrap();
        let frame = msg.to_frame(ChannelId::new(1)).unwrap();
        handler.handle(frame).await.unwrap();

        assert_eq!(received.lock().unwrap().len(), 1);
        assert_eq!(received.lock().unwrap()[0].body, "hello");
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].body, "hello");
    }

    #[tokio::test]
    async fn handle_dedups_same_message_id() {
        let (cs, _dir) = store(3);
        let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cl = received.clone();
        let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        let msg = ChatMessage::new("hello").unwrap();
        let frame1 = msg.to_frame(ChannelId::new(1)).unwrap();
        // Same message id sent twice (idempotent re-delivery).
        let frame2 = SessionFrame::new(
            ChannelId::new(1),
            ChatMessage::message_type(),
            frame1.flags,
            Bytes::from(frame1.payload.to_vec()),
        );
        handler.handle(frame1).await.unwrap();
        handler.handle(frame2).await.unwrap();

        assert_eq!(received.lock().unwrap().len(), 1, "sink fires once");
        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "store holds one record");
    }

    #[tokio::test]
    async fn handle_rejects_malformed_frame_without_panicking() {
        let (cs, _dir) = store(4);
        let sink: ReceivedSink = Arc::new(|_rec| {});
        let (handler, peer_slot) = ChatHandler::new(cs, sink);
        let _ = peer_slot.set(DeviceId::from("pb-sender"));

        // Peer is bound, but the frame is neither the right message type nor
        // valid JSON — `handle` must return an error, not panic.
        let bad = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(999),
            MessageFlags::END_OF_MESSAGE,
            Bytes::from_static(b"not json"),
        );
        let err = handler.handle(bad).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
    }

    #[tokio::test]
    async fn an_inbound_receipt_marks_our_messages_read() {
        let (cs, _dir) = store(5);
        let sink: ReceivedSink = Arc::new(|_| {});
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        let frame = Receipt::read_through(&m.id)
            .to_frame(ChannelId::new(1))
            .unwrap();
        handler.handle(frame).await.unwrap();

        assert!(cs.history(&peer).unwrap()[0].read_at.is_some());
    }

    #[tokio::test]
    async fn an_inbound_reaction_is_applied_to_our_row() {
        let (cs, _dir) = store(5);
        let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cl = received.clone();
        let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        let m = ChatMessage::new("ship it").unwrap();
        cs.append(&ChatRecord::sent(&peer, &m)).unwrap();

        let frame = Reaction::add(&m.id, "\u{1F44D}")
            .to_frame(ChannelId::new(1))
            .unwrap();
        handler.handle(frame).await.unwrap();

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist[0].reactions.len(), 1);
        assert_eq!(hist[0].reactions[0].by, Direction::In);
        assert!(
            received.lock().unwrap().is_empty(),
            "a reaction is a change to an existing row, not a new record"
        );
    }

    #[tokio::test]
    async fn an_inbound_reaction_naming_nothing_keeps_the_channel() {
        // A reaction for a message we deleted — or never had — must be a silent
        // success. Failing the channel would let a peer tear down a
        // conversation by reacting to an id we no longer hold.
        let (cs, _dir) = store(5);
        let sink: ReceivedSink = Arc::new(|_| {});
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let _ = peer_slot.set(DeviceId::from("pb-sender"));

        let frame = Reaction::add("no-such-id", "\u{1F44D}")
            .to_frame(ChannelId::new(1))
            .unwrap();
        assert!(handler.handle(frame).await.is_ok());
    }

    /// MESSAGE_REGISTRY.md §6: an unknown MessageType flagged OPTIONAL must be
    /// ignored — the message is skipped and the channel survives. Without this,
    /// adding any second chat message type tears down an older peer's channel.
    ///
    /// Uses `MessageType::new(999)` — a deliberately unassigned id — rather
    /// than `2`, since `MSG_FILE_REF` (2) is now a *known* type with its own
    /// dispatch arm and would no longer exercise this fallback.
    #[tokio::test]
    async fn handle_ignores_unknown_optional_message_type() {
        let (cs, _dir) = store(5);
        let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cl = received.clone();
        let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(cs.clone(), sink);
        let peer = DeviceId::from("pb-sender");
        let _ = peer_slot.set(peer.clone());

        // An additive future message type this build does not implement,
        // marked OPTIONAL by its sender, carrying a body this build cannot
        // parse.
        let unknown = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(999),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{\"whatever\":true}"),
        );

        handler
            .handle(unknown)
            .await
            .expect("optional unknown is ignored, not an error");

        assert!(received.lock().unwrap().is_empty(), "sink must not fire");
        assert!(
            cs.history(&peer).unwrap().is_empty(),
            "nothing may be persisted for an ignored frame"
        );
    }

    /// The other half of §6: an unknown MessageType that is NOT optional was
    /// required, so it still fails this channel (and only this channel).
    ///
    /// Uses `MessageType::new(999)` for the same reason as the test above.
    #[tokio::test]
    async fn handle_still_rejects_unknown_required_message_type() {
        let (cs, _dir) = store(6);
        let sink: ReceivedSink = Arc::new(|_rec| {});
        let (handler, peer_slot) = ChatHandler::new(cs, sink);
        let _ = peer_slot.set(DeviceId::from("pb-sender"));

        let required = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(999),
            MessageFlags::END_OF_MESSAGE, // no OPTIONAL bit
            Bytes::from_static(b"{\"whatever\":true}"),
        );

        let err = handler.handle(required).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
    }

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
        handler
            .handle(r.to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

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
        handler
            .handle(r.to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();
        handler
            .handle(r.to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();
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
        frame.payload =
            bytes::Bytes::from_static(br#"{"id":"x","timestamp":"t","name":"../escape","size":1}"#);
        assert!(handler.handle(frame).await.is_err());
        assert!(cs.history(&peer).unwrap().is_empty());
    }

    // ── FileDecline ─────────────────────────────────────────────────────────
    //
    // A decline is the only thing that makes a refusal terminal for the sender:
    // without it a queued file is retried keep-forever and re-prompts its
    // receiver on every drain tick. Every write it drives goes through
    // `ChatStore::settle_file_row`, so the tests below are as much about what a
    // decline CANNOT touch as about what it settles.

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
        handler
            .handle(d.to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

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

    /// A chat message id is a wire field the peer has already seen, and it is
    /// also what a peer puts in `transfer_id`. So a decline naming one of our
    /// TEXT rows must change nothing — otherwise a decline is a write primitive
    /// aimed at any row in the conversation.
    #[tokio::test]
    async fn a_decline_naming_a_text_row_is_ignored() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        let msg = ChatMessage::new("our own outgoing text").unwrap();
        store.append(&ChatRecord::sent(&peer, &msg)).unwrap();

        handler
            .handle(
                FileDecline::new(&msg.id)
                    .to_frame(ChannelId::new(1))
                    .unwrap(),
            )
            .await
            .unwrap();

        let rec = store.get(&peer, &msg.id).unwrap().unwrap();
        assert_eq!(rec.status, Status::Sent, "a text row is not declinable");
        assert_eq!(rec.kind, Kind::Text);
        assert_eq!(rec.body, "our own outgoing text");
    }

    /// A settled row is final. Without this, a peer that already received a
    /// file could re-declare it declined and make the conversation assert the
    /// opposite of what happened.
    #[tokio::test]
    async fn a_decline_for_an_already_settled_row_is_ignored() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        let r = FileRef::new("done.pdf", 7).unwrap();
        store
            .append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, r.size, None),
                Status::Sent,
            ))
            .unwrap();

        handler
            .handle(FileDecline::new(&r.id).to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

        assert_eq!(
            store.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Sent,
            "a delivered file cannot be retroactively declined"
        );
    }

    #[tokio::test]
    async fn a_decline_for_an_unknown_id_is_a_silent_no_op() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        handler
            .handle(
                FileDecline::new("never-existed")
                    .to_frame(ChannelId::new(1))
                    .unwrap(),
            )
            .await
            .expect("an unknown id must not fail the channel");
        assert!(store.history(&peer).unwrap().is_empty());
    }

    /// The dispatch arm has to sit BEFORE `handle`'s `other =>` fallback. A
    /// `FileDecline` ships OPTIONAL, so an arm placed after the fallback would
    /// be swallowed by it and nothing would ever settle — silently, with the
    /// call still returning `Ok`. This is what catches that: same frame, same
    /// `Ok`, but a row that actually moved.
    #[tokio::test]
    async fn the_decline_arm_precedes_the_optional_unknown_type_fallback() {
        let (handler, peer_slot, store, _rx, _tmp) = new_handler();
        let peer = DeviceId::from("pb-bob".to_string());
        let _ = peer_slot.set(peer.clone());
        let r = FileRef::new("ordered.pdf", 3).unwrap();
        store
            .append(&ChatRecord::file_out(
                &peer,
                &r,
                FileMeta::new(&r.name, r.size, None),
                Status::Transferring,
            ))
            .unwrap();

        let frame = FileDecline::new(&r.id).to_frame(ChannelId::new(1)).unwrap();
        assert!(
            frame.flags.is_optional(),
            "the fallback only swallows OPTIONAL frames — if this stops being \
             optional the test no longer proves ordering"
        );
        handler.handle(frame).await.unwrap();

        assert_eq!(
            store.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Declined,
            "the decline was swallowed by the unknown-type fallback"
        );
    }
}
