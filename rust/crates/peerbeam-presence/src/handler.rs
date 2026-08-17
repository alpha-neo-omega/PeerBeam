//! The Presence channel's MessageHandler: decode → validate → record → notify.
//!
//! Receiving is deliberately **ungated**. The opt-in setting and the
//! trusted-only rule govern what leaves this device (see [`crate::gate`]); a
//! status that arrives is displayed regardless, because the peer already made
//! its own decision to send it and the session it arrived on is authenticated
//! either way.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::{Status, MSG_STATUS};
use crate::registry::{PeerStatus, PresenceRegistry};

/// Called with each accepted status so a surface can refresh live.
pub type PresenceSink = Arc<dyn Fn(DeviceId, PeerStatus) + Send + Sync>;

/// Serves inbound Presence-channel frames for one session. The session peer is
/// bound once, after the handshake, via the returned [`OnceLock`] — the same
/// contract as `ChatHandler`.
pub struct PresenceHandler {
    registry: PresenceRegistry,
    peer: Arc<OnceLock<DeviceId>>,
    sink: PresenceSink,
}

impl PresenceHandler {
    /// Build a handler + the peer slot the caller must `set` after the
    /// handshake (before the session run loop dispatches any frame).
    #[must_use]
    pub fn new(
        registry: PresenceRegistry,
        sink: PresenceSink,
    ) -> (Arc<PresenceHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(PresenceHandler {
            registry,
            peer: peer.clone(),
            sink,
        });
        (handler, peer)
    }
}

