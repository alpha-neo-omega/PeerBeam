//! Spaces end to end, over the real encrypted `FsAppStore` and the real
//! `FsTrust` — so revoking a member is an actual `peerbeam trust revoke` and a
//! closed window is an actual expired record, not a fake answering `false`.

use std::sync::Arc;

use chrono::{Duration, Utc};

use peerbeam_appstore_fs::FsAppStore;
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_domain::entity::{PermissionSet, TrustRecord};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{AppStore, EncryptionProvider, TrustStore};
use peerbeam_spaces::{SpaceError, SpaceStore};
use peerbeam_trust_fs::FsTrust;

fn app_store(dir: &std::path::Path) -> Arc<dyn AppStore> {
    let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
    let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
    Arc::new(FsAppStore::open(dir.join("appstore"), key, enc))
}

fn trust_store(dir: &std::path::Path) -> Arc<FsTrust> {
    Arc::new(FsTrust::open(dir.join("trust.json")).expect("open trust store"))
}

/// Pin `id` the way the authenticated handshake does, optionally with a window
/// the user attached to it. `expires_at` is an absolute instant, so a closed
/// window is expressed by passing one already in the past rather than by
/// waiting for one to run out.
fn pin(trust: &FsTrust, id: &str, expires_at: Option<chrono::DateTime<Utc>>) {
    trust
        .record(TrustRecord {
            device: DeviceId::from(id),
            fingerprint: format!("fp-{id}"),
            name: id.to_string(),
            trusted_at: Utc::now(),
            approved: true,
            permissions: PermissionSet::granted_on_approval(),
            expires_at,
            mine: false,
            auto_accept: false,
        })
        .expect("pin a device");
}

fn alice() -> DeviceId {
    DeviceId::from("pb-alice00001")
}

fn bob() -> DeviceId {
    DeviceId::from("pb-bob0000001")
}

#[test]
fn a_space_and_its_members_round_trip_through_a_real_store() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(&trust, alice().as_str(), None);
    pin(&trust, bob().as_str(), None);

    let spaces = SpaceStore::new(app_store(dir.path()), trust);
    let id = spaces.create("Work Laptops").unwrap().id;
    assert!(spaces.add_member(&id, &alice()).unwrap());
    assert!(spaces.add_member(&id, &bob()).unwrap());

    // A second store over the same directory — which is what the CLI is, since
    // it opens fresh stores per command and must see what the app wrote.
    let reopened = SpaceStore::new(app_store(dir.path()), trust_store(dir.path()));
    let view = reopened.by_name("work laptops").unwrap().expect("found");
    assert_eq!(view.id, id);
    assert_eq!(view.name, "Work Laptops");
    assert_eq!(view.live, vec![alice(), bob()]);
    assert!(view.stale.is_empty());
}

/// **The revoked member, through the real revoke.** `FsTrust::remove` is what
/// `peerbeam trust revoke` calls; nothing writes to either space between the
/// reads either side of it.
#[test]
fn revoking_a_device_takes_it_out_of_every_space_at_the_next_read() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(&trust, alice().as_str(), None);
    pin(&trust, bob().as_str(), None);

    let spaces = SpaceStore::new(app_store(dir.path()), trust.clone());
    let work = spaces.create("Work").unwrap().id;
    let home = spaces.create("Home").unwrap().id;
    for space in [&work, &home] {
        spaces.add_member(space, &alice()).unwrap();
        spaces.add_member(space, &bob()).unwrap();
    }

    assert!(
        trust.remove(&bob()).unwrap(),
        "precondition: bob was pinned"
    );

    for space in [&work, &home] {
        let view = spaces.get(space).unwrap().unwrap();
        assert_eq!(
            view.live,
            vec![alice()],
            "a revoked device was still in the fan-out"
        );
        assert_eq!(
            view.stale,
            vec![bob()],
            "and it must be reported, not silently dropped"
        );
    }

    // The record kept it, so re-pairing restores the membership the user set up
    // rather than making them rebuild the space by hand.
    pin(&trust, bob().as_str(), None);
    assert_eq!(
        spaces.get(&work).unwrap().unwrap().live,
        vec![alice(), bob()]
    );
}

