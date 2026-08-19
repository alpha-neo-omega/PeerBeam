//! Spaces on disk, in the encrypted [`AppStore`] — the same route
//! `peerbeam_chat`'s store and `peerbeam_notes`' store take.

use std::sync::Arc;

use rand::rngs::OsRng;
use rand::RngCore;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{AppStore, TrustStore};

use crate::space::{normalise, validate_member, validate_name, Space, SpaceError, SpaceView};

/// The single namespace every space lives in.
///
/// One namespace, not one per space: spaces are a handful of small records that
/// are almost always read together (to list them, and to refuse a duplicate
/// name), and [`AppStore::namespaces`] would otherwise be doing the work
/// [`AppStore::list`] already does in one call.
pub const NS: &str = "spaces";

/// Spaces, persisted and encrypted at rest by the [`AppStore`] beneath, with
/// every read reconciled against the [`TrustStore`].
///
/// # Both ports are held, not passed per call
///
/// A caller cannot obtain a member list without the trust question having been
/// asked, because there is no read on this type that does not ask it — the same
/// "one place to read, one place to test" shape the capability gates use. A
/// `TrustStore` supplied per call would give every future surface its own
/// chance to pass the wrong thing, or nothing.
#[derive(Clone)]
pub struct SpaceStore {
    store: Arc<dyn AppStore>,
    trust: Arc<dyn TrustStore>,
}