#[async_trait]
impl MessageHandler for PresenceHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::PRESENCE
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        // The peer is bound right after the handshake, before the run loop
        // dispatches anything. If somehow unbound, this is a channel error
        // rather than a panic — and critically, a status must never be
        // attributed to an unknown device.
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("presence peer not bound".into()));
        };
        match frame.message_type.get() {
            MSG_STATUS => {
                // `from_frame` is where an out-of-range battery is refused and
                // an unknown network word is dropped, so nothing past this
                // line is peer-controlled beyond its documented domain.
                let status = Status::from_frame(&frame)?;
                let entry = PeerStatus {
                    status,
                    received_at: Utc::now(),
                };
                self.registry
                    .record(peer, entry.status.clone(), entry.received_at);
                (self.sink)(peer.clone(), entry);
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
                    "unsupported presence message type {other} (required)"
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

    fn status(battery: Option<u8>) -> Status {
        Status {
            battery_percent: battery,
            storage_free_bytes: Some(4096),
            network: Some("lan".into()),
            app_version: Some("0.4.1".into()),
            sent_at: "2026-08-17T10:00:00Z".into(),
            charging: Some(false),
        }
    }

    /// A handler plus everything a test needs to see what it did.
    #[allow(clippy::type_complexity)]
    fn new_handler() -> (
        Arc<PresenceHandler>,
        Arc<OnceLock<DeviceId>>,
        PresenceRegistry,
        Arc<Mutex<Vec<(DeviceId, PeerStatus)>>>,
    ) {
        let reg = PresenceRegistry::new();
        let seen: Arc<Mutex<Vec<(DeviceId, PeerStatus)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_cl = seen.clone();
        let sink: PresenceSink = Arc::new(move |id, st| {
            seen_cl.lock().unwrap().push((id, st));
        });
        let (handler, slot) = PresenceHandler::new(reg.clone(), sink);
        (handler, slot, reg, seen)
    }

    #[tokio::test]
    async fn handle_rejects_a_frame_when_the_peer_is_unbound() {
        let (handler, _slot, reg, _seen) = new_handler();
        let frame = status(Some(50)).to_frame(ChannelId::new(1)).unwrap();
        let err = handler.handle(frame).await.unwrap_err();
        assert!(matches!(err, SessionError::FrameDecode(_)));
        assert!(
            reg.is_empty(),
            "nothing may be attributed to an unknown peer"
        );
    }

    #[tokio::test]
    async fn handle_records_and_notifies_once_bound() {
        let (handler, slot, reg, seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        handler
            .handle(status(Some(64)).to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

        let got = reg.get(&bob).expect("recorded");
        assert_eq!(got.status.battery_percent, Some(64));
        assert_eq!(got.status.network.as_deref(), Some("lan"));
        assert_eq!(seen.lock().unwrap().len(), 1, "sink fired once");
        assert_eq!(seen.lock().unwrap()[0].0, bob);
    }

    /// A device that reports no battery is the normal case, not an error, and
    /// must be recorded as absent rather than as zero.
    #[tokio::test]
    async fn a_status_with_no_battery_is_recorded_as_absent_not_zero() {
        let (handler, slot, reg, _seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        handler
            .handle(status(None).to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();

        let got = reg.get(&bob).unwrap();
        assert_eq!(got.status.battery_percent, None);
        assert_ne!(got.status.battery_percent, Some(0), "absent is not 0%");
        assert_eq!(
            got.status.storage_free_bytes,
            Some(4096),
            "the fields it did share still arrive"
        );
    }

    /// `battery_percent: 101` is rejected and the message discarded — nothing
    /// is recorded and nothing is surfaced.
    #[tokio::test]
    async fn an_out_of_range_battery_records_nothing() {
        let (handler, slot, reg, seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        let frame = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(br#"{"battery_percent":101,"sent_at":"t"}"#),
        );
        assert!(handler.handle(frame).await.is_err());
        assert!(reg.is_empty(), "a rejected message must record nothing");
        assert!(seen.lock().unwrap().is_empty(), "and surface nothing");
    }

    /// A rejected heartbeat must not destroy the last good one. Keeping the
    /// previous reading, aged, beats replacing it with nothing.
    #[tokio::test]
    async fn a_rejected_heartbeat_leaves_the_previous_good_one_intact() {
        let (handler, slot, reg, _seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        handler
            .handle(status(Some(64)).to_frame(ChannelId::new(1)).unwrap())
            .await
            .unwrap();
        let bad = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(br#"{"battery_percent":200,"sent_at":"t"}"#),
        );
        assert!(handler.handle(bad).await.is_err());

        assert_eq!(
            reg.get(&bob).unwrap().status.battery_percent,
            Some(64),
            "the last good reading survives a bad follow-up"
        );
    }

    /// An unknown network word never reaches the registry — and so never
    /// reaches a surface — but the rest of the status still lands.
    #[tokio::test]
    async fn an_unknown_network_word_is_recorded_as_absent() {
        let (handler, slot, reg, _seen) = new_handler();
        let bob = DeviceId::from("pb-bob");
        let _ = slot.set(bob.clone());

        let frame = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(
                br#"{"network":"<b>pwned</b>","storage_free_bytes":99,"sent_at":"t"}"#,
            ),
        );
        handler.handle(frame).await.expect("still a valid message");

        let got = reg.get(&bob).unwrap();
        assert_eq!(got.status.network, None, "never rendered verbatim");
        assert_eq!(got.status.storage_free_bytes, Some(99));
    }

    /// MESSAGE_REGISTRY.md §6, both halves.
    #[tokio::test]
    async fn unknown_optional_types_are_ignored_and_required_ones_fail_the_channel() {
        let (handler, slot, reg, seen) = new_handler();
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
        assert!(reg.is_empty());
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
        let (handler, slot, _reg, _seen) = new_handler();
        let _ = slot.set(DeviceId::from("pb-bob"));
        let bad = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"not json"),
        );
        assert!(handler.handle(bad).await.is_err());
    }

    /// The channel type is what the session's registry dispatches on; getting
    /// it wrong would route presence frames to nobody.
    #[test]
    fn the_handler_serves_the_presence_channel() {
        let (handler, _slot, _reg, _seen) = new_handler();
        assert_eq!(handler.channel_type(), ChannelType::PRESENCE);
    }
}
