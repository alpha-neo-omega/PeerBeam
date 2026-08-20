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
    /// Files moved locally instead of re-fetched.
    pub renamed: usize,
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
            Action::Rename { from, to } => {
                // Moving what we already hold, instead of fetching it again.
                //
                // **Both sides resolved, not joined.** `to` is a path the
                // *peer* chose: `collapse_renames` pairs a local delete with a
                // remote fetch on content hash alone, so the destination comes
                // off the wire. `root.join("../../.bashrc")` resolves outside
                // the sync root, and `fs::rename` would then move the user's
                // file there — a peer picking the name of a file it never had.
                // `local_path` drops `.` and `..` and keeps only real
                // components, so a hostile `to` lands inside the root or not at
                // all. `from` goes through it too: it is an index key today,
                // but a key is only ever as trustworthy as whatever wrote it.
                let src = peerbeam_domain::local_path(root, from);
                let dst = peerbeam_domain::local_path(root, to);
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&src, &dst).is_ok() {
                    // The index follows the bytes: the old path becomes a
                    // tombstone and the new one inherits the content hash, so
                    // the next scan sees a move rather than a delete and a
                    // create all over again.
                    if let Some(mut e) = known.get(from).cloned() {
                        let content = e.content.clone();
                        let version = e.version.clone();
                        e.deleted = true;
                        e.size = 0;
                        index.put(folder, &e)?;
                        index.put(
                            folder,
                            &IndexEntry {
                                path: to.clone(),
                                size: std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
                                modified: 0,
                                content,
                                version,
                                deleted: false,
                            },
                        )?;
                    }
                    outcome.renamed += 1;
                } else {
                    // The move failed — a permission problem, a vanished
                    // source. Fall back to fetching it, which is slower and
                    // always works.
                    outcome.fetching += 1;
                }
            }
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
                // Resolved, not joined: a delete names a path this device is
                // about to remove, and `..` in it would remove somebody else's.
                let target = peerbeam_domain::local_path(root, path);
                // A missing file is success, not an error: the user may have
                // deleted it themselves between the scan and now, and failing
                // here would abort a sync over something already done.
                //
                // **But "already gone" and "could not be removed" are different
                // facts.** The old arm collapsed every error into success, so a
                // permission problem or a file held open by another program was
                // recorded as deleted — and the index below then marks it
                // deleted, meaning this device stops offering a file it still
                // has and no later scan puts it back. That is a silent
                // divergence between the two peers, which is the one thing sync
                // must not produce quietly.
                match std::fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %path,
                            "sync could not delete a file the peer removed; it stays here \
                             and will be offered again"
                        );
                        // Not indexed as deleted: the file is still on disk, and
                        // claiming otherwise is what makes the divergence
                        // permanent.
                        continue;
                    }
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
                            // A tombstone for a file this device never held has
                            // no content to hash, so it can never pair as a
                            // rename — correct: nothing moved here.
                            content: String::new(),
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
pub(crate) mod tests_support {
    pub(crate) use super::tests::setup as setup_for_containment;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use peerbeam_domain::port::{AppStore, EncryptionProvider};

    pub(crate) fn setup() -> (tempfile::TempDir, SyncIndex, std::path::PathBuf) {
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

#[cfg(test)]
mod containment_tests {
    use super::tests_support::setup_for_containment as setup;
    use super::*;

    /// **A peer-chosen rename must not move a local file out of the root.**
    ///
    /// `collapse_renames` pairs a local delete with a remote fetch on content
    /// hash alone, so the destination comes off the wire. Joined naively,
    /// `../escaped.txt` resolves outside the sync root and `fs::rename` moves
    /// the user's own file there.
    #[test]
    fn a_rename_cannot_move_a_file_outside_the_root() {
        let (dir, index, root) = setup();
        std::fs::write(root.join("secret.txt"), b"mine").unwrap();
        let outside = dir.path().join("escaped.txt");

        let _ = apply_local(
            &index,
            "folder",
            &root,
            &[Action::Rename {
                from: "secret.txt".into(),
                to: "../../escaped.txt".into(),
            }],
            &std::collections::BTreeMap::new(),
        );

        assert!(!outside.exists(), "the file was moved out of the sync root");
        assert!(
            root.join("escaped.txt").exists() || root.join("secret.txt").exists(),
            "the file vanished entirely"
        );
    }

    /// The same containment for a delete: `..` must not reach outside.
    #[test]
    fn a_delete_cannot_remove_a_file_outside_the_root() {
        let (dir, index, root) = setup();
        let outside = dir.path().join("keepme.txt");
        std::fs::write(&outside, b"not yours").unwrap();

        let _ = apply_local(
            &index,
            "folder",
            &root,
            &[Action::Delete {
                path: "../keepme.txt".into(),
            }],
            &std::collections::BTreeMap::new(),
        );

        assert!(outside.exists(), "a file outside the sync root was deleted");
    }
}

#[cfg(test)]
mod delete_failure_tests {
    use super::tests_support::setup_for_containment as setup;
    use super::*;

    /// **"Already gone" and "could not be removed" are different facts.**
    ///
    /// The old arm collapsed every `remove_file` error into success, and the
    /// index below then marked the entry deleted — so this device stopped
    /// offering a file it still had, and no later scan put it back. A permanent
    /// silent divergence between two peers, which is the one outcome sync must
    /// never produce quietly.
    ///
    /// A directory stands in for the unremovable file: `remove_file` refuses it
    /// on every platform, with no permissions games and nothing to clean up.
    #[test]
    fn a_file_that_cannot_be_removed_is_not_recorded_as_deleted() {
        let (_dir, index, root) = setup();
        std::fs::create_dir_all(root.join("stubborn")).unwrap();

        // Seed the index as if we hold it, so "marked deleted" is observable.
        index
            .put(
                "folder",
                &IndexEntry {
                    path: "stubborn".into(),
                    size: 1,
                    modified: 0,
                    content: "abc".into(),
                    version: VersionVector::new(),
                    deleted: false,
                },
            )
            .unwrap();

        let _ = apply_local(
            &index,
            "folder",
            &root,
            &[Action::Delete {
                path: "stubborn".into(),
            }],
            &std::collections::BTreeMap::new(),
        );

        let after = index.load("folder").unwrap();
        let entry = after.get("stubborn").expect("the entry must survive");
        assert!(
            !entry.deleted,
            "a file still on disk was recorded as deleted, so it will never be offered again"
        );
        assert!(root.join("stubborn").exists(), "and it is still there");
    }

    /// A file that really is gone is still success — the user may have deleted
    /// it themselves between the scan and now.
    #[test]
    fn an_already_missing_file_is_still_a_successful_delete() {
        let (_dir, index, root) = setup();
        index
            .put(
                "folder",
                &IndexEntry {
                    path: "vanished.txt".into(),
                    size: 1,
                    modified: 0,
                    content: "abc".into(),
                    version: VersionVector::new(),
                    deleted: false,
                },
            )
            .unwrap();

        let out = apply_local(
            &index,
            "folder",
            &root,
            &[Action::Delete {
                path: "vanished.txt".into(),
            }],
            &std::collections::BTreeMap::new(),
        )
        .expect("a missing file is not an error");
        assert_eq!(out.deleted, 1);
        assert!(index.load("folder").unwrap()["vanished.txt"].deleted);
    }
}
