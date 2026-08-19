//! The one decision that answers: *may this machine wake that device, right
//! now?*

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// May a magic packet for `device` leave this machine?
///
/// Two legs, both of which must hold. Kept in a single pure function — the same
/// shape as `peerbeam_presence::gate::may_share_status` and
/// `peerbeam_chat::gate::may_exchange_chat` — so there is one place to read,
/// one place to test, and no way for a refactor to drop a leg quietly.
///
/// 1. **The user recorded a hardware address for this device**
///    (`mac_recorded`). Passed in rather than looked up here so the decision
///    stays pure and so [`crate::wake_device`], which has already done the
///    lookup, does not do it twice.
/// 2. **The user approved this device** — [`TrustStore::is_approved`], asked of
///    the store on every wake rather than cached, so revoking a device stops
///    the *next* wake instead of the next restart. Deliberately **not**
///    [`TrustStore::is_trusted`]: that answers "have we pinned this key", which
///    the authenticated handshake makes true for every peer that has ever
///    connected, stranger included.
///
/// # Why waking is gated at all (I6)
///
/// Sending a magic packet is an action whose effect happens on someone else's
/// hardware, so I6 — *"explicit, revocable, per-capability consent"* — is the
/// invariant in play, and it is worth being precise about what the gate does
/// and does not protect.
///
/// It does **not** protect the target. Wake-on-LAN has no authentication of any
/// kind: the packet carries no identity, no key and no signature, and any
/// device on the broadcast domain can wake any machine whose address it knows.
/// A gate in PeerBeam cannot change that and would be theatre if it claimed to.
///
/// What it protects is **this** machine, from becoming a packet source for a
/// device the user never chose. The two facts a wake needs — *which device* and
/// *what address* — are held here together, so a peer that could get an address
/// recorded against a device id would have obtained a button on the user's own
/// host that emits arbitrary frames onto their LAN. Requiring approval means
/// the only device that button can ever be pointed at is one the user
/// deliberately accepted.
///
/// Both halves of I6's requirement are met without inventing a new permission
/// bit:
///
/// * **Explicit** — nothing records an address by inference. There is no wire
///   field for it and nothing in this crate parses one; a device's address gets
///   into the store because a person put it there. (If a hardware address is
///   ever added to the handshake, this gate is *not* sufficient on its own:
///   a peer-claimed address is a claim about third-party hardware, and the
///   claim would need its own consent step.)
/// * **Revocable, per capability** — deleting the record
///   ([`WakeStore::forget`]) revokes waking and nothing else, and revoking the
///   device's approval stops it too while leaving the address on disk, exactly
///   as pinning survives an expired grant.
///
/// # Two things this gate deliberately does not consult
///
/// **[`TrustRecord::mine`]**, though "wake my devices" is the feature's whole
/// pitch. That field's own documentation forbids it: it is a label the user
/// wrote, settable by anything that can write one bool into the local trust
/// file, and *"nothing that sends, accepts, or opens anything may branch on
/// it"*. A gate that read it would turn a note into a way to grant a power.
///
/// **A negotiated capability.** Every other gate in the workspace has one (I9),
/// and this one cannot: the device is asleep, there is no session, and there is
/// nothing on the other end to negotiate with. That absence is the feature, not
/// an oversight — see the module documentation.
///
/// [`WakeStore::forget`]: crate::WakeStore::forget
/// [`TrustRecord::mine`]: peerbeam_domain::entity::TrustRecord::mine
#[must_use]
pub fn may_wake(mac_recorded: bool, trust: &dyn TrustStore, device: &DeviceId) -> bool {
    mac_recorded && trust.is_approved(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::error::{DomainError, Result};

    /// The states a real trust store distinguishes, because the gate turns on
    /// exactly one of them.
    ///
    /// `approved` — the user explicitly accepted this device.
    /// `pinned` — the device completed a handshake and had its key recorded,
    /// which `auth.rs` does for *every* never-seen peer with `approved: false`.
    /// `Broken` — a store that cannot be read at all, which must not be
    /// mistaken for permission.
    enum FakeTrust {
        Approved,
        /// Approved, but with a window the user attached to it that has since
        /// closed.
        Expired,
        PinnedOnly,
        Unknown,
        Broken,
    }

    impl TrustStore for FakeTrust {
        fn record(&self, _record: TrustRecord) -> Result<()> {
            Ok(())
        }
        fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>> {
            let approved = match self {
                FakeTrust::Approved | FakeTrust::Expired => true,
                FakeTrust::PinnedOnly => false,
                FakeTrust::Unknown => return Ok(None),
                FakeTrust::Broken => {
                    return Err(DomainError::Storage("trust store unreadable".into()))
                }
            };
            Ok(Some(TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Desktop".into(),
                trusted_at: chrono::Utc::now(),
                approved,
                permissions: PermissionSet::granted_on_approval(),
                expires_at: match self {
                    FakeTrust::Expired => Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                    _ => None,
                },
                mine: false,
            }))
        }
    }

    fn desktop() -> DeviceId {
        DeviceId::from("pb-desktop")
    }

    #[test]
    fn an_approved_device_with_a_recorded_address_may_be_woken() {
        assert!(may_wake(true, &FakeTrust::Approved, &desktop()));
    }

    /// **The gate TOFU nearly gave away.** Every peer that has ever connected
    /// is pinned — that pin is what makes a later key change detectable — so
    /// `is_trusted` answers `true` for a stranger on the café LAN. Waking asks
    /// `is_approved` instead. Replacing it with `is_trusted` must fail this.
    #[test]
    fn a_merely_pinned_device_is_refused_even_with_an_address_recorded() {
        let trust = FakeTrust::PinnedOnly;
        assert!(
            trust.is_trusted(&desktop()),
            "precondition: the handshake pinned it, so `is_trusted` is true"
        );
        assert!(!may_wake(true, &trust, &desktop()));
    }

    /// Revoking approval stops the next wake even though the address is still
    /// on disk — the store keeps the address so that re-approving does not mean
    /// re-typing it, exactly as a trust pin outlives an expired grant.
    #[test]
    fn a_device_the_user_never_approved_is_refused() {
        assert!(!may_wake(true, &FakeTrust::Unknown, &desktop()));
    }

    /// No address recorded is no wake, whatever the trust store says: there is
    /// nothing to address a packet to.
    #[test]
    fn an_approved_device_with_no_recorded_address_is_refused() {
        assert!(!may_wake(false, &FakeTrust::Approved, &desktop()));
    }

    /// **The window ends the grant.** A device approved for half an hour, an
    /// hour ago, is not wakeable — a gate that read `TrustRecord::approved`
    /// itself instead of asking [`TrustStore::is_approved`] would pass every
    /// other test here and fail this one.
    #[test]
    fn a_device_whose_approval_window_has_closed_is_refused() {
        assert!(!may_wake(true, &FakeTrust::Expired, &desktop()));
    }

    /// **Fail closed.** A trust store that cannot answer is not permission —
    /// the rule every gate in the workspace follows.
    #[test]
    fn an_unreadable_trust_store_is_not_permission() {
        assert!(!may_wake(true, &FakeTrust::Broken, &desktop()));
    }

    /// Neither leg is redundant: each one alone refuses, so a test that only
    /// ever flipped both together would not distinguish them.
    #[test]
    fn the_two_legs_are_independent() {
        assert!(!may_wake(false, &FakeTrust::Unknown, &desktop()));
        assert!(!may_wake(true, &FakeTrust::Unknown, &desktop()));
        assert!(!may_wake(false, &FakeTrust::Approved, &desktop()));
        assert!(may_wake(true, &FakeTrust::Approved, &desktop()));
    }
}
