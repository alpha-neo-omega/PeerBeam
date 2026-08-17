//! The Clipboard channel's MessageHandler: decode → validate → notify.
//!
//! Receiving is deliberately **ungated**. The opt-in setting and the
//! trusted-only rule govern what leaves this device (see [`crate::gate`]); a
//! clip that arrives is applied regardless, because the peer already made its
//! own decision to send it and the session it arrived on is authenticated
//! either way. That asymmetry is what lets a phone — which Android forbids from
//! ever auto-sending — still take part fully as a receiver.
//!
//! Nothing is recorded here. Unlike Presence there is no registry: a clip has
//! no "current value" worth holding, and keeping a list of everything the user
//! copied would build, one clip at a time, exactly the durable log of secrets
//! this feature promises not to keep (I4). The clip goes to the sink and is
//! gone.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::{Clip, MSG_CLIP};

/// Called with each accepted clip so a surface can apply it to the system
/// clipboard and tell the user it happened.
pub type ClipboardSink = Arc<dyn Fn(DeviceId, Clip) + Send + Sync>;

/// Serves inbound Clipboard-channel frames for one session. The session peer is
/// bound once, after the handshake, via the returned [`OnceLock`] — the same
/// contract as `ChatHandler` and `PresenceHandler`.
pub struct ClipboardHandler {
    peer: Arc<OnceLock<DeviceId>>,
    sink: ClipboardSink,
}

impl ClipboardHandler {
    /// Build a handler + the peer slot the caller must `set` after the
    /// handshake (before the session run loop dispatches any frame).
    #[must_use]
    pub fn new(sink: ClipboardSink) -> (Arc<ClipboardHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(ClipboardHandler {
            peer: peer.clone(),
            sink,
        });
        (handler, peer)
    }
}

#[async_trait]
impl MessageHandler for ClipboardHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::CLIPBOARD
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        // The peer is bound right after the handshake, before the run loop
        // dispatches anything. If somehow unbound, this is a channel error
        // rather than a panic — and critically, a clip must never be applied
        // without being able to name the device it came from. "Your clipboard
        // just changed" is only acceptable to a user alongside "because Bob
        // copied something".
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("clipboard peer not bound".into()));
        };
        match frame.message_type.get() {
            MSG_CLIP => {
                // `from_frame` is where an over-cap, empty or non-UTF-8 payload
                // is refused, so nothing past this line is peer-controlled
                // beyond its documented domain: bounded UTF-8 text.
                let clip = Clip::from_frame(&frame)?;
                (self.sink)(peer.clone(), clip);
                Ok(())
            }
            // MESSAGE_REGISTRY.md §6 — unknown type: OPTIONAL means skip and
            // keep the channel; required means fail this channel only.
            other => {
                if frame.flags.is_optional() {
                    // Ignored on purpose: a newer peer sent an additive message
                    // this build does not implement.
                    return Ok(());
                }
                Err(SessionError::FrameDecode(format!(
                    "unsupported clipboard message type {other} (required)"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType};
    use std::sync::Mutex;

    type Seen = Arc<Mutex<Vec<(DeviceId, Clip)>>>;

    fn new_handler() -> (Arc<ClipboardHandler>, Arc<OnceLock<DeviceId>>, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cl = seen.clone();
        let sink: ClipboardSink = Arc::new(move |id, clip| {
            seen_cl.lock().unwrap().push((id, clip));
        });
        let (handler, slot) = ClipboardHandler::new(sink);
        (handler, slot, seen)
    }

    fn frame_of(text: &str) -> SessionFrame {
        SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "text": text,
                    "sent_at": "2026-08-17T10:00:00Z",
                }))
                .unwrap(),
            ),
        )
    }

    #[tokio::test]
    async fn handle_rejects_a_frame_when_the_peer_is_unbound() {
        let (handler, _slot, seen) = new_handler();
        let err = handler.handle(frame_of("secret")).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
        assert!(
            seen.lock().unwrap().is_empty(),
            "no clip may be applied without naming the device it came from"
        );
    }

    #[tokio::test]
    async fn handle_surfaces_the_clip_once_bound() {
        let (handler, slot, seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        handler.handle(frame_of("hello")).await.unwrap();

        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1, "sink fired once");
        assert_eq!(got[0].0, bob);
        assert_eq!(got[0].1.text, "hello");
    }

    /// An over-cap clip surfaces nothing at all — it is neither applied nor
    /// truncated, and the failure closes the Clipboard channel only (§6).
    #[tokio::test]
    async fn an_over_cap_clip_surfaces_nothing() {
        let (handler, slot, seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-bob"));

        let big = "x".repeat(crate::MAX_CLIP + 1);
        assert!(handler.handle(frame_of(&big)).await.is_err());
        assert!(
            seen.lock().unwrap().is_empty(),
            "a refused clip must never reach the system clipboard"
        );
    }

    /// An empty clip surfaces nothing: applying it would erase the local
    /// clipboard on a peer's say-so.
    #[tokio::test]
    async fn an_empty_clip_surfaces_nothing() {
        let (handler, slot, seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-bob"));

        assert!(handler.handle(frame_of("")).await.is_err());
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Clipboard text reaches the sink **verbatim**. The receiving surface
    /// writes it to the system clipboard as plain text and renders it as text;
    /// nothing here interprets it, and a hostile peer's markup is just
    /// characters.
    #[tokio::test]
    async fn peer_text_reaches_the_sink_verbatim() {
        let (handler, slot, seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-mallory"));

        let hostile = "<script>alert(1)</script>\n../../etc/passwd\u{0}";
        handler.handle(frame_of(hostile)).await.unwrap();
        assert_eq!(seen.lock().unwrap()[0].1.text, hostile);
    }

    /// MESSAGE_REGISTRY.md §6, both halves.
    #[tokio::test]
    async fn unknown_optional_types_are_ignored_and_required_ones_fail_the_channel() {
        let (handler, slot, seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-bob"));

        let optional = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(999),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{\"whatever\":true}"),
        );
        handler
            .handle(optional)
            .await
            .expect("optional unknown is ignored, not an error");
        assert!(seen.lock().unwrap().is_empty());

        let required = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(999),
            MessageFlags::END_OF_MESSAGE, // no OPTIONAL bit
            Bytes::from_static(b"{\"whatever\":true}"),
        );
        assert!(handler.handle(required).await.is_err());
    }

    #[tokio::test]
    async fn handle_rejects_malformed_json_without_panicking() {
        let (handler, slot, _seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-bob"));
        let bad = SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"not json"),
        );
        assert!(handler.handle(bad).await.is_err());
    }

    /// The channel type is what the session's registry dispatches on; getting
    /// it wrong would route clips to nobody — or, worse, to the presence
    /// handler, whose id is one away.
    #[test]
    fn the_handler_serves_the_clipboard_channel() {
        let (handler, _slot, _seen) = new_handler();
        assert_eq!(handler.channel_type(), ChannelType::CLIPBOARD);
    }
}
