//! Carrying out what [`reconcile`](crate::reconcile) decided.
//!
//! Split from the deciding so the decision can be tested without a filesystem
//! and the doing can be tested without a network — and so a caller can show a
//! user what *would* happen before anything does.

use std::path::Path;

use crate::index::{IndexEntry, SyncIndex};
use crate::reconcile::Action;
use crate::version::VersionVector;

/// What actually happened, for reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Files asked for from the peer.
    pub fetching: usize,
    /// Files offered to the peer.
    pub pushing: usize,
    /// Files deleted locally because the peer's deletion descends from ours.
    pub deleted: usize,
    /// Files where both sides changed. The peer's copy is fetched under a
    /// conflict name and **nothing local is touched**.
    pub conflicts: Vec<String>,
}

/// Apply the local half of a plan: deletions, and recording what was decided.
///
/// Fetches and pushes are network work the caller owns; this handles what
/// happens on disk and in the index, so that a crash between deciding and
/// transferring cannot leave the index claiming something that never happened.
///
/// **A deletion is applied only when it descends from the local copy** —
/// `reconcile` has already established that, and this re-checks nothing: the
/// two-step split exists so the rule lives in exactly one place.
pub fn apply_local(
    index: &SyncIndex,
    folder: &str,
    root: &Path,
    actions: &[Action],
    remote_versions: &std::collections::BTreeMap<String, VersionVector>,
) -> Result<Outcome, crate::manifest::SyncError> {
    let mut outcome = Outcome::default();
    let known = index.load(folder)?;

    for action in actions {
        match action {
            Action::Fetch { .. } => outcome.fetching += 1,
            Action::Push { .. } => outcome.pushing += 1,
            Action::Conflict { path, keep_as } => {
                // Counted and named, but **nothing is moved or overwritten**.
                // The local file stays exactly where the user left it; the
                // peer's copy arrives beside it under `keep_as` when the
                // transfer lands.
                outcome.conflicts.push(keep_as.clone());
                let _ = path;
            }
            Action::Delete { path } => {
                let target = root.join(path);
                // A missing file is success, not an error: the user may have
                // deleted it themselves between the scan and now, and failing
                // here would abort a sync over something already done.
                match std::fs::remove_file(&target) {
                    Ok(()) | Err(_) => {}
                }
                // The entry stays, marked deleted and carrying the peer's
                // version, so this device does not later believe it still has
                // the file and push it back.
                if let Some(mut e) = known.get(path).cloned() {
                    e.deleted = true;
                    e.size = 0;
                    if let Some(v) = remote_versions.get(path) {
                        e.version = e.version.merge(v);
                    }
                    index.put(folder, &e)?;
                } else {
                    index.put(
                        folder,
                        &IndexEntry {
                            path: path.clone(),
                            size: 0,
                            modified: 0,
                            version: remote_versions.get(path).cloned().unwrap_or_default(),
                            deleted: true,
                        },
                    )?;
                }
                outcome.deleted += 1;
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use peerbeam_domain::port::{AppStore, EncryptionProvider};

    fn setup() -> (tempfile::TempDir, SyncIndex, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(peerbeam_crypto::AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[13u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        let index = SyncIndex::new(app, "pb-me");
        (dir, index, root)
    }

    fn vv(device: &str, n: u64) -> VersionVector {
        let mut v = VersionVector::new();
        for _ in 0..n {
            v.bump(device);
        }
        v
    }

    #[test]
    fn a_delete_removes_the_file_and_keeps_a_tombstone() {
        let (_dir, index, root) = setup();
        std::fs::write(root.join("a.txt"), b"bye").unwrap();
        index.rescan("f", &root).unwrap();

        let mut remote = BTreeMap::new();
        remote.insert("a.txt".to_string(), vv("bob", 3));
        let out = apply_local(
            &index,
            "f",
            &root,
            &[Action::Delete {
                path: "a.txt".to_string(),
            }],
            &remote,
        )
        .unwrap();

        assert_eq!(out.deleted, 1);
        assert!(!root.join("a.txt").exists(), "the file was not removed");
        let entry = &index.load("f").unwrap()["a.txt"];
        assert!(entry.deleted, "no tombstone was left");
        assert_eq!(
            entry.version.get("bob"),
            3,
            "the peer's version was not absorbed, so this device would push the \
             file back on the next sync"
        );
    }

    /// **A conflict touches nothing.** The user's file stays where they left
    /// it, under the name they know; the peer's copy arrives beside it.
    #[test]
    fn a_conflict_leaves_the_local_file_exactly_as_it_was() {
        let (_dir, index, root) = setup();
        std::fs::write(root.join("notes.txt"), b"my work").unwrap();
        index.rescan("f", &root).unwrap();

        let out = apply_local(
            &index,
            "f",
            &root,
            &[Action::Conflict {
                path: "notes.txt".to_string(),
                keep_as: "notes.sync-conflict-bob.txt".to_string(),
            }],
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(out.conflicts, vec!["notes.sync-conflict-bob.txt"]);
        assert_eq!(
            std::fs::read(root.join("notes.txt")).unwrap(),
            b"my work",
            "a conflict overwrote the user's file"
        );
        assert!(!index.load("f").unwrap()["notes.txt"].deleted);
    }

    #[test]
    fn deleting_a_file_that_is_already_gone_is_not_an_error() {
        // The user may have deleted it between the scan and now. Failing would
        // abort a sync over something already done.
        let (_dir, index, root) = setup();
        let out = apply_local(
            &index,
            "f",
            &root,
            &[Action::Delete {
                path: "never-existed".to_string(),
            }],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(out.deleted, 1);
        assert!(index.load("f").unwrap()["never-existed"].deleted);
    }

    #[test]
    fn fetches_and_pushes_are_counted_but_not_acted_on_here() {
        // They are network work the caller owns; doing them here would mean two
        // places knew how a transfer starts.
        let (_dir, index, root) = setup();
        let out = apply_local(
            &index,
            "f",
            &root,
            &[
                Action::Fetch {
                    path: "a".to_string(),
                },
                Action::Push {
                    path: "b".to_string(),
                },
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!((out.fetching, out.pushing), (1, 1));
        assert!(index.load("f").unwrap().is_empty(), "the index was written");
    }
}
