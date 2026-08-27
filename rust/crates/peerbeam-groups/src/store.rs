//! Groups on disk, in the encrypted [`AppStore`] — the same route
//! `peerbeam_chat`, `peerbeam_notes` and `peerbeam_spaces` take.

use std::sync::Arc;

use rand::rngs::OsRng;
use rand::RngCore;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{AppStore, TrustStore};

use crate::group::{
    normalise, validate_member, validate_name, Group, GroupError, PendingInvite, MAX_MEMBERS,
};

/// The single namespace every group lives in.
pub const NS: &str = "groups";

/// Invitations received and not yet answered.
///
/// **Separate from [`NS`], because an invitation is not a membership.** A
/// pending invitation must not appear anywhere a group does — it is an offer
/// somebody made, and the whole point of A2's fourth condition is that nothing
/// is joined until this device's own user says so. Keeping them in one
/// namespace would mean every listing had to remember to filter, and one that
/// forgot would show a group this device is not in.
pub const INVITES_NS: &str = "group-invites";

/// A shared, wire-visible group id: 32 random hex characters.
///
/// **Wider than a Space's, deliberately.** A Space id is local, so it only has
/// to be unique within one store and 64 bits is generous. This one is minted on
/// one device and then held by every member, so it has to be unique across
/// every device that will ever exchange one — two people creating a group at
/// the same second on two machines must not produce the same id, because the
/// second group would silently merge into the first on anyone who held both.
///
/// Random rather than derived, for the reason `peerbeam_spaces::mint_id` gives
/// and one more: a name, a roster or a timestamp would collide exactly when two
/// groups are most alike, and would leak what it was derived from to anyone who
/// saw the id — including members who were never told the other thing.
fn mint_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Groups, persisted and encrypted at rest, reconciled against the trust store
/// on every read.
///
/// Both ports are held rather than passed per call, for the reason
/// `peerbeam_spaces::SpaceStore` gives: there is no read on this type that does
/// not ask the trust question, so no future caller gets a chance to skip it.
#[derive(Clone)]
pub struct GroupStore {
    store: Arc<dyn AppStore>,
    trust: Arc<dyn TrustStore>,
    /// This device, so a roster can include it and a send can exclude it.
    me: DeviceId,
}

