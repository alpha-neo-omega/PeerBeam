//! Trust port: persisted device trust (TOFU).

use chrono::Utc;

use crate::entity::{Permission, TrustRecord};
use crate::error::Result;
use crate::id::DeviceId;

/// Stores and queries trusted-device records.
///
/// # The clock lives here
///
/// A trust record may carry a deadline ([`TrustRecord::expires_at`]), and the
/// three predicates below are the only place it is enforced. They read the
/// clock themselves rather than taking a `now`, because "is this device
/// trusted?" is a question about the present by definition, and every one of
/// the workspace's gates asks it per operation. Deferring the clock to the
/// caller would hand a dozen call sites the chance to forget it — and one that
/// forgot would keep a closed window open. The entity's predicates are the pure
/// ones ([`TrustRecord::has_expired`] and friends take an explicit `now`), so
/// the boundary can still be asserted exactly.
///
/// Note what is deliberately *not* here: nothing sweeps expired records away.
/// A sweeper is a second source of truth and the slower one — the gap between
/// sweeps is a device still trusted after its window closed.
pub trait TrustStore: Send + Sync {
    /// Record (or update) trust for a device.
    fn record(&self, record: TrustRecord) -> Result<()>;

    /// Look up the trust record for a device, if any.
    ///
    /// **Returns the record as stored, expired or not.** This is the raw read,
    /// and the pin outlives the grant on purpose: `peerbeam_transfer::auth`
    /// compares a presented fingerprint against whatever is here, so a device
    /// whose window has closed is still recognised — and a *changed* key is
    /// still caught. Answering `None` for an expired record would turn a
    /// 30-minute window into a TOFU reset.
    ///
    /// It follows that a caller must never read [`TrustRecord::approved`] or
    /// [`TrustRecord::permissions`] off this directly to decide anything. Ask
    /// [`is_approved`] or [`may`], or go through
    /// [`TrustRecord::effective_permissions_at`] with a `now` in hand — those
    /// are where expiry is applied.
    ///
    /// [`is_approved`]: TrustStore::is_approved
    /// [`may`]: TrustStore::may
    fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>>;

    /// Convenience predicate: is this device currently trusted?
    ///
    /// **This means "we hold a live pin for this device's key", not "the user
    /// chose this device".** A never-seen peer is recorded during the
    /// authenticated handshake — that pin is what makes a later key change
    /// detectable — and it is written with `approved: false`. So this answers
    /// `true` for any device that has completed a handshake and whose window
    /// (if it was given one) is still open, including a stranger on the LAN who
    /// connected once and was never accepted.
    ///
    /// For "may this device receive something of mine?", use [`is_approved`].
    /// For the MITM question — *is this the key I saw before?* — use
    /// [`lookup`] and compare fingerprints: that one must keep working after a
    /// window closes, and this predicate deliberately does not.
    ///
    /// Provided rather than required so the expiry rule has exactly one home;
    /// an implementation may override it for a cheaper read, but it then owns
    /// honouring [`TrustRecord::has_expired`] itself.
    ///
    /// [`is_approved`]: TrustStore::is_approved
    /// [`lookup`]: TrustStore::lookup
    fn is_trusted(&self, device: &DeviceId) -> bool {
        matches!(self.lookup(device), Ok(Some(r)) if !r.has_expired(Utc::now()))
    }

