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
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_domain::session::ChannelId;
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
}