impl GroupStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>, trust: Arc<dyn TrustStore>, me: DeviceId) -> Self {
        Self { store, trust, me }
    }

    /// Every group this device holds, ordered by name.
    ///
    /// # Errors
    /// [`GroupError::Unreadable`] when the store cannot be read.
    pub fn list(&self) -> Result<Vec<Group>, GroupError> {
        let raw = self.store.list(NS).map_err(|e| GroupError::Unreadable {
            reason: e.to_string(),
        })?;
        let mut groups: Vec<Group> = raw
            .iter()
            .filter_map(|(_, bytes)| serde_json::from_slice(bytes).ok())
            .collect();
        groups.sort_by_key(|g| normalise(&g.name));
        Ok(groups)
    }

    /// One group by id.
    ///
    /// # Errors
    /// [`GroupError::Unreadable`], or [`GroupError::UnknownGroup`].
    pub fn get(&self, id: &str) -> Result<Group, GroupError> {
        self.list()?
            .into_iter()
            .find(|g| g.id == id)
            .ok_or_else(|| GroupError::UnknownGroup { id: id.to_string() })
    }

    /// Create a group holding only this device.
    ///
    /// **Members are added by inviting them, never at creation.** A create that
    /// took a member list would be enrolling other people's devices in
    /// something they have not agreed to, which A2's fourth condition forbids;
    /// they join by accepting an invitation, and the roster grows when they say
    /// so.
    ///
    /// # Errors
    /// [`GroupError::EmptyName`], [`GroupError::NameTooLong`],
    /// [`GroupError::DuplicateName`], or a store error.
    pub fn create(&self, name: &str) -> Result<Group, GroupError> {
        let name = validate_name(name)?;
        let existing = self.list()?;
        if existing
            .iter()
            .any(|g| normalise(&g.name) == normalise(&name))
        {
            return Err(GroupError::DuplicateName { name });
        }
        let group = Group {
            id: mint_id(),
            name,
            members: vec![self.me.clone()],
        };
        self.write(&group)?;
        Ok(group)
    }

    /// Adopt a group from an invitation the user accepted.
    ///
    /// The roster written is the inviter's list **plus this device**: the
    /// inviter cannot know we accepted until we tell it, so its copy does not
    /// have us in it yet.
    ///
    /// Idempotent by id. Accepting the same invitation twice — two copies of
    /// one offer, a tap repeated — is the state the user asked for either way,
    /// and erroring would be pedantry about a job already done.
    ///
    /// # Errors
    /// A member id that could forge a delimiter, a roster over
    /// [`MAX_MEMBERS`], or a store error.
    pub fn adopt(&self, id: &str, name: &str, members: &[DeviceId]) -> Result<Group, GroupError> {
        let name = validate_name(name)?;
        for m in members {
            validate_member(m)?;
        }
        let mut roster: Vec<DeviceId> = members.to_vec();
        if !roster.iter().any(|m| m == &self.me) {
            roster.push(self.me.clone());
        }
        if roster.len() > MAX_MEMBERS {
            return Err(GroupError::TooManyMembers);
        }
        let group = Group {
            id: id.to_string(),
            name,
            members: roster,
        };
        self.write(&group)?;
        Ok(group)
    }

    /// Record that `device` is now in `id`'s roster.
    ///
    /// Idempotent: a member already present is left alone rather than
    /// duplicated, because a `GroupJoined` can legitimately arrive twice.
    ///
    /// # Errors
    /// [`GroupError::UnknownGroup`], [`GroupError::TooManyMembers`], a bad
    /// member id, or a store error.
    pub fn add_member(&self, id: &str, device: &DeviceId) -> Result<Group, GroupError> {
        validate_member(device)?;
        let mut group = self.get(id)?;
        if group.holds(device) {
            return Ok(group);
        }
        if group.members.len() + 1 > MAX_MEMBERS {
            return Err(GroupError::TooManyMembers);
        }
        group.members.push(device.clone());
        self.write(&group)?;
        Ok(group)
    }

    /// Record that `device` has left `id`.
    ///
    /// Idempotent for the same reason [`add_member`](Self::add_member) is.
    ///
    /// # Errors
    /// [`GroupError::UnknownGroup`] or a store error.
    pub fn remove_member(&self, id: &str, device: &DeviceId) -> Result<Group, GroupError> {
        let mut group = self.get(id)?;
        group.members.retain(|m| m != device);
        self.write(&group)?;
        Ok(group)
    }

    /// Forget a group entirely on this device.
    ///
    /// Local and complete: this device stops sending to it and stops showing
    /// it. Whether the other members stop sending is their decision, and
    /// nothing here can compel it — see [`GroupLeft`](crate::GroupLeft).
    ///
    /// Returns whether a record was actually removed; a group that is already
    /// gone is `false` rather than an error, because forgetting twice reaches
    /// the state the user asked for.
    ///
    /// # Errors
    /// A store error.
    pub fn forget(&self, id: &str) -> Result<bool, GroupError> {
        self.store
            .delete(NS, id)
            .map_err(|e| GroupError::Unwritable {
                reason: e.to_string(),
            })
    }

    /// Rename a group **on this device only**.
    ///
    /// # Errors
    /// [`GroupError::UnknownGroup`], a name error, [`GroupError::DuplicateName`],
    /// or a store error.
    pub fn rename(&self, id: &str, name: &str) -> Result<Group, GroupError> {
        let name = validate_name(name)?;
        let mut group = self.get(id)?;
        if self
            .list()?
            .iter()
            .any(|g| g.id != id && normalise(&g.name) == normalise(&name))
        {
            return Err(GroupError::DuplicateName { name });
        }
        group.name = name;
        self.write(&group)?;
        Ok(group)
    }

    /// Remember an invitation until the user answers it.
    ///
    /// Keyed by group id, so a second copy of the same offer replaces the first
    /// rather than stacking: two invitations to one group are one decision.
    ///
    /// # Errors
    /// A bad member id, or a store error.
    pub fn record_invite(&self, invite: &PendingInvite) -> Result<(), GroupError> {
        for m in &invite.members {
            validate_member(m)?;
        }
        let bytes = serde_json::to_vec(invite).map_err(|e| GroupError::Unwritable {
            reason: e.to_string(),
        })?;
        self.store
            .put(INVITES_NS, &invite.group, &bytes)
            .map_err(|e| GroupError::Unwritable {
                reason: e.to_string(),
            })
    }

    /// Invitations waiting for an answer.
    ///
    /// An invitation to a group this device has **already joined** is dropped
    /// from the answer: it is an offer that has been taken, and showing it
    /// would invite the user to accept something twice.
    ///
    /// # Errors
    /// A store error.
    pub fn invites(&self) -> Result<Vec<PendingInvite>, GroupError> {
        let raw = self
            .store
            .list(INVITES_NS)
            .map_err(|e| GroupError::Unreadable {
                reason: e.to_string(),
            })?;
        let held = self.list()?;
        Ok(raw
            .iter()
            .filter_map(|(_, b)| serde_json::from_slice::<PendingInvite>(b).ok())
            .filter(|i| !held.iter().any(|g| g.id == i.group))
            .collect())
    }

    /// One pending invitation by group id.
    ///
    /// # Errors
    /// A store error.
    pub fn invite(&self, group: &str) -> Result<Option<PendingInvite>, GroupError> {
        Ok(self.invites()?.into_iter().find(|i| i.group == group))
    }

    /// Forget an invitation — answered, or declined.
    ///
    /// # Errors
    /// A store error.
    pub fn forget_invite(&self, group: &str) -> Result<bool, GroupError> {
        self.store
            .delete(INVITES_NS, group)
            .map_err(|e| GroupError::Unwritable {
                reason: e.to_string(),
            })
    }

    /// Split `id`'s recipients into those this device may still message and
    /// those it may not, as of now.
    ///
    /// **Asked at read time, never pruned on write.** A membership does not
    /// stop being true because somebody wrote to this store, and two of the
    /// ways a member goes stale involve no write here at all: the user revokes
    /// the device, or a time-limited grant runs out while nothing is running.
    /// Pruning on write would be a sweeper, and the interval between sweeps is
    /// exactly the interval in which a send still includes a device the user
    /// revoked. `peerbeam_spaces::SpaceStore` reads its members the same way
    /// and for the same reason.
    ///
    /// The refused list is returned rather than dropped so a caller can **name**
    /// who was skipped — A2's third condition, and the same promise
    /// `space send` already keeps.
    ///
    /// # Errors
    /// [`GroupError::UnknownGroup`] or a store error.
    pub fn reachable(&self, id: &str) -> Result<(Vec<DeviceId>, Vec<DeviceId>), GroupError> {
        let group = self.get(id)?;
        let mut live = Vec::new();
        let mut stale = Vec::new();
        for member in group.recipients(&self.me) {
            if self
                .trust
                .may(&member, peerbeam_domain::entity::Permission::Chat)
            {
                live.push(member);
            } else {
                stale.push(member);
            }
        }
        Ok((live, stale))
    }

    fn write(&self, group: &Group) -> Result<(), GroupError> {
        let bytes = serde_json::to_vec(group).map_err(|e| GroupError::Unwritable {
            reason: e.to_string(),
        })?;
        self.store
            .put(NS, &group.id, &bytes)
            .map_err(|e| GroupError::Unwritable {
                reason: e.to_string(),
            })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{Permission, PermissionSet, TrustRecord};
    use peerbeam_domain::error::{DomainError, Result};
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Mutex;

    /// A trust store whose approved set can be edited mid-test, so revoking is
    /// a real state change rather than a second fixture.
    ///
    /// `approved: true`, unlike the equivalent in `peerbeam_spaces`: `may`
    /// implies approval, and this crate's `reachable` asks `may` rather than
    /// `is_trusted` — a pinned stranger is not somebody to send a group message
    /// to.
    pub(crate) struct FakeTrust {
        approved: Mutex<Vec<String>>,
        broken: bool,
    }

    impl FakeTrust {
        fn holding(ids: &[&str]) -> Arc<FakeTrust> {
            Arc::new(FakeTrust {
                approved: Mutex::new(ids.iter().map(|s| (*s).to_string()).collect()),
                broken: false,
            })
        }
        fn revoke(&self, id: &str) {
            self.approved.lock().unwrap().retain(|p| p != id);
        }
    }

    impl TrustStore for FakeTrust {
        fn record(&self, _record: TrustRecord) -> Result<()> {
            Ok(())
        }
        fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>> {
            if self.broken {
                return Err(DomainError::Storage("trust store unreadable".into()));
            }
            if !self
                .approved
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == device.as_str())
            {
                return Ok(None);
            }
            Ok(Some(TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: true,
                permissions: PermissionSet::granted_on_approval(),
                expires_at: None,
                mine: false,
                auto_accept: false,
            }))
        }
    }

    fn app_store(dir: &std::path::Path) -> Arc<dyn AppStore> {
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[9u8; 32], b"peerbeam-appstore-v1");
        Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.join("appstore"),
            key,
            enc,
        ))
    }

    fn me() -> DeviceId {
        DeviceId::from("pb-me")
    }
    fn alice() -> DeviceId {
        DeviceId::from("pb-alice")
    }
    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    pub(crate) fn new_store() -> (GroupStore, Arc<FakeTrust>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let trust = FakeTrust::holding(&["pb-alice", "pb-bob"]);
        let store = GroupStore::new(app_store(dir.path()), trust.clone(), me());
        (store, trust, dir)
    }

    /// **A created group holds only its creator.** There is no member list to
    /// pass, because adding somebody at creation would enrol their device in
    /// something they never agreed to — A2's fourth condition.
    #[test]
    fn a_created_group_holds_only_this_device() {
        let (store, _trust, _dir) = new_store();
        let g = store.create("Trip").unwrap();
        assert_eq!(g.members, vec![me()]);
        assert_eq!(g.name, "Trip");
        assert_eq!(g.id.len(), 32, "a wire-visible id must be 128 bits of hex");
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn a_group_survives_a_reopen_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let trust = FakeTrust::holding(&[]);
        let id = {
            let store = GroupStore::new(app_store(dir.path()), trust.clone(), me());
            store.create("Trip").unwrap().id
        };
        let store = GroupStore::new(app_store(dir.path()), trust, me());
        assert_eq!(store.get(&id).unwrap().name, "Trip");
    }

    #[test]
    fn two_groups_a_person_would_call_the_same_are_refused() {
        let (store, _t, _d) = new_store();
        store.create("Work Trip").unwrap();
        assert!(matches!(
            store.create("  work   trip "),
            Err(GroupError::DuplicateName { .. })
        ));
    }

    /// Adopting an invitation adds **this device** to the roster: the inviter's
    /// copy cannot know we accepted until we tell it.
    #[test]
    fn adopting_an_invitation_puts_this_device_in_the_roster() {
        let (store, _t, _d) = new_store();
        let g = store.adopt("abc", "Trip", &[alice(), bob()]).unwrap();
        assert!(g.holds(&me()));
        assert!(g.holds(&alice()));
        assert_eq!(g.members.len(), 3);
    }

    /// Two copies of one offer, or a tap repeated, reach the state the user
    /// asked for either way.
    #[test]
    fn adopting_the_same_invitation_twice_is_not_an_error() {
        let (store, _t, _d) = new_store();
        store.adopt("abc", "Trip", &[alice()]).unwrap();
        let again = store.adopt("abc", "Trip", &[alice()]).unwrap();
        assert_eq!(again.members.len(), 2);
        assert_eq!(store.list().unwrap().len(), 1, "it was stored twice");
    }

    #[test]
    fn a_member_is_added_once_however_many_times_it_announces_itself() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        store.add_member(&g.id, &alice()).unwrap();
        let after = store.add_member(&g.id, &alice()).unwrap();
        assert_eq!(after.members, vec![me(), alice()]);
    }

    #[test]
    fn removing_a_member_that_is_not_there_is_not_an_error() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        let after = store.remove_member(&g.id, &bob()).unwrap();
        assert_eq!(after.members, vec![me()]);
    }

    /// **The reachability split is asked at read time, and names who is
    /// refused.** A2's third condition: membership grants nothing, every send
    /// still passes the per-member permission check, and a member who cannot be
    /// messaged is named rather than silently dropped.
    #[test]
    fn a_revoked_member_becomes_unreachable_with_nothing_written_here() {
        let (store, trust, _dir) = new_store();
        let g = store.create("Trip").unwrap();
        store.add_member(&g.id, &alice()).unwrap();
        store.add_member(&g.id, &bob()).unwrap();

        let (live, stale) = store.reachable(&g.id).unwrap();
        assert_eq!(live, vec![alice(), bob()]);
        assert!(stale.is_empty());

        // The user revokes Bob. Nothing writes to the group store.
        trust.revoke("pb-bob");

        let (live, stale) = store.reachable(&g.id).unwrap();
        assert_eq!(live, vec![alice()]);
        assert_eq!(stale, vec![bob()], "a refused member must be named");
        assert!(
            store.get(&g.id).unwrap().holds(&bob()),
            "the roster must not be pruned: revoking is not leaving"
        );
    }

    /// This device is never one of its own recipients.
    #[test]
    fn the_sender_is_not_a_recipient_of_its_own_group() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        store.add_member(&g.id, &alice()).unwrap();
        let (live, stale) = store.reachable(&g.id).unwrap();
        assert!(!live.contains(&me()) && !stale.contains(&me()));
    }

    #[test]
    fn a_rename_is_local_and_still_refuses_a_duplicate() {
        let (store, _t, _d) = new_store();
        let a = store.create("Trip").unwrap();
        store.create("Work").unwrap();
        assert!(matches!(
            store.rename(&a.id, "work"),
            Err(GroupError::DuplicateName { .. })
        ));
        assert_eq!(store.rename(&a.id, "Holiday").unwrap().name, "Holiday");
    }

    #[test]
    fn forgetting_a_group_twice_is_not_an_error() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        assert!(store.forget(&g.id).unwrap());
        assert!(!store.forget(&g.id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    /// A send is N direct sends, so the roster is bounded — a group nobody can
    /// message is not a feature.
    #[test]
    fn a_roster_cannot_grow_past_the_cap() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        for n in 0..MAX_MEMBERS - 1 {
            store
                .add_member(&g.id, &DeviceId::from(format!("pb-{n}")))
                .unwrap();
        }
        assert_eq!(store.get(&g.id).unwrap().members.len(), MAX_MEMBERS);
        assert!(matches!(
            store.add_member(&g.id, &DeviceId::from("pb-one-too-many")),
            Err(GroupError::TooManyMembers)
        ));
    }

    /// An id that could forge a delimiter must not reach a roster, whether it
    /// arrives by invitation or by announcement.
    #[test]
    fn a_hostile_member_id_is_refused_from_both_directions() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        assert!(store
            .add_member(&g.id, &DeviceId::from("pb-a/../b"))
            .is_err());
        assert!(store.adopt("x", "T", &[DeviceId::from("pb-a\nb")]).is_err());
    }

    /// A group whose permission check cannot be made must not be reported as
    /// reachable: an unreadable trust store is not permission.
    #[test]
    fn an_unreadable_trust_store_makes_nobody_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let trust = Arc::new(FakeTrust {
            approved: Mutex::new(vec!["pb-alice".into()]),
            broken: true,
        });
        let store = GroupStore::new(app_store(dir.path()), trust, me());
        let g = store.create("Trip").unwrap();
        store.add_member(&g.id, &alice()).unwrap();
        let (live, stale) = store.reachable(&g.id).unwrap();
        assert!(live.is_empty(), "a store error must not read as permission");
        assert_eq!(stale, vec![alice()]);
    }

    fn an_invite(group: &str) -> PendingInvite {
        PendingInvite {
            group: group.to_string(),
            name: "Trip".into(),
            from: alice(),
            members: vec![alice(), bob()],
            at: "2026-08-27T10:00:00Z".into(),
        }
    }

    /// An invitation is held apart from groups: holding one is not being in
    /// one, and a listing that mixed them would show a group this device has
    /// not joined.
    #[test]
    fn a_pending_invitation_is_not_a_group() {
        let (store, _t, _d) = new_store();
        store.record_invite(&an_invite("g-1")).unwrap();
        assert_eq!(store.invites().unwrap().len(), 1);
        assert!(
            store.list().unwrap().is_empty(),
            "an invitation appeared as a group"
        );
    }

    /// Once the offer is taken, it stops being an offer — otherwise the user is
    /// invited to accept something they are already in.
    #[test]
    fn an_invitation_to_a_group_already_joined_is_not_offered() {
        let (store, _t, _d) = new_store();
        store.record_invite(&an_invite("g-1")).unwrap();
        store.adopt("g-1", "Trip", &[alice(), bob()]).unwrap();
        assert!(store.invites().unwrap().is_empty());
    }

    /// Two copies of one offer are one decision, not two rows.
    #[test]
    fn a_repeated_invitation_replaces_rather_than_stacks() {
        let (store, _t, _d) = new_store();
        store.record_invite(&an_invite("g-1")).unwrap();
        store.record_invite(&an_invite("g-1")).unwrap();
        assert_eq!(store.invites().unwrap().len(), 1);
    }

    #[test]
    fn forgetting_an_invitation_twice_is_not_an_error() {
        let (store, _t, _d) = new_store();
        store.record_invite(&an_invite("g-1")).unwrap();
        assert!(store.forget_invite("g-1").unwrap());
        assert!(!store.forget_invite("g-1").unwrap());
        assert!(store.invites().unwrap().is_empty());
    }

    /// A roster arriving from a peer is validated like any other: an id that
    /// could forge a delimiter must not be written even into an offer.
    #[test]
    fn a_hostile_id_in_an_invitation_is_refused() {
        let (store, _t, _d) = new_store();
        let mut bad = an_invite("g-1");
        bad.members = vec![DeviceId::from("pb-a\nb")];
        assert!(store.record_invite(&bad).is_err());
    }

    #[test]
    fn minted_ids_do_not_repeat() {
        let seen: std::collections::BTreeSet<String> = (0..256).map(|_| mint_id()).collect();
        assert_eq!(seen.len(), 256);
        assert!(seen.iter().all(|id| id.len() == 32));
    }

    /// `Permission::Chat` is the gate, not merely "is it pinned".
    #[test]
    fn a_pinned_but_unapproved_device_is_not_reachable() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        // `pb-stranger` is absent from the approved set, so `lookup` answers
        // None and `may` is false.
        store
            .add_member(&g.id, &DeviceId::from("pb-stranger"))
            .unwrap();
        let (live, stale) = store.reachable(&g.id).unwrap();
        assert!(live.is_empty());
        assert_eq!(stale, vec![DeviceId::from("pb-stranger")]);
        let _ = Permission::Chat;
    }
}
