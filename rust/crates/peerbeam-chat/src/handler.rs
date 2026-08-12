//! The Chat channel's MessageHandler: decode → dedup → persist → notify.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::{ChatMessage, FileRef, MSG_FILE_REF, MSG_TEXT};
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
    use crate::record::{Kind, Status};
    use bytes::Bytes;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType};
    use std::sync::Mutex;

    fn store(seed: u8) -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[seed; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app), dir)
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
}
