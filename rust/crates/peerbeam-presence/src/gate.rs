//! The one decision that answers: *may this device's status leave for this
//! peer, right now?*
//!
//! Three independent gates, all of which must hold. They are combined in a
//! single pure function rather than scattered through the send path so that
//! there is exactly one place to read, one place to test, and no way for a
//! refactor to delete a leg silently — the same shape as
//! `peerbeam_ffi::transfer::should_send_decline`, for the same reason.
//!
//! The gates are deliberately *not* symmetric with the receive path. Receiving
//! and displaying a peer's status is unconditional; only sending is gated. A
//! device with sharing off is a full participant in everyone else's dashboard
//! and contributes nothing to it, which is exactly what an opt-in means.

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelType, PRESENCE_FEAT_STATUS};

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// presence `Status` feature.
///
/// Split out so the decision is testable without standing up a session, and so
/// exactly one place knows how the bit is read (mirrors
/// `session_exec::caps_support_file_ref`).
#[must_use]
pub fn caps_support_status(caps: &CapabilitySet) -> bool {
    caps.features(ChannelType::PRESENCE)
        .is_some_and(|f| f & PRESENCE_FEAT_STATUS != 0)
}

/// May we put a `Status` on the wire toward `peer`?
///
/// All three must hold:
///
/// 1. **`sharing_enabled`** — the user turned on *"Share device status with
///    trusted devices"*. It defaults to **off** (I11, secure defaults), and
///    while it is off this device sends no status at all, to anyone. Read fresh
///    on every heartbeat rather than captured once, so turning the setting off
///    stops an already-running session's heartbeats at the next tick instead of
///    at the next reconnect.
/// 2. **The user approved the peer** — [`TrustStore::is_approved`], asked of
///    the store itself rather than a cached bool, so revoking stops the next
///    heartbeat and not the next session. Deliberately **not**
///    [`TrustStore::is_trusted`]: that answers "have we pinned this key",
///    which the authenticated handshake makes true for *every* peer that has
///    ever connected — including a stranger on the LAN, recorded with
///    `approved: false` purely so a later key change is detectable. Battery
///    level, free disk and network kind are a device fingerprint, and they go
///    to devices the user actually chose or nowhere. This gate is **not
///    configurable**.
/// 3. **The peer negotiated [`PRESENCE_FEAT_STATUS`]** — capability-advertised,
///    not assumed (MESSAGE_REGISTRY.md §7 / I9). A 1b/2a-era peer never
///    advertises the PRESENCE channel, `CapabilitySet::intersect` drops it, and
///    it is sent nothing; it shows as "status not shared", never as an error.
///
/// Gates 1 and 2 are local privacy decisions and a peer has no say in them.
/// Gate 3 is the peer's own statement about what it understands. Keeping that
/// distinction visible is why this is one function with three named legs rather
/// than a single `bool` computed somewhere upstream.
#[must_use]
pub fn may_share_status(
    sharing_enabled: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
    negotiated: &CapabilitySet,
) -> bool {
    sharing_enabled && trust.is_approved(peer) && caps_support_status(negotiated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::Capability;

    /// A trust store that distinguishes the two states a real one has, because
    /// the gate turns on exactly that distinction.
    ///
    /// `approved` — the user explicitly accepted-and-trusted this device.
    /// `pinned` — the device completed a handshake and had its key recorded,
    /// which `auth.rs` does for *every* never-seen peer with `approved: false`.
    /// A store that answered only "trusted / not trusted" could not tell a
    /// stranger on the LAN from the user's own laptop.
    struct FakeTrust {
        approved: Vec<String>,
        pinned: Vec<String>,
    }

    impl TrustStore for FakeTrust {
        fn record(
            &self,
            _record: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            device: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            let approved = self.approved.iter().any(|d| d == &device.0);
            if !approved && !self.pinned.iter().any(|d| d == &device.0) {
                return Ok(None);
            }
            Ok(Some(peerbeam_domain::entity::TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved,
            }))
        }
        fn is_trusted(&self, device: &DeviceId) -> bool {
            self.approved
                .iter()
                .chain(self.pinned.iter())
                .any(|d| d == &device.0)
        }
    }

    fn trusting() -> FakeTrust {
        FakeTrust {
            approved: vec!["pb-bob".to_string()],
            pinned: Vec::new(),
        }
    }

    /// Bob's key is pinned — he has connected before — but the user never
    /// accepted him. This is what a stranger on the LAN looks like the instant
    /// after the handshake.
    fn pinned_only() -> FakeTrust {
        FakeTrust {
            approved: Vec::new(),
            pinned: vec!["pb-bob".to_string()],
        }
    }

    fn empty_trust() -> FakeTrust {
        FakeTrust {
            approved: Vec::new(),
            pinned: Vec::new(),
        }
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    /// A peer that negotiated presence properly.
    fn negotiated() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::PRESENCE,
            PRESENCE_FEAT_STATUS,
        ))
    }

    #[test]
    fn all_three_gates_open_permits_a_send() {
        assert!(may_share_status(true, &trusting(), &bob(), &negotiated()));
    }

    /// **The gate that TOFU nearly gave away.** A device is pinned by the
    /// authenticated handshake itself — that pin is what makes a later key
    /// change detectable — and `is_trusted` answers `true` for it. So a
    /// stranger who connected once passes "trusted" without the user ever
    /// having chosen them, which is not a basis for shipping them this
    /// device's battery level and free disk. The gate asks `is_approved`.
    #[test]
    fn a_merely_pinned_peer_is_refused_even_with_sharing_on() {
        let trust = pinned_only();
        assert!(
            trust.is_trusted(&bob()),
            "precondition: the handshake pinned him, so `is_trusted` is true"
        );
        assert!(
            !may_share_status(true, &trust, &bob(), &negotiated()),
            "a pinned-but-unapproved peer must not receive our status"
        );
    }

    /// **The trust gate.** A peer with no record at all is never sent a status
    /// *even with the setting on* — deleting `trust.is_approved(peer)` from
    /// [`may_share_status`] must make this fail.
    #[test]
    fn an_untrusted_peer_is_refused_even_with_sharing_on() {
        assert!(
            !may_share_status(true, &empty_trust(), &bob(), &negotiated()),
            "status must never leave for an untrusted peer"
        );
    }

    /// **The opt-in gate.** With the setting off nothing is sent, even to a
    /// peer that is trusted and fully capable — deleting `sharing_enabled`
    /// from [`may_share_status`] must make this fail.
    #[test]
    fn sharing_off_refuses_even_a_trusted_capable_peer() {
        assert!(
            !may_share_status(false, &trusting(), &bob(), &negotiated()),
            "the opt-in default is off and off must mean silent"
        );
    }

    /// Neither gate is redundant: each one alone refuses, so a test that only
    /// ever flipped both at once would not distinguish them.
    #[test]
    fn the_two_privacy_gates_are_independent() {
        assert!(!may_share_status(
            false,
            &empty_trust(),
            &bob(),
            &negotiated()
        ));
        assert!(!may_share_status(
            true,
            &empty_trust(),
            &bob(),
            &negotiated()
        ));
        assert!(!may_share_status(false, &trusting(), &bob(), &negotiated()));
        assert!(may_share_status(true, &trusting(), &bob(), &negotiated()));
    }

    /// A 1b/2a-era peer advertises no PRESENCE capability at all, so the
    /// intersection drops it and it is never sent a Status.
    #[test]
    fn a_peer_that_does_not_advertise_presence_is_never_sent_a_status() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(ChannelType::TRANSFER))
            .with(Capability::new(ChannelType::CHAT));
        let negotiated = negotiated().intersect(&legacy);
        assert!(!negotiated.supports(ChannelType::PRESENCE));
        assert!(!caps_support_status(&negotiated));
        assert!(!may_share_status(true, &trusting(), &bob(), &negotiated));
    }

    /// A peer that advertises the channel but with `features: 0` — the shape a
    /// future build with a stripped-down presence implementation would have —
    /// has the bit ANDed away and is likewise sent nothing.
    #[test]
    fn a_peer_advertising_presence_with_no_features_is_sent_nothing() {
        let bare = CapabilitySet::new().with(Capability::new(ChannelType::PRESENCE));
        let n = negotiated().intersect(&bare);
        assert!(n.supports(ChannelType::PRESENCE), "the channel negotiates");
        assert_eq!(
            n.features(ChannelType::PRESENCE),
            Some(0),
            "the bit is ANDed away"
        );
        assert!(!may_share_status(true, &trusting(), &bob(), &n));
    }

    /// Unknown future bits from a newer peer must not be mistaken for this one.
    #[test]
    fn an_unrelated_future_feature_bit_does_not_imply_status() {
        let future =
            CapabilitySet::new().with(Capability::with_features(ChannelType::PRESENCE, 1 << 5));
        let n = negotiated().intersect(&future);
        assert!(!caps_support_status(&n));
    }

    /// The gate is asked about *this* peer, not about "some trusted peer".
    /// Without the `peer` argument being used, a device trusting anyone at all
    /// would leak its status to everyone.
    #[test]
    fn trust_is_evaluated_for_the_peer_being_sent_to() {
        let trust = trusting(); // trusts pb-bob only
        assert!(may_share_status(true, &trust, &bob(), &negotiated()));
        assert!(
            !may_share_status(true, &trust, &DeviceId::from("pb-mallory"), &negotiated()),
            "trusting one device must not open the gate for another"
        );
    }
}
