//! The one decision that answers: *may this peer's bytes be written to this
//! process's stdout, right now?*
//!
//! Five legs, all of which must hold, combined in a single pure function rather
//! than scattered through the accept path — the same shape as
//! `peerbeam_presence::gate::may_share_status` and
//! `peerbeam_clipboard::gate::may_share_clip`, for the same reason: one place
//! to read, one place to test, and no way for a refactor to delete a leg
//! silently.
//!
//! **The direction is the opposite of those two.** Clipboard and Presence gate
//! what *leaves*; a pipe gates what *arrives*, because what arrives is written
//! raw to a shell's stdout. Sending a pipe needs no gate at all beyond the
//! user's own `pipe --to`: the bytes are their own, chosen deliberately, at a
//! shell prompt.

use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelType, PIPE_FEAT_STREAM};

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// pipe stream feature.
///
/// Split out so the decision is testable without standing up a session, and so
/// exactly one place knows how the bit is read (mirrors
/// `peerbeam_presence::caps_support_status`).
///
/// Both ends consult this, for different questions. A sender asks it *before
/// reading a byte of stdin*, so a peer that predates `peerbeam pipe` is refused
/// up front with a reason instead of being streamed bytes it would drop. A
/// receiver asks it as one leg of [`may_accept_pipe`], so it never accepts a
/// channel its own negotiation says should not exist.
#[must_use]
pub fn caps_support_stream(caps: &CapabilitySet) -> bool {
    caps.features(ChannelType::PIPE)
        .is_some_and(|f| f & PIPE_FEAT_STREAM != 0)
}