/// A time-limited grant closes with nothing running: the record is written once
/// with a deadline already in the past, and the very first read sorts that
/// member as stale. No sweeper, and nothing sleeps.
#[test]
fn a_member_whose_trust_window_has_closed_is_stale_without_anything_running() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(&trust, alice().as_str(), None);
    pin(
        &trust,
        bob().as_str(),
        Some(Utc::now() - Duration::hours(1)),
    );

    let spaces = SpaceStore::new(app_store(dir.path()), trust.clone());
    let id = spaces.create("Work").unwrap().id;
    spaces.add_member(&id, &alice()).unwrap();
    spaces.add_member(&id, &bob()).unwrap();

    let view = spaces.get(&id).unwrap().unwrap();
    assert_eq!(view.live, vec![alice()]);
    assert_eq!(view.stale, vec![bob()], "the window closed an hour ago");

    // Renewing it — `peerbeam trust approve` with no deadline — brings the
    // member back, because expiry never edited the space.
    trust.approve_for(&bob(), None).unwrap();
    assert_eq!(spaces.get(&id).unwrap().unwrap().live, vec![alice(), bob()]);
}

/// A device whose window is currently closed can still be *added*: the add
/// check asks whether this machine has ever trusted the device, not whether the
/// grant is live this second. Refusing here would make a space unbuildable
/// during a lapse that it is `view`'s job to report anyway.
#[test]
fn a_device_with_a_closed_window_can_be_added_and_is_reported_stale() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(
        &trust,
        bob().as_str(),
        Some(Utc::now() - Duration::hours(1)),
    );

    let spaces = SpaceStore::new(app_store(dir.path()), trust);
    let id = spaces.create("Work").unwrap().id;
    assert!(spaces.add_member(&id, &bob()).unwrap());
    assert_eq!(spaces.get(&id).unwrap().unwrap().stale, vec![bob()]);
}

#[test]
fn a_device_that_was_revoked_cannot_be_added_back_until_it_is_paired_again() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(&trust, bob().as_str(), None);

    let spaces = SpaceStore::new(app_store(dir.path()), trust.clone());
    let id = spaces.create("Work").unwrap().id;
    trust.remove(&bob()).unwrap();

    let err = spaces.add_member(&id, &bob()).expect_err("accepted");
    assert!(matches!(err, SpaceError::UnknownMember { .. }), "{err}");
    assert!(spaces.get(&id).unwrap().unwrap().live.is_empty());

    pin(&trust, bob().as_str(), None);
    assert!(spaces.add_member(&id, &bob()).unwrap());
}

/// Nothing about a Space reaches the wire, so nothing about it reaches trust
/// either: creating, renaming, filling, emptying and deleting a space leaves
/// the pinned records byte-identical. A Space is a label over trust, never a
/// change to it.
#[test]
fn defining_a_space_changes_nothing_about_trust() {
    let dir = tempfile::tempdir().unwrap();
    let trust = trust_store(dir.path());
    pin(&trust, alice().as_str(), None);
    pin(&trust, bob().as_str(), None);
    let before = std::fs::read(dir.path().join("trust.json")).unwrap();

    let spaces = SpaceStore::new(app_store(dir.path()), trust);
    let id = spaces.create("Work").unwrap().id;
    spaces.add_member(&id, &alice()).unwrap();
    spaces.add_member(&id, &bob()).unwrap();
    spaces.rename(&id, "Office").unwrap();
    spaces.remove_member(&id, &alice()).unwrap();
    spaces.delete(&id).unwrap();

    assert_eq!(
        std::fs::read(dir.path().join("trust.json")).unwrap(),
        before,
        "a space operation wrote to the trust store"
    );
}

/// The record is encrypted at rest by the store beneath (I11), so the private
/// label a user chose is not sitting in cleartext on disk next to it.
#[test]
fn a_space_name_is_not_readable_from_the_files_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let spaces = SpaceStore::new(app_store(dir.path()), trust_store(dir.path()));
    spaces.create("Very Secret Project").unwrap();

    let records = walk(&dir.path().join("appstore"));
    assert!(!records.is_empty(), "the test read no records at all");
    for record in records {
        let bytes = std::fs::read(&record).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("Very Secret Project"),
            "{} holds the name in cleartext",
            record.display()
        );
    }
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