    /// Did the **user** deliberately grant this device standing, and does that
    /// grant still stand?
    ///
    /// True only for a device the user explicitly accepted-and-trusted, which
    /// is the act that sets [`TrustRecord::approved`] — and only while any
    /// window they attached to it is open. This is the predicate for any
    /// feature that sends something outward on the user's behalf without asking
    /// again — presence status, clipboard contents, an accepted pipe — because
    /// each of those is only defensible as "my own devices", and a key pinned
    /// by the handshake is not that.
    ///
    /// Fails **closed**: a store that cannot answer is not permission.
    fn is_approved(&self, device: &DeviceId) -> bool {
        matches!(self.lookup(device), Ok(Some(r)) if r.is_approved_at(Utc::now()))
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
    /// # It implies approval, and approval can run out
    ///
    /// An **unapproved device may nothing**, whatever its bits say, and so may
    /// one whose window has closed — both rules live in
    /// [`TrustRecord::effective_permissions_at`], so this predicate and every
    /// listing agree by construction rather than by each remembering to check
    /// the same two things. Permissions narrow a standing the user granted; they
    /// never create one, and they do not outlive one. This is the same lesson as
    /// [`is_approved`] versus [`is_trusted`]: the TOFU handshake pins every
    /// stranger that connects, so a predicate that skipped approval would answer
    /// `true` for a peer nobody chose the moment its record happened to carry a
    /// default grant.
    ///
    /// Fails **closed**: a store that cannot answer is not permission, exactly
    /// as [`is_approved`] does.
    ///
    /// # Read per operation, never cached
    ///
    /// Callers must ask this on **every** operation, not once per session, so
    /// that revoking a permission — or a window running out mid-session —
    /// stops the *next* clip, heartbeat, message or accept rather than the next
    /// reconnect. That is also what makes expiry need no sweeper: the answer is
    /// recomputed from the clock every time somebody asks.
    ///
    /// [`is_approved`]: TrustStore::is_approved
    /// [`is_trusted`]: TrustStore::is_trusted
    fn may(&self, device: &DeviceId, permission: Permission) -> bool {
        matches!(
            self.lookup(device),
            Ok(Some(r)) if r.effective_permissions_at(Utc::now()).grants(permission)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::PermissionSet;
    use crate::error::DomainError;
    use chrono::{DateTime, Duration};

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
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    fn record(approved: bool, permissions: PermissionSet) -> TrustRecord {
        expiring(approved, permissions, None)
    }

    fn expiring(
        approved: bool,
        permissions: PermissionSet,
        expires_at: Option<DateTime<Utc>>,
    ) -> TrustRecord {
        TrustRecord {
            device: bob(),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: Utc::now(),
            approved,
            permissions,
            expires_at,
        }
    }

    #[test]
    fn an_approved_device_may_what_it_was_granted() {
        let store = Store::Holds(record(true, PermissionSet::granted_on_approval()));
        for p in Permission::ALL {
            let granted = PermissionSet::granted_on_approval().grants(p);
            assert_eq!(
                store.may(&bob(), p),
                granted,
                "{p}: may() disagreed with what approval actually granted"
            );
        }
    }

    /// The upgrade rule, stated as behaviour rather than as a constant: a
    /// device approved before a permission existed must be refused that
    /// permission until someone grants it. `Notes` is slot 5, assigned after
    /// `granted_on_approval` was frozen, so it is the real case rather than a
    /// hypothetical one.
    #[test]
    fn approval_predating_a_permission_does_not_confer_it() {
        let store = Store::Holds(record(true, PermissionSet::granted_on_approval()));
        assert!(
            !store.may(&bob(), Permission::Notes),
            "a device trusted before notes existed was allowed to sync them"
        );

        let store = Store::Holds(record(
            true,
            PermissionSet::granted_on_approval().set(Permission::Notes, true),
        ));
        assert!(
            store.may(&bob(), Permission::Notes),
            "granting the permission explicitly must still work"
        );
    }

    /// **Permissions are separate bits, not an alias for `approved`.** Revoking
    /// one must leave every other *granted* permission answering `true` — a
    /// `may` that ignored its `permission` argument would fail this.
    #[test]
    fn revoking_one_permission_leaves_the_others() {
        // Start from a device holding *every* permission this build knows, not
        // just the ones approval grants — otherwise a permission added after
        // the freeze (`Notes`) is absent for its own reason and the test would
        // report it as collateral damage from the revocation under test.
        let all = Permission::ALL
            .into_iter()
            .fold(PermissionSet::none(), |set, p| set.set(p, true));
        for target in Permission::ALL {
            let store = Store::Holds(record(true, all.set(target, false)));
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
    /// from [`TrustRecord::is_approved_at`] must make this fail.
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
        assert!(!Store::Broken.is_trusted(&bob()));
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

    // ── expiry, through the predicates the gates actually ask ───────────────

    /// **Read time, not sweep time.** Nothing runs between these two stores but
    /// the deadline they carry; one is an hour past it and answers `false` to
    /// every question, with no cleanup pass anywhere in the workspace.
    #[test]
    fn an_expired_record_is_refused_by_every_predicate() {
        let now = Utc::now();
        let store = Store::Holds(expiring(
            true,
            PermissionSet::granted_on_approval(),
            Some(now - Duration::hours(1)),
        ));

        assert!(!store.is_trusted(&bob()), "the window closed");
        assert!(!store.is_approved(&bob()));
        for p in Permission::ALL {
            assert!(!store.may(&bob(), p), "an expired device may not {p}");
        }

        // ...and the record is still there, which is what keeps the pin — and
        // therefore key-change detection — alive after the grant has gone.
        assert!(store.lookup(&bob()).unwrap().is_some());
    }

    /// A window still open changes nothing: this is the control for the test
    /// above, so a `may` that simply answered `false` for any record carrying a
    /// deadline could not pass both.
    #[test]
    fn a_record_inside_its_window_is_trusted_normally() {
        let store = Store::Holds(expiring(
            true,
            PermissionSet::granted_on_approval(),
            Some(Utc::now() + Duration::hours(1)),
        ));
        assert!(store.is_trusted(&bob()));
        assert!(store.is_approved(&bob()));
        for p in PermissionSet::granted_on_approval().granted() {
            assert!(store.may(&bob(), p), "{p} must hold inside the window");
        }
    }

    /// A record with no deadline is unaffected by all of this — the ordinary
    /// case, and the one every pre-upgrade store is in.
    #[test]
    fn a_record_with_no_window_never_expires() {
        let store = Store::Holds(record(true, PermissionSet::granted_on_approval()));
        assert!(store.is_trusted(&bob()));
        assert!(store.is_approved(&bob()));
        assert!(store.may(&bob(), Permission::Files));
    }
}
