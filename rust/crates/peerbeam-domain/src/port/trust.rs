//! Trust port: persisted device trust (TOFU).

use crate::entity::{Permission, TrustRecord};
use crate::error::Result;
use crate::id::DeviceId;

/// Stores and queries trusted-device records.
pub trait TrustStore: Send + Sync {
    /// Record (or update) trust for a device.
    fn record(&self, record: TrustRecord) -> Result<()>;

    /// Look up the trust record for a device, if any.
    fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>>;

    /// Convenience predicate: is this device currently trusted?
    ///
    /// **This means "we have pinned this device's key", not "the user chose
    /// this device".** A never-seen peer is recorded during the authenticated
    /// handshake — that pin is what makes a later key change detectable — and
    /// it is written with `approved: false`. So this answers `true` for any
    /// device that has ever completed a handshake, including a stranger on the
    /// LAN who connected once and was never accepted.
    ///
    /// Use it for MITM questions ("is this the key I saw before?"). For "may
    /// this device receive something of mine?", use [`is_approved`].
    ///
    /// [`is_approved`]: TrustStore::is_approved
    fn is_trusted(&self, device: &DeviceId) -> bool;

    /// Did the **user** deliberately grant this device standing?
    ///
    /// True only for a device the user explicitly accepted-and-trusted, which
    /// is the act that sets [`TrustRecord::approved`]. This is the predicate
    /// for any feature that sends something outward on the user's behalf
    /// without asking again — presence status, clipboard contents, an accepted
    /// pipe — because each of those is only defensible as "my own devices",
    /// and a key pinned by the handshake is not that.
    ///
    /// Fails **closed**: a store that cannot answer is not permission.
    fn is_approved(&self, device: &DeviceId) -> bool {
        matches!(self.lookup(device), Ok(Some(r)) if r.approved)
    }

    /// May this device do **this particular thing**?
    ///
    /// The one predicate for every per-device permission question in the
    /// workspace. Each feature's gate calls it as one named leg — see
    /// `peerbeam_presence::gate::may_share_status`,
    /// `peerbeam_clipboard::gate::may_share_clip`,
    /// `peerbeam_chat::gate::may_exchange_chat`,
    /// `peerbeam_transfer::pipe::gate::may_accept_pipe` and
    /// `peerbeam_ffi::transfer::may_auto_accept` — rather than each call site
    /// reaching into [`TrustRecord::permissions`] itself. One predicate means
    /// one place to read, one place to test, and no way for two features to
    /// disagree about what a grant means.
    ///
    /// # It implies approval
    ///
    /// An **unapproved device may nothing**, whatever its bits say — the rule
    /// lives in [`TrustRecord::effective_permissions`], so this predicate and
    /// every listing agree by construction rather than by both remembering to
    /// check the same flag. Permissions
    /// narrow a standing the user granted; they never create one. This is the
    /// same lesson as [`is_approved`] versus [`is_trusted`]: the TOFU handshake
    /// pins every stranger that connects, so a predicate that skipped approval
    /// would answer `true` for a peer nobody chose the moment its record
    /// happened to carry a default grant.
    ///
    /// Fails **closed**: a store that cannot answer is not permission, exactly
    /// as [`is_approved`] does.
    ///
    /// # Read per operation, never cached
    ///
    /// Callers must ask this on **every** operation, not once per session, so
    /// that revoking a permission stops the *next* clip, heartbeat, message or
    /// accept rather than the next reconnect.
    ///
    /// [`is_approved`]: TrustStore::is_approved
    /// [`is_trusted`]: TrustStore::is_trusted
    fn may(&self, device: &DeviceId, permission: Permission) -> bool {
        matches!(
            self.lookup(device),
            Ok(Some(r)) if r.effective_permissions().grants(permission)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::PermissionSet;
    use crate::error::DomainError;

    /// The three answers a real store can give, because [`TrustStore::may`]
    /// must behave differently for each: a record, no record, and a store that
    /// cannot be read at all.
    enum Store {
        Holds(TrustRecord),
        Empty,
        Broken,
    }

    impl TrustStore for Store {
        fn record(&self, _record: TrustRecord) -> Result<()> {
            Ok(())
        }
        fn lookup(&self, _device: &DeviceId) -> Result<Option<TrustRecord>> {
            match self {
                Store::Holds(r) => Ok(Some(r.clone())),
                Store::Empty => Ok(None),
                Store::Broken => Err(DomainError::Storage("trust store unreadable".into())),
            }
        }
        fn is_trusted(&self, _device: &DeviceId) -> bool {
            !matches!(self, Store::Empty)
        }
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    fn record(approved: bool, permissions: PermissionSet) -> TrustRecord {
        TrustRecord {
            device: bob(),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved,
            permissions,
        }
    }

    #[test]
    fn an_approved_device_may_what_it_was_granted() {
        let store = Store::Holds(record(true, PermissionSet::granted_on_approval()));
        for p in Permission::ALL {
            assert!(store.may(&bob(), p), "{p} was granted");
        }
    }

    /// **Permissions are separate bits, not an alias for `approved`.** Revoking
    /// one must leave the other four answering `true` — a `may` that ignored
    /// its `permission` argument would fail this.
    #[test]
    fn revoking_one_permission_leaves_the_others() {
        for target in Permission::ALL {
            let store = Store::Holds(record(
                true,
                PermissionSet::granted_on_approval().set(target, false),
            ));
            assert!(!store.may(&bob(), target), "{target} was revoked");
            for other in Permission::ALL.into_iter().filter(|p| *p != target) {
                assert!(
                    store.may(&bob(), other),
                    "revoking {target} must not revoke {other}"
                );
            }
        }
    }

    /// **`may` implies approval.** A device the handshake pinned but nobody
    /// chose carries the default grant on disk, so a predicate that read only
    /// the bits would open every gate for a stranger. Deleting `r.approved`
    /// from [`TrustStore::may`] must make this fail.
    #[test]
    fn an_unapproved_device_may_nothing_whatever_its_bits_say() {
        let store = Store::Holds(record(false, PermissionSet::granted_on_approval()));
        assert!(
            store.is_trusted(&bob()),
            "precondition: the handshake pinned it"
        );
        for p in Permission::ALL {
            assert!(!store.may(&bob(), p), "an unapproved device may not {p}");
        }
    }

    #[test]
    fn a_device_with_no_record_may_nothing() {
        for p in Permission::ALL {
            assert!(!Store::Empty.may(&bob(), p));
        }
    }

    /// **Fail closed.** A store that cannot answer is not permission — the same
    /// rule [`TrustStore::is_approved`] follows.
    #[test]
    fn a_store_error_is_not_permission() {
        assert!(!Store::Broken.is_approved(&bob()));
        for p in Permission::ALL {
            assert!(!Store::Broken.may(&bob(), p), "a store error must deny {p}");
        }
    }

    /// An approved device with an empty set is approved and may nothing: the
    /// two questions are genuinely independent in both directions.
    #[test]
    fn approval_alone_is_not_a_permission() {
        let store = Store::Holds(record(true, PermissionSet::none()));
        assert!(store.is_approved(&bob()));
        for p in Permission::ALL {
            assert!(!store.may(&bob(), p));
        }
    }
}
