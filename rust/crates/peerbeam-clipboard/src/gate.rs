//! The one decision that answers: *may this device's clipboard leave for this
//! peer, right now?*
//!
//! Three independent gates, all of which must hold. They are combined in a
//! single pure function rather than scattered through the send path so that
//! there is exactly one place to read, one place to test, and no way for a
//! refactor to delete a leg silently — the same shape as
//! `peerbeam_presence::gate::may_share_status`, for the same reason.
//!
//! The gates are deliberately *not* symmetric with the receive path. Receiving
//! and applying a peer's clip is unconditional; only sending is gated. A device
//! with sync off still accepts what its peers send, which is exactly what an
//! opt-in means: the setting governs what leaves this machine, not what reaches
//! it.

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelType, CLIPBOARD_FEAT_CLIP};

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// clipboard `Clip` feature.
///
/// Split out so the decision is testable without standing up a session, and so
/// exactly one place knows how the bit is read (mirrors
/// `peerbeam_presence::caps_support_status`).
#[must_use]
pub fn caps_support_clip(caps: &CapabilitySet) -> bool {
    caps.features(ChannelType::CLIPBOARD)
        .is_some_and(|f| f & CLIPBOARD_FEAT_CLIP != 0)
}

/// May we put a `Clip` on the wire toward `peer`?
///
/// All three must hold:
///
/// 1. **`sync_enabled`** — the user turned on *"Sync clipboard with trusted
///    devices"*. It defaults to **off** (I11, secure defaults), and while it is
///    off this device sends no clip at all, to anyone. Read fresh on every push
///    rather than captured once, so turning the setting off stops the next clip
///    instead of the next reconnect.
/// 2. **The peer is trusted** — asked of the [`TrustStore`] itself, not of a
///    cached bool, for the same reason: revoking trust must stop the next clip,
///    not the next session. This gate is **not configurable**; there is no
///    setting that turns it off. The clipboard is the single most sensitive
///    buffer on a desktop — it holds whatever the user last copied, and (see
///    the crate docs) nothing can tell which of those were passwords — so it
///    goes to the user's own pinned devices or nowhere.
/// 3. **The peer negotiated [`CLIPBOARD_FEAT_CLIP`]** — capability-advertised,
///    not assumed (MESSAGE_REGISTRY.md §7 / I9). A peer from before this
///    feature never advertises the CLIPBOARD channel, `CapabilitySet::intersect`
///    drops it, and it is sent nothing; it simply does not take part in sync,
///    which is not an error.
///
/// Gates 1 and 2 are local privacy decisions and a peer has no say in them.
/// Gate 3 is the peer's own statement about what it understands. Keeping that
/// distinction visible is why this is one function with three named legs rather
/// than a single `bool` computed somewhere upstream.
#[must_use]
pub fn may_share_clip(
    sync_enabled: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
    negotiated: &CapabilitySet,
) -> bool {
    sync_enabled && trust.is_trusted(peer) && caps_support_clip(negotiated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::Capability;

    /// A trust store that trusts exactly the ids it was built with. Small
    /// enough to be obviously correct — the point of these tests is the gate,
    /// not the store.
    struct FakeTrust(Vec<String>);

    impl TrustStore for FakeTrust {
        fn record(
            &self,
            _record: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            _device: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            Ok(None)
        }
        fn is_trusted(&self, device: &DeviceId) -> bool {
            self.0.iter().any(|d| d == &device.0)
        }
    }

    fn trusting() -> FakeTrust {
        FakeTrust(vec!["pb-bob".to_string()])
    }

    fn empty_trust() -> FakeTrust {
        FakeTrust(Vec::new())
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    /// A peer that negotiated clipboard properly.
    fn negotiated() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::CLIPBOARD,
            CLIPBOARD_FEAT_CLIP,
        ))
    }

    #[test]
    fn all_three_gates_open_permits_a_send() {
        assert!(may_share_clip(true, &trusting(), &bob(), &negotiated()));
    }

    /// **The trust gate.** An untrusted peer is never sent a clip *even with
    /// the setting on* — deleting `trust.is_trusted(peer)` from
    /// [`may_share_clip`] must make this fail.
    #[test]
    fn an_untrusted_peer_is_refused_even_with_sync_on() {
        assert!(
            !may_share_clip(true, &empty_trust(), &bob(), &negotiated()),
            "a clipboard must never leave for an untrusted peer"
        );
    }

    /// **The opt-in gate.** With the setting off nothing is sent, even to a
    /// peer that is trusted and fully capable — deleting `sync_enabled` from
    /// [`may_share_clip`] must make this fail.
    #[test]
    fn sync_off_refuses_even_a_trusted_capable_peer() {
        assert!(
            !may_share_clip(false, &trusting(), &bob(), &negotiated()),
            "the opt-in default is off and off must mean silent"
        );
    }

    /// Neither gate is redundant: each one alone refuses, so a test that only
    /// ever flipped both at once would not distinguish them.
    #[test]
    fn the_two_privacy_gates_are_independent() {
        assert!(!may_share_clip(
            false,
            &empty_trust(),
            &bob(),
            &negotiated()
        ));
        assert!(!may_share_clip(true, &empty_trust(), &bob(), &negotiated()));
        assert!(!may_share_clip(false, &trusting(), &bob(), &negotiated()));
        assert!(may_share_clip(true, &trusting(), &bob(), &negotiated()));
    }

    /// A peer from before clipboard sync advertises no CLIPBOARD capability at
    /// all, so the intersection drops it and it is never sent a Clip.
    #[test]
    fn a_peer_that_does_not_advertise_clipboard_is_never_sent_a_clip() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(ChannelType::TRANSFER))
            .with(Capability::new(ChannelType::CHAT));
        let negotiated = negotiated().intersect(&legacy);
        assert!(!negotiated.supports(ChannelType::CLIPBOARD));
        assert!(!caps_support_clip(&negotiated));
        assert!(!may_share_clip(true, &trusting(), &bob(), &negotiated));
    }

    /// A peer that advertises the channel but with `features: 0` — the shape a
    /// future build with a stripped-down clipboard implementation would have —
    /// has the bit ANDed away and is likewise sent nothing.
    #[test]
    fn a_peer_advertising_clipboard_with_no_features_is_sent_nothing() {
        let bare = CapabilitySet::new().with(Capability::new(ChannelType::CLIPBOARD));
        let n = negotiated().intersect(&bare);
        assert!(n.supports(ChannelType::CLIPBOARD), "the channel negotiates");
        assert_eq!(
            n.features(ChannelType::CLIPBOARD),
            Some(0),
            "the bit is ANDed away"
        );
        assert!(!may_share_clip(true, &trusting(), &bob(), &n));
    }

    /// Unknown future bits from a newer peer must not be mistaken for this one.
    #[test]
    fn an_unrelated_future_feature_bit_does_not_imply_clip() {
        let future =
            CapabilitySet::new().with(Capability::with_features(ChannelType::CLIPBOARD, 1 << 5));
        let n = negotiated().intersect(&future);
        assert!(!caps_support_clip(&n));
    }

    /// The bit is read off the CLIPBOARD capability specifically. All three
    /// first-party feature bits assigned so far are `1 << 0` in their own
    /// namespaces, so a gate that read the wrong channel's features would look
    /// perfectly correct until a peer advertised one channel and not the other.
    #[test]
    fn the_bit_is_read_off_the_clipboard_capability_not_another_channels() {
        let chat_only = CapabilitySet::new().with(Capability::with_features(
            ChannelType::CHAT,
            peerbeam_domain::session::CHAT_FEAT_FILEREF,
        ));
        assert!(
            !caps_support_clip(&chat_only),
            "CHAT's bit 0 must never be read as clipboard support"
        );
        let presence_only = CapabilitySet::new().with(Capability::with_features(
            ChannelType::PRESENCE,
            peerbeam_domain::session::PRESENCE_FEAT_STATUS,
        ));
        assert!(
            !caps_support_clip(&presence_only),
            "PRESENCE's bit 0 must never be read as clipboard support"
        );
    }

    /// The gate is asked about *this* peer, not about "some trusted peer".
    /// Without the `peer` argument being used, a device trusting anyone at all
    /// would leak its clipboard to everyone.
    #[test]
    fn trust_is_evaluated_for_the_peer_being_sent_to() {
        let trust = trusting(); // trusts pb-bob only
        assert!(may_share_clip(true, &trust, &bob(), &negotiated()));
        assert!(
            !may_share_clip(true, &trust, &DeviceId::from("pb-mallory"), &negotiated()),
            "trusting one device must not open the gate for another"
        );
    }
}