/// May this process write `peer`'s inbound pipe to its stdout?
///
/// All five must hold:
///
/// 1. **`listening`** — this process is a `peerbeam pipe --listen`, started by
///    the user for exactly this. **There is no background acceptance and no
///    setting that grants one:** a running `receive`, `daemon` or `serve`, and
///    the Flutter GUI, all pass `false` here and refuse every pipe offered to
///    them. This is the leg that stops a long-lived daemon from becoming a
///    remote write to whatever terminal it happens to be attached to.
///
///    It is also why there is **no interactive approval prompt**, unlike a file
///    transfer: running the command *is* the approval. A prompt would read from
///    stdin — which on the sending side is the payload — and would break the
///    headless, scripted use the feature exists for. Consent is expressed once,
///    at the shell, by a person who is already there.
/// 2. **The user approved the peer** — [`TrustStore::is_approved`], asked of
///    the store itself rather than of a cached bool, so revoking a device stops
///    the next pipe rather than the next reconnect. **Not configurable**;
///    there is no setting that turns it off, exactly as for clipboard and
///    presence.
///
///    Deliberately **not** [`TrustStore::is_trusted`]. PeerBeam's handshake is
///    TOFU: a peer connecting for the first time is *pinned as it connects*, so
///    `is_trusted` is already true for a stranger on the LAN by the time this
///    is asked — the pin exists to make a later key change detectable, not to
///    record a decision. Asking `is_approved` is what makes this leg carry
///    weight against a stranger rather than only against a device the user has
///    explicitly revoked. See `docs/SECURITY.md`.
/// 3. **The user left this device its `pipe` permission** —
///    [`TrustStore::may`], the workspace's one permission predicate, asked per
///    pipe for the same reason leg 2 is: revoking must refuse the next pipe,
///    not the next reconnect. Approval and permission are separate questions
///    and both are named. `may` *implies* leg 2 (an unapproved device may
///    nothing), so this leg alone would be sufficient; it is stated anyway so
///    the gate does not lean on another predicate's internal implication —
///    the same reason legs 2 and 4 do not collapse into each other either.
/// 4. **`only_from`, when set, names this peer** — `pipe --listen --from
///    laptop` accepts that device and refuses every other, trusted or not. The
///    comparison is against the **authenticated** `DeviceId` from the
///    handshake, never against the human name the peer presented: a name is
///    peer-supplied and a peer can present any name it likes, so matching on
///    one would turn the restriction into a suggestion.
/// 5. **The peer negotiated [`PIPE_FEAT_STREAM`]** — capability-advertised, not
///    assumed (MESSAGE_REGISTRY.md §7 / I9). A channel from a peer whose
///    negotiated set lacks the bit should not exist at all; refusing it is
///    fail-closed rather than trusting the channel type alone.
///
/// Legs 1–4 are local decisions and a peer has no say in them. Leg 5 is the
/// peer's own statement about what it understands. Keeping that distinction
/// visible is why this is one function with five named legs rather than a
/// single `bool` computed somewhere upstream.
#[must_use]
pub fn may_accept_pipe(
    listening: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
    only_from: Option<&DeviceId>,
    negotiated: &CapabilitySet,
) -> bool {
    // Written as an explicit `match` rather than `Option::is_none_or` so it
    // compiles on the workspace MSRV (1.80) — and, for a security predicate,
    // reads as the two cases it actually is.
    let from_permitted = match only_from {
        Some(want) => want == peer,
        None => true,
    };
    listening
        && trust.is_approved(peer)
        && trust.may(peer, Permission::Pipe)
        && from_permitted
        && caps_support_stream(negotiated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::PermissionSet;
    use peerbeam_domain::session::Capability;

    /// `.0` = ids the user approved; `.1` = ids merely pinned by a handshake;
    /// `.2` = what an approved id was left. The first two are kept apart
    /// because that distinction is what the trust leg turns on — a store
    /// answering only "trusted / not trusted" could not tell a stranger who
    /// connected once from the user's own laptop — and the third is what the
    /// permission leg turns on.
    struct FakeTrust(Vec<String>, Vec<String>, PermissionSet);

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
            let approved = self.0.iter().any(|d| d == &device.0);
            if !approved && !self.1.iter().any(|d| d == &device.0) {
                return Ok(None);
            }
            Ok(Some(peerbeam_domain::entity::TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved,
                permissions: if approved {
                    self.2
                } else {
                    PermissionSet::none()
                },
                expires_at: None,
                mine: false,
                auto_accept: false,
            }))
        }
        fn is_trusted(&self, device: &DeviceId) -> bool {
            self.0.iter().chain(self.1.iter()).any(|d| d == &device.0)
        }
    }

    fn trusting() -> FakeTrust {
        FakeTrust(
            vec!["pb-bob".to_string(), "pb-carol".to_string()],
            Vec::new(),
            PermissionSet::granted_on_approval(),
        )
    }

    /// Approved, but with `permission` taken away — the state a `peerbeam trust
    /// revoke-permission` or the app's toggle leaves behind.
    fn trusting_without(permission: Permission) -> FakeTrust {
        let FakeTrust(approved, pinned, permissions) = trusting();
        FakeTrust(approved, pinned, permissions.set(permission, false))
    }

    /// Pinned by the handshake but never accepted by the user — what a stranger
    /// on the LAN looks like the instant after connecting.
    fn pinned_only() -> FakeTrust {
        FakeTrust(
            Vec::new(),
            vec!["pb-bob".to_string()],
            PermissionSet::granted_on_approval(),
        )
    }

    fn empty_trust() -> FakeTrust {
        FakeTrust(Vec::new(), Vec::new(), PermissionSet::granted_on_approval())
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    fn carol() -> DeviceId {
        DeviceId::from("pb-carol")
    }

    /// A peer that negotiated the pipe capability properly.
    fn negotiated() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::PIPE,
            PIPE_FEAT_STREAM,
        ))
    }

    /// **The gate that TOFU nearly gave away.** A stranger is pinned by the
    /// handshake itself, so `is_trusted` is true for them before anyone has
    /// decided anything. Were this leg asking that, a listening terminal would
    /// accept a stranger's bytes on its stdout the first time they connected.
    #[test]
    fn a_merely_pinned_peer_is_refused_even_while_listening() {
        let trust = pinned_only();
        assert!(
            trust.is_trusted(&bob()),
            "precondition: the handshake pinned him, so `is_trusted` is true"
        );
        assert!(
            !may_accept_pipe(true, &trust, &bob(), None, &negotiated()),
            "a pinned-but-unapproved peer must not reach our stdout"
        );
    }

    #[test]
    fn all_five_gates_open_permits_an_accept() {
        assert!(may_accept_pipe(
            true,
            &trusting(),
            &bob(),
            None,
            &negotiated()
        ));
    }

    /// **The listen gate.** A process that is not `pipe --listen` refuses even a
    /// trusted, fully capable peer — deleting `listening` from
    /// [`may_accept_pipe`] must make this fail.
    #[test]
    fn a_process_that_is_not_listening_refuses_a_trusted_capable_peer() {
        assert!(
            !may_accept_pipe(false, &trusting(), &bob(), None, &negotiated()),
            "only a `pipe --listen` may take an inbound pipe"
        );
    }

    /// **The trust gate.** A revoked peer is refused even by a listener —
    /// deleting `trust.is_trusted(peer)` must make this fail.
    #[test]
    fn an_untrusted_peer_is_refused_even_while_listening() {
        assert!(
            !may_accept_pipe(true, &empty_trust(), &bob(), None, &negotiated()),
            "an untrusted peer must never reach stdout"
        );
    }

    /// **The `--from` gate.** Another *trusted* peer is refused when the
    /// listener named a device — the interesting case, since an untrusted one
    /// was already refused by leg 2.
    #[test]
    fn from_refuses_a_different_trusted_peer() {
        assert!(
            may_accept_pipe(true, &trusting(), &bob(), Some(&bob()), &negotiated()),
            "the named device is accepted"
        );
        assert!(
            !may_accept_pipe(true, &trusting(), &carol(), Some(&bob()), &negotiated()),
            "carol is trusted and still refused: --from named bob"
        );
    }

    /// Every leg refuses on its own, so a test that only ever flipped several
    /// at once would not distinguish them.
    #[test]
    fn the_gates_are_independent() {
        assert!(!may_accept_pipe(
            false,
            &trusting(),
            &bob(),
            None,
            &negotiated()
        ));
        assert!(!may_accept_pipe(
            true,
            &empty_trust(),
            &bob(),
            None,
            &negotiated()
        ));
        assert!(!may_accept_pipe(
            true,
            &trusting(),
            &bob(),
            Some(&carol()),
            &negotiated()
        ));
        assert!(!may_accept_pipe(
            true,
            &trusting(),
            &bob(),
            None,
            &CapabilitySet::new()
        ));
        assert!(may_accept_pipe(
            true,
            &trusting(),
            &bob(),
            None,
            &negotiated()
        ));
    }

    /// A peer that predates `peerbeam pipe` advertises no PIPE capability at
    /// all, so the intersection drops it and no pipe is exchanged in either
    /// direction.
    #[test]
    fn a_peer_that_does_not_advertise_pipe_is_refused() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(ChannelType::TRANSFER))
            .with(Capability::new(ChannelType::CHAT));
        let n = negotiated().intersect(&legacy);
        assert!(!n.supports(ChannelType::PIPE));
        assert!(!caps_support_stream(&n));
        assert!(!may_accept_pipe(true, &trusting(), &bob(), None, &n));
    }

    /// A peer advertising the channel with `features: 0` has the bit ANDed away
    /// and is likewise refused.
    #[test]
    fn a_peer_advertising_pipe_without_the_feature_bit_is_refused() {
        let bare = CapabilitySet::new().with(Capability::new(ChannelType::PIPE));
        let n = negotiated().intersect(&bare);
        assert!(n.supports(ChannelType::PIPE), "the channel negotiates");
        assert_eq!(n.features(ChannelType::PIPE), Some(0), "the bit is ANDed");
        assert!(!caps_support_stream(&n));
        assert!(!may_accept_pipe(true, &trusting(), &bob(), None, &n));
    }

    /// Unknown future bits from a newer peer must not be mistaken for this one.
    #[test]
    fn an_unrelated_future_feature_bit_does_not_imply_stream() {
        let future =
            CapabilitySet::new().with(Capability::with_features(ChannelType::PIPE, 1 << 6));
        let n = negotiated().intersect(&future);
        assert!(!caps_support_stream(&n));
    }

    /// The gate is asked about *this* peer, not about "some trusted peer".
    /// Without the `peer` argument being used, a device trusting anyone at all
    /// would take a pipe from everyone.
    #[test]
    fn trust_is_evaluated_for_the_peer_being_accepted() {
        let trust = FakeTrust(
            vec!["pb-bob".to_string()],
            Vec::new(),
            PermissionSet::granted_on_approval(),
        );
        assert!(may_accept_pipe(true, &trust, &bob(), None, &negotiated()));
        assert!(
            !may_accept_pipe(
                true,
                &trust,
                &DeviceId::from("pb-mallory"),
                None,
                &negotiated()
            ),
            "trusting one device must not open the gate for another"
        );
    }

    /// `--from` is matched against the authenticated device id. A peer that
    /// merely *calls itself* the named device is a different id and is refused;
    /// this pins that the comparison is on ids, since a name-based one would
    /// make the restriction spoofable by anyone on the network.
    #[test]
    fn from_compares_authenticated_ids_not_presented_names() {
        let impostor = DeviceId::from("pb-mallory");
        let trust = FakeTrust(
            vec!["pb-mallory".to_string()],
            Vec::new(),
            PermissionSet::granted_on_approval(),
        );
        assert!(
            !may_accept_pipe(true, &trust, &impostor, Some(&bob()), &negotiated()),
            "only the id `--from` resolved to may pass"
        );
    }

    /// **The permission gate.** A listener refuses an approved, fully capable,
    /// `--from`-matching peer whose `pipe` permission the user took away —
    /// deleting `trust.may(peer, Permission::Pipe)` from [`may_accept_pipe`]
    /// must make this fail.
    #[test]
    fn revoking_the_pipe_permission_refuses_an_otherwise_open_gate() {
        assert!(
            !may_accept_pipe(
                true,
                &trusting_without(Permission::Pipe),
                &bob(),
                None,
                &negotiated()
            ),
            "a device whose pipe permission was taken away must not reach stdout"
        );
    }

    /// **The permissions are separate bits, not an alias for `approved`.**
    /// Revoking any *other* permission leaves this gate wide open.
    #[test]
    fn revoking_a_different_permission_leaves_pipe_working() {
        for other in Permission::ALL
            .into_iter()
            .filter(|p| *p != Permission::Pipe)
        {
            assert!(
                may_accept_pipe(true, &trusting_without(other), &bob(), None, &negotiated()),
                "revoking {other} must not refuse a pipe"
            );
        }
    }
}