impl SpaceStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>, trust: Arc<dyn TrustStore>) -> Self {
        SpaceStore { store, trust }
    }

    /// Create a space with `name` and no members.
    ///
    /// Refuses an empty name, one longer than [`MAX_NAME`], one holding a
    /// character that cannot be rendered honestly, and a name another space
    /// already answers to.
    ///
    /// [`MAX_NAME`]: crate::MAX_NAME
    ///
    /// # The duplicate check is not atomic, and says so
    ///
    /// The scan and the write are two operations against a store with no
    /// compare-and-swap, so two processes creating the same name in the same
    /// instant can both pass the scan — the same window
    /// `peerbeam_trust_fs::FsTrust::persist` documents for its own merge.
    /// [`by_name`](Self::by_name) resolves the result deterministically (lowest
    /// id wins) so both processes then agree on which space that name means,
    /// and the user can delete the other. The alternative — keying records by
    /// name so the store enforces uniqueness — would make a rename a
    /// write-then-delete pair, and a crash between the two would either
    /// duplicate a space or lose one outright.
    pub fn create(&self, name: &str) -> Result<SpaceView, SpaceError> {
        let name = validate_name(name)?;
        self.refuse_name_clash(&name, None)?;
        let space = Space {
            id: mint_id(),
            name,
            members: Vec::new(),
        };
        self.write(&space)?;
        Ok(space.view(self.trust.as_ref()))
    }

    /// Rename a space, keeping its id and its members.
    ///
    /// Renaming to the name it already has — or to a different casing of it —
    /// is allowed: the clash check skips the space being renamed, so
    /// "work" → "Work" is a rename and not a collision with itself.
    pub fn rename(&self, id: &str, name: &str) -> Result<SpaceView, SpaceError> {
        let name = validate_name(name)?;
        let mut space = self.record(id)?;
        self.refuse_name_clash(&name, Some(id))?;
        space.name = name;
        self.write(&space)?;
        Ok(space.view(self.trust.as_ref()))
    }

    /// Delete a space. Returns whether one existed.
    ///
    /// Idempotent — deleting a space that is already gone is the outcome the
    /// caller asked for, not a mistake worth reporting. The mutations are not:
    /// renaming or adding to something that does not exist means the caller has
    /// the wrong id, and [`SpaceError::NotFound`] tells them so.
    ///
    /// **Nothing else is touched.** A space is a label; deleting it deletes the
    /// label. No trust record, conversation or transfer history knows the space
    /// existed, because none of them ever did.
    pub fn delete(&self, id: &str) -> Result<bool, SpaceError> {
        self.store
            .delete(NS, id)
            .map_err(|e| SpaceError::Storage(e.to_string()))
    }

    /// Add a member. Returns whether the space changed — `false` when the
    /// device was already in it.
    ///
    /// Three refusals, in the order that gives the most useful message:
    ///
    /// 1. an id that could not be a device id at all
    ///    ([`SpaceError::BadMember`]);
    /// 2. an id naming a device this machine does not trust
    ///    ([`SpaceError::UnknownMember`]) — a typo, or a device that was
    ///    revoked;
    /// 3. a trust store that cannot be read ([`SpaceError::TrustUnreadable`]),
    ///    which is refused rather than guessed either way.
    ///
    /// The trust check runs **before** the already-a-member check on purpose.
    /// Re-adding a member whose trust has since been revoked then reports the
    /// real problem — the device is gone, and the space will not reach it —
    /// instead of a cheerful "nothing to do" that hides it.
    ///
    /// This check is a **message, not a gate**: what keeps a fan-out honest is
    /// [`Space::view`], which re-asks on every read. A device trusted today and
    /// revoked tomorrow was added legitimately and must still stop receiving,
    /// and no write-time check can do that.
    pub fn add_member(&self, id: &str, member: &DeviceId) -> Result<bool, SpaceError> {
        validate_member(member)?;
        let mut space = self.record(id)?;
        match self.trust.lookup(member) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(SpaceError::UnknownMember {
                    id: member.0.clone(),
                })
            }
            Err(e) => {
                return Err(SpaceError::TrustUnreadable {
                    id: member.0.clone(),
                    reason: e.to_string(),
                })
            }
        }
        if space.members.contains(member) {
            return Ok(false);
        }
        space.members.push(member.clone());
        self.write(&space)?;
        Ok(true)
    }

    /// Remove a member. Returns whether the space changed.
    ///
    /// **Validates nothing.** A member that has been revoked, or whose id this
    /// build would no longer accept, is exactly the one a user most needs to
    /// take out; putting the `add_member` checks on this path would make such a
    /// member unremovable.
    pub fn remove_member(&self, id: &str, member: &DeviceId) -> Result<bool, SpaceError> {
        let mut space = self.record(id)?;
        let before = space.members.len();
        space.members.retain(|m| m != member);
        if space.members.len() == before {
            return Ok(false);
        }
        self.write(&space)?;
        Ok(true)
    }

    /// One space by id, or `None` if there is no such space.
    ///
    /// A record that exists but cannot be decoded is an **error**, not `None`.
    /// The caller is asking about *this* space; telling them it does not exist
    /// while its record sits on disk would have them create a second one beside
    /// it. [`list`](Self::list) makes the opposite trade, for its own reason.
    pub fn get(&self, id: &str) -> Result<Option<SpaceView>, SpaceError> {
        let Some(bytes) = self
            .store
            .get(NS, id)
            .map_err(|e| SpaceError::Storage(e.to_string()))?
        else {
            return Ok(None);
        };
        let space: Space =
            serde_json::from_slice(&bytes).map_err(|e| SpaceError::Serialization(e.to_string()))?;
        Ok(Some(space.view(self.trust.as_ref())))
    }

    /// One space by name, compared the way [`normalise`] compares names.
    ///
    /// The lookup `peerbeam space send work …` performs, which is why two
    /// spaces may not share a name. Should the create-race above ever produce
    /// two anyway, the lowest id wins — [`list`](Self::list) is sorted, so
    /// every process resolves the ambiguity the same way rather than each
    /// picking whichever record its filesystem happened to hand back first.
    pub fn by_name(&self, name: &str) -> Result<Option<SpaceView>, SpaceError> {
        let wanted = normalise(name);
        Ok(self
            .list()?
            .into_iter()
            .find(|s| normalise(&s.name) == wanted))
    }

    /// Every space, ordered by name (ignoring case) and then by id.
    ///
    /// Sorted here rather than left in storage order because the store's own
    /// order is by id — minted at random, so it means nothing to a person — and
    /// because two processes listing the same spaces must produce the same
    /// order for [`by_name`](Self::by_name) to resolve a clash identically.
    ///
    /// A record this build cannot decode is skipped with a warning, never
    /// fatal: the containment `ChatStore::history` and `NoteStore::all` both
    /// apply, since one unreadable row must not make every other space
    /// unreachable.
    pub fn list(&self) -> Result<Vec<SpaceView>, SpaceError> {
        let rows = self
            .store
            .list(NS)
            .map_err(|e| SpaceError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for (key, value) in rows {
            match serde_json::from_slice::<Space>(&value) {
                Ok(space) => out.push(space.view(self.trust.as_ref())),
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "skipping unreadable space record");
                }
            }
        }
        out.sort_by(|a, b| {
            normalise(&a.name)
                .cmp(&normalise(&b.name))
                .then(a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// The stored record for `id`, or [`SpaceError::NotFound`].
    fn record(&self, id: &str) -> Result<Space, SpaceError> {
        let Some(bytes) = self
            .store
            .get(NS, id)
            .map_err(|e| SpaceError::Storage(e.to_string()))?
        else {
            return Err(SpaceError::NotFound(id.to_string()));
        };
        serde_json::from_slice(&bytes).map_err(|e| SpaceError::Serialization(e.to_string()))
    }

    /// Refuse `name` if another space — any but `keep`, the one being renamed —
    /// already answers to it.
    fn refuse_name_clash(&self, name: &str, keep: Option<&str>) -> Result<(), SpaceError> {
        let wanted = normalise(name);
        for existing in self.list()? {
            if Some(existing.id.as_str()) == keep {
                continue;
            }
            if normalise(&existing.name) == wanted {
                return Err(SpaceError::NameTaken {
                    wanted: name.to_string(),
                    existing: existing.name,
                });
            }
        }
        Ok(())
    }

    fn write(&self, space: &Space) -> Result<(), SpaceError> {
        let bytes =
            serde_json::to_vec(space).map_err(|e| SpaceError::Serialization(e.to_string()))?;
        self.store
            .put(NS, &space.id, &bytes)
            .map_err(|e| SpaceError::Storage(e.to_string()))
    }
}

/// An opaque local id: 16 random hex characters.
///
/// Deliberately **not** the time-ordered id `peerbeam_chat::mint_id` and
/// `peerbeam_notes::mint_id` mint. Those exist so a message or a note sorts by
/// when it was written; spaces are listed by name, so a timestamp in the id
/// would order nothing and would put the moment a private label was created
/// into a string that appears in logs and CLI output. Random is also what makes
/// the create-above race lose nothing: a counter derived from the existing
/// records would hand two concurrent creates the same id, and the second write
/// would overwrite the first.
fn mint_id() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::MAX_NAME;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::error::{DomainError, Result};
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Mutex;

    /// A trust store whose pinned set can be edited mid-test, so revoking is a
    /// real state change rather than a second fixture.
    struct FakeTrust {
        pinned: Mutex<Vec<String>>,
        broken: bool,
    }

    impl FakeTrust {
        fn holding(ids: &[&str]) -> Arc<FakeTrust> {
            Arc::new(FakeTrust {
                pinned: Mutex::new(ids.iter().map(|s| (*s).to_string()).collect()),
                broken: false,
            })
        }
        fn revoke(&self, id: &str) {
            self.pinned.lock().unwrap().retain(|p| p != id);
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
                .pinned
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
                approved: false,
                permissions: PermissionSet::granted_on_approval(),
                expires_at: None,
                mine: false,
            }))
        }
    }

    fn app_store(dir: &std::path::Path) -> Arc<dyn AppStore> {
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.join("appstore"),
            key,
            enc,
        ))
    }

    /// A store over a real encrypted `FsAppStore`, with `alice` and `bob`
    /// trusted.
    fn new_store() -> (SpaceStore, Arc<FakeTrust>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let trust = FakeTrust::holding(&["pb-alice", "pb-bob"]);
        let store = SpaceStore::new(app_store(dir.path()), trust.clone());
        (store, trust, dir)
    }

    fn alice() -> DeviceId {
        DeviceId::from("pb-alice")
    }
    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    // ── create / rename / delete ────────────────────────────────────────────

    #[test]
    fn a_created_space_is_empty_and_listed_under_its_name() {
        let (spaces, _trust, _dir) = new_store();
        let made = spaces.create("  Work  ").unwrap();
        assert_eq!(made.name, "Work", "the stored name is trimmed");
        assert!(made.live.is_empty() && made.stale.is_empty());

        let listed = spaces.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, made.id);
    }

    #[test]
    fn a_space_survives_a_reopen_of_the_store() {
        let (spaces, trust, dir) = new_store();
        let made = spaces.create("Work").unwrap();
        spaces.add_member(&made.id, &alice()).unwrap();
        drop(spaces);

        let reopened = SpaceStore::new(app_store(dir.path()), trust);
        let view = reopened.get(&made.id).unwrap().expect("still there");
        assert_eq!(view.name, "Work");
        assert_eq!(view.live, vec![alice()]);
    }

    #[test]
    fn renaming_keeps_the_id_and_the_members() {
        let (spaces, _trust, _dir) = new_store();
        let made = spaces.create("Work").unwrap();
        spaces.add_member(&made.id, &alice()).unwrap();

        let renamed = spaces.rename(&made.id, "Office").unwrap();
        assert_eq!(renamed.id, made.id, "a rename must not mint a new space");
        assert_eq!(renamed.name, "Office");
        assert_eq!(renamed.live, vec![alice()]);
        assert!(spaces.by_name("work").unwrap().is_none());
        assert_eq!(spaces.by_name("office").unwrap().unwrap().id, made.id);
    }

    /// Changing only the casing is a rename, not a collision with itself — the
    /// clash check must skip the space being renamed.
    #[test]
    fn a_space_can_be_renamed_to_another_casing_of_its_own_name() {
        let (spaces, _trust, _dir) = new_store();
        let made = spaces.create("work").unwrap();
        assert_eq!(spaces.rename(&made.id, "Work").unwrap().name, "Work");
    }

    #[test]
    fn deleting_removes_the_space_and_repeating_it_is_not_an_error() {
        let (spaces, _trust, _dir) = new_store();
        let made = spaces.create("Work").unwrap();

        assert!(spaces.delete(&made.id).unwrap());
        assert!(spaces.get(&made.id).unwrap().is_none());
        assert!(spaces.list().unwrap().is_empty());
        assert!(
            !spaces.delete(&made.id).unwrap(),
            "a second delete reported a change"
        );
    }

    #[test]
    fn mutating_a_space_that_does_not_exist_says_so() {
        let (spaces, _trust, _dir) = new_store();
        assert!(matches!(
            spaces.rename("nope", "Work"),
            Err(SpaceError::NotFound(_))
        ));
        assert!(matches!(
            spaces.add_member("nope", &alice()),
            Err(SpaceError::NotFound(_))
        ));
        assert!(matches!(
            spaces.remove_member("nope", &alice()),
            Err(SpaceError::NotFound(_))
        ));
    }

    #[test]
    fn spaces_are_listed_by_name_ignoring_case() {
        let (spaces, _trust, _dir) = new_store();
        for name in ["zebra", "Alpha", "mid"] {
            spaces.create(name).unwrap();
        }
        let names: Vec<String> = spaces.list().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Alpha", "mid", "zebra"]);
    }

    // ── membership ──────────────────────────────────────────────────────────

    #[test]
    fn members_are_added_once_and_removed_once() {
        let (spaces, _trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;

        assert!(spaces.add_member(&id, &alice()).unwrap());
        assert!(
            !spaces.add_member(&id, &alice()).unwrap(),
            "adding a member twice reported a change"
        );
        assert!(spaces.add_member(&id, &bob()).unwrap());
        assert_eq!(spaces.get(&id).unwrap().unwrap().live, vec![alice(), bob()]);

        assert!(spaces.remove_member(&id, &alice()).unwrap());
        assert!(
            !spaces.remove_member(&id, &alice()).unwrap(),
            "removing a member that is not there reported a change"
        );
        assert_eq!(spaces.get(&id).unwrap().unwrap().live, vec![bob()]);
    }

    #[test]
    fn two_spaces_can_hold_the_same_member_independently() {
        // A space is a label, and a device may wear several. Removing it from
        // one must not touch the other.
        let (spaces, _trust, _dir) = new_store();
        let work = spaces.create("Work").unwrap().id;
        let home = spaces.create("Home").unwrap().id;
        spaces.add_member(&work, &alice()).unwrap();
        spaces.add_member(&home, &alice()).unwrap();

        spaces.remove_member(&work, &alice()).unwrap();
        assert!(spaces.get(&work).unwrap().unwrap().live.is_empty());
        assert_eq!(spaces.get(&home).unwrap().unwrap().live, vec![alice()]);
    }

    // ── the revoked member ──────────────────────────────────────────────────

    /// **The whole point of read-time enforcement.** Nothing writes to the
    /// space between the two reads; only trust changes.
    #[test]
    fn revoking_a_member_takes_it_out_of_the_fan_out_at_the_very_next_read() {
        let (spaces, trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;
        spaces.add_member(&id, &alice()).unwrap();
        spaces.add_member(&id, &bob()).unwrap();
        assert_eq!(spaces.get(&id).unwrap().unwrap().live.len(), 2);

        trust.revoke("pb-bob");

        let view = spaces.get(&id).unwrap().unwrap();
        assert_eq!(
            view.live,
            vec![alice()],
            "a revoked device was still sent to"
        );
        assert_eq!(
            view.stale,
            vec![bob()],
            "and it must be reported, not silently dropped"
        );
        assert_eq!(
            spaces.list().unwrap()[0].stale,
            vec![bob()],
            "every read agrees, not just the one by id"
        );
    }

    /// Trust comes back — a re-paired device, a renewed window — and the
    /// membership must come back with it. A prune on write could not do this.
    #[test]
    fn re_trusting_a_revoked_member_restores_it_without_the_user_re_adding_it() {
        let (spaces, trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;
        spaces.add_member(&id, &bob()).unwrap();

        trust.revoke("pb-bob");
        assert_eq!(spaces.get(&id).unwrap().unwrap().stale, vec![bob()]);

        trust.pinned.lock().unwrap().push("pb-bob".to_string());
        let view = spaces.get(&id).unwrap().unwrap();
        assert_eq!(view.live, vec![bob()]);
        assert!(view.stale.is_empty());
    }

    /// A stale member is the one a user most needs to take out, so removal must
    /// not run the checks that adding does.
    #[test]
    fn a_revoked_member_can_still_be_removed() {
        let (spaces, trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;
        spaces.add_member(&id, &bob()).unwrap();
        trust.revoke("pb-bob");

        assert!(spaces.remove_member(&id, &bob()).unwrap());
        let view = spaces.get(&id).unwrap().unwrap();
        assert!(view.live.is_empty() && view.stale.is_empty());
    }

    // ── refusals ────────────────────────────────────────────────────────────

    #[test]
    fn a_space_cannot_be_created_with_an_empty_or_whitespace_name() {
        let (spaces, _trust, _dir) = new_store();
        for raw in ["", "   ", "\n"] {
            assert!(
                matches!(spaces.create(raw), Err(SpaceError::EmptyName(_))),
                "{raw:?} was accepted"
            );
        }
        assert!(spaces.list().unwrap().is_empty(), "and nothing was written");
    }

    #[test]
    fn an_oversized_or_undisplayable_name_is_refused_on_create_and_on_rename() {
        let (spaces, _trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;

        let long = "x".repeat(MAX_NAME + 1);
        assert!(matches!(
            spaces.create(&long),
            Err(SpaceError::NameTooLong { .. })
        ));
        assert!(matches!(
            spaces.rename(&id, &long),
            Err(SpaceError::NameTooLong { .. })
        ));
        assert!(matches!(
            spaces.create("work\u{202E}nimda"),
            Err(SpaceError::UndisplayableName { .. })
        ));
        assert!(matches!(
            spaces.rename(&id, "a\nb"),
            Err(SpaceError::UndisplayableName { .. })
        ));
        assert_eq!(
            spaces.get(&id).unwrap().unwrap().name,
            "Work",
            "a refused rename must not have half-applied"
        );
    }

    /// The refusal names the space that already holds the name, so the user can
    /// find it — and it fires across casing and padding, which is what makes
    /// `by_name` unambiguous.
    #[test]
    fn a_duplicate_name_is_refused_and_says_which_space_holds_it() {
        let (spaces, _trust, _dir) = new_store();
        spaces.create("Work").unwrap();

        for attempt in ["Work", "work", "  WORK  "] {
            let err = spaces.create(attempt).expect_err("accepted a duplicate");
            let text = err.to_string();
            assert!(matches!(err, SpaceError::NameTaken { .. }), "{text}");
            assert!(text.contains("Work"), "the message must name it: {text}");
        }
        assert_eq!(spaces.list().unwrap().len(), 1);
    }

    #[test]
    fn a_rename_onto_another_spaces_name_is_refused() {
        let (spaces, _trust, _dir) = new_store();
        spaces.create("Work").unwrap();
        let home = spaces.create("Home").unwrap().id;

        assert!(matches!(
            spaces.rename(&home, "work"),
            Err(SpaceError::NameTaken { .. })
        ));
        assert_eq!(spaces.get(&home).unwrap().unwrap().name, "Home");
    }

    #[test]
    fn a_member_id_that_is_not_a_usable_device_id_is_refused() {
        let (spaces, _trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;

        for raw in ["", "pb- alice", "pb-\u{0007}"] {
            assert!(
                matches!(
                    spaces.add_member(&id, &DeviceId::from(raw)),
                    Err(SpaceError::BadMember { .. })
                ),
                "{raw:?} was accepted as a member"
            );
        }
        assert!(spaces.get(&id).unwrap().unwrap().live.is_empty());
    }

    /// A well-formed id for a device nobody has ever trusted is a typo, and the
    /// refusal says what to do about it.
    #[test]
    fn a_member_this_machine_has_never_trusted_is_refused_by_name() {
        let (spaces, _trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;

        let err = spaces
            .add_member(&id, &DeviceId::from("pb-stranger01"))
            .expect_err("accepted an unknown device");
        assert!(matches!(err, SpaceError::UnknownMember { .. }));
        let text = err.to_string();
        assert!(
            text.contains("pb-stranger01") && text.contains("pair"),
            "{text}"
        );
    }

    /// Re-adding a member whose trust was revoked reports *that*, rather than a
    /// "nothing to do" that hides why the space no longer reaches it.
    #[test]
    fn re_adding_a_revoked_member_reports_the_revocation_not_a_no_op() {
        let (spaces, trust, _dir) = new_store();
        let id = spaces.create("Work").unwrap().id;
        spaces.add_member(&id, &bob()).unwrap();
        trust.revoke("pb-bob");

        assert!(matches!(
            spaces.add_member(&id, &bob()),
            Err(SpaceError::UnknownMember { .. })
        ));
        assert_eq!(
            spaces.get(&id).unwrap().unwrap().stale,
            vec![bob()],
            "and the membership is untouched by the refusal"
        );
    }

    /// Fails closed: a trust store that cannot answer is not a reason to add.
    #[test]
    fn an_unreadable_trust_store_refuses_an_add_rather_than_assuming() {
        let dir = tempfile::tempdir().unwrap();
        let trust = Arc::new(FakeTrust {
            pinned: Mutex::new(vec!["pb-alice".to_string()]),
            broken: true,
        });
        let spaces = SpaceStore::new(app_store(dir.path()), trust);
        // `create` writes and reads nothing from trust, so it still works —
        // only the membership question is unanswerable.
        let id = spaces.create("Work").unwrap().id;

        let err = spaces.add_member(&id, &alice()).expect_err("accepted");
        assert!(matches!(err, SpaceError::TrustUnreadable { .. }));
        assert!(err.to_string().contains("unreadable"), "{err}");
    }

    // ── storage honesty ─────────────────────────────────────────────────────

    #[test]
    fn an_undecodable_record_is_skipped_by_list_but_reported_by_get() {
        let (spaces, _trust, dir) = new_store();
        let good = spaces.create("Work").unwrap().id;
        app_store(dir.path())
            .put(NS, "junk", b"not json at all")
            .unwrap();

        let listed = spaces.list().unwrap();
        assert_eq!(listed.len(), 1, "one bad row hid every readable space");
        assert_eq!(listed[0].id, good);

        assert!(
            matches!(spaces.get("junk"), Err(SpaceError::Serialization(_))),
            "asking for that space by id must not answer 'no such space'"
        );
    }

    /// A record written without a member list is a space with no members, not
    /// an unreadable row.
    #[test]
    fn a_record_with_no_member_list_loads_as_an_empty_space() {
        let (spaces, _trust, dir) = new_store();
        app_store(dir.path())
            .put(NS, "s1", br#"{"id":"s1","name":"Work"}"#)
            .unwrap();

        let view = spaces.get("s1").unwrap().expect("loaded");
        assert_eq!(view.name, "Work");
        assert!(view.live.is_empty() && view.stale.is_empty());
    }

    #[test]
    fn minted_ids_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(mint_id()), "a minted space id repeated");
        }
        assert!(seen.iter().all(|id| id.len() == 16));
    }
}
