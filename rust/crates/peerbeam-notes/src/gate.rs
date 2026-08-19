//! The one decision that answers: *may notes be exchanged with this peer?*
//!
//! One place to read, one place to test, and no way for a refactor to delete a
//! leg silently — the same shape as `peerbeam_chat::gate` and
//! `peerbeam_presence::gate`.

use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// May we exchange notes with `peer`?
///
/// One leg, deliberately: [`TrustStore::may`] with [`Permission::Notes`], which
/// implies the device is **approved** and has kept that permission.
///
/// **Unlike chat, there is no "prior policy" leg.** Chat needs one because it
/// worked before permissions existed, so gating it on `may` alone would revoke
/// it from every device nobody explicitly approved. Notes have no such history:
/// the permission and the feature arrived together, so `may` is the whole rule
/// and a device acquires it only by being granted it.
///
/// That is also what makes "your own devices" mean something concrete. PeerBeam
/// has no notion of *owning* a device, only of trusting one, so this permission
/// **is** that notion — a device receives your notes because you said it may,
/// not because anything inferred that it is yours.
///
/// Asked per exchange rather than per session, so revoking stops the next sync
/// rather than waiting for a reconnect.
#[must_use]
pub fn may_sync_notes(trust: &dyn TrustStore, peer: &DeviceId) -> bool {
    trust.may(peer, Permission::Notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};

    struct Store(Option<TrustRecord>);

    impl TrustStore for Store {
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

    fn record(approved: bool, permissions: PermissionSet) -> TrustRecord {
        TrustRecord {
            device: bob(),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved,
            permissions,
            expires_at: None,
        }
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    #[test]
    fn a_device_granted_notes_may_sync() {
        let store = Store(Some(record(
            true,
            PermissionSet::granted_on_approval().set(Permission::Notes, true),
        )));
        assert!(may_sync_notes(&store, &bob()));
    }

    /// The upgrade rule where it bites: approval alone is not enough, because
    /// `Notes` was assigned after `granted_on_approval` was frozen. A device
    /// trusted before notes existed must not start receiving them.
    #[test]
    fn approval_alone_does_not_permit_notes() {
        let store = Store(Some(record(true, PermissionSet::granted_on_approval())));
        assert!(!may_sync_notes(&store, &bob()));
    }

    #[test]
    fn a_merely_pinned_device_may_not_sync() {
        // Pinned is not approved. A handshake makes a key change detectable; it
        // is not a decision to share anything.
        let store = Store(Some(record(
            false,
            PermissionSet::granted_on_approval().set(Permission::Notes, true),
        )));
        assert!(!may_sync_notes(&store, &bob()));
    }

    #[test]
    fn an_unknown_device_may_not_sync() {
        assert!(!may_sync_notes(&Store(None), &bob()));
    }
}
