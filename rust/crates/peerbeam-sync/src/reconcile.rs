//! Deciding what to do about each file, given both sides' versions.
//!
//! This is where bidirectional sync differs from a pull mirror. A pull only
//! ever asks "is theirs newer?" and refuses when unsure. With version vectors
//! there are five distinct answers, and the one that matters is the fifth:
//! **both changed**, which no clock can detect and no automatic rule can
//! resolve correctly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::index::IndexEntry;
use crate::version::{Relation, VersionVector};

/// What one file needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Fetch the peer's copy: they have edits we do not.
    Fetch { path: String },
    /// Send ours: we have edits they do not.
    Push { path: String },
    /// Delete ours: their deletion descends from our copy.
    Delete { path: String },
    /// **Both sides changed since they last agreed.** Their copy is fetched
    /// under a conflict name and ours is left exactly where it is — see
    /// [`conflict_name`].
    Conflict { path: String, keep_as: String },
    /// The peer has this file under a new name and we already hold the bytes.
    ///
    /// Moving a local file costs nothing; fetching it again costs the whole
    /// file. Emitted instead of a `Fetch`/`Delete` pair when the content hashes
    /// say they are the same bytes.
    Rename { from: String, to: String },
}

/// One file as a peer describes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub version: VersionVector,
    /// The peer's content hash, if it sent one. Empty means "cannot say", and
    /// an empty hash never pairs — a re-send costs bandwidth, a wrong pairing
    /// costs the file.
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub deleted: bool,
}

/// Where a peer's copy goes when both sides edited the same file.
///
/// **Neither copy is discarded and neither is declared correct.** The local
/// file keeps its name — the user's own edit stays where they left it, under
/// the name they know — and the peer's arrives beside it, labelled with whose
/// it is. Every automatic alternative loses somebody's work: last-writer-wins
/// discards an edit, refusing to sync leaves the folders permanently apart, and
/// merging text files is a guess that corrupts anything binary.
#[must_use]
pub fn conflict_name(path: &str, peer: &str) -> String {
    let (stem, ext) = match path.rfind('.') {
        // A leading dot is a hidden file, not an extension: `.bashrc` must not
        // become `.sync-conflict-x.bashrc`.
        Some(i) if i > 0 && !path[i + 1..].contains('/') => (&path[..i], &path[i..]),
        _ => (path, ""),
    };
    format!("{stem}.sync-conflict-{peer}{ext}")
}

/// Work out what to do about every file either side knows.
///
/// `local` is this device's index; `remote` is what the peer sent. `peer` names
/// the peer, for conflict file names.
#[must_use]
pub fn reconcile(
    local: &BTreeMap<String, IndexEntry>,
    remote: &[RemoteFile],
    peer: &str,
) -> Vec<Action> {
    let actions = reconcile_raw(local, remote, peer);
    collapse_renames(local, remote, actions)
}

/// Turn a `Fetch` and a `Delete` that describe the same bytes into a `Rename`.
///
/// Run **after** reconciliation rather than inside it: whether two paths hold
/// the same content is a question about bytes, and whether a file should move
/// at all is a question about versions. Keeping them apart means the version
/// logic never has to know about hashes, and this pass never has to know about
/// vectors.
#[must_use]
fn collapse_renames(
    local: &BTreeMap<String, IndexEntry>,
    remote: &[RemoteFile],
    actions: Vec<Action>,
) -> Vec<Action> {
    let remote_hash: BTreeMap<&str, &str> = remote
        .iter()
        .map(|f| (f.path.as_str(), f.content.as_str()))
        .collect();

    // Candidates: files we are about to delete, and files we are about to
    // fetch, each with the content hash of the bytes involved.
    let deleting: Vec<(String, String)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Delete { path } => {
                let h = local.get(path).map(|e| e.content.clone())?;
                (!h.is_empty()).then(|| (path.clone(), h))
            }
            _ => None,
        })
        .collect();
    let fetching: Vec<(String, String)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Fetch { path } => {
                let h = remote_hash.get(path.as_str())?;
                (!h.is_empty()).then(|| (path.clone(), (*h).to_string()))
            }
            _ => None,
        })
        .collect();

    let (renames, _, _) = crate::rename::detect(&deleting, &fetching);
    if renames.is_empty() {
        return actions;
    }

    let moved_from: BTreeMap<&str, &str> = renames
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str()))
        .collect();
    let moved_to: std::collections::BTreeSet<&str> =
        renames.iter().map(|r| r.to.as_str()).collect();

    let mut out = Vec::with_capacity(actions.len());
    for action in actions {
        match &action {
            // The delete becomes the rename; the matching fetch disappears.
            Action::Delete { path } if moved_from.contains_key(path.as_str()) => {
                out.push(Action::Rename {
                    from: path.clone(),
                    to: moved_from[path.as_str()].to_string(),
                });
            }
            Action::Fetch { path } if moved_to.contains(path.as_str()) => {}
            _ => out.push(action),
        }
    }
    out
}

/// Reconciliation proper, before renames are collapsed.
#[must_use]
fn reconcile_raw(
    local: &BTreeMap<String, IndexEntry>,
    remote: &[RemoteFile],
    peer: &str,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let remote_by_path: BTreeMap<&str, &RemoteFile> =
        remote.iter().map(|f| (f.path.as_str(), f)).collect();

    for f in remote {
        match local.get(&f.path) {
            // Never seen it. Take it unless the peer is telling us about a
            // deletion of something we never had, which is nothing to do.
            None => {
                if !f.deleted {
                    actions.push(Action::Fetch {
                        path: f.path.clone(),
                    });
                }
            }
            Some(mine) => match mine.version.relate(&f.version) {
                Relation::Same => {}
                Relation::Ahead => {
                    // Ours descends from theirs. Push, unless ours is a
                    // deletion they have already applied.
                    if !(mine.deleted && f.deleted) {
                        actions.push(Action::Push {
                            path: f.path.clone(),
                        });
                    }
                }
                Relation::Behind => {
                    if f.deleted {
                        // Their deletion descends from our copy, so it is not a
                        // conflict — they removed a file we had not touched
                        // since. Only act if we still have it.
                        if !mine.deleted {
                            actions.push(Action::Delete {
                                path: f.path.clone(),
                            });
                        }
                    } else {
                        actions.push(Action::Fetch {
                            path: f.path.clone(),
                        });
                    }
                }
                Relation::Diverged => {
                    // Both changed. A deletion on one side and an edit on the
                    // other is still a conflict: the edit is somebody's work and
                    // deleting it because a clock said so is the loss this
                    // whole design exists to prevent.
                    actions.push(Action::Conflict {
                        path: f.path.clone(),
                        keep_as: conflict_name(&f.path, peer),
                    });
                }
            },
        }
    }

    // Files we have that the peer never mentioned. A deletion is not pushed:
    // the peer has no record to delete, and pushing "this is gone" for a file
    // they never had would be noise.
    for (path, mine) in local {
        if remote_by_path.contains_key(path.as_str()) || mine.deleted {
            continue;
        }
        actions.push(Action::Push { path: path.clone() });
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vv(pairs: &[(&str, u64)]) -> VersionVector {
        let mut v = VersionVector::new();
        for (d, n) in pairs {
            for _ in 0..*n {
                v.bump(d);
            }
        }
        v
    }

    fn local(entries: &[(&str, VersionVector, bool)]) -> BTreeMap<String, IndexEntry> {
        entries
            .iter()
            .map(|(p, v, deleted)| {
                (
                    (*p).to_string(),
                    IndexEntry {
                        path: (*p).to_string(),
                        size: 1,
                        modified: 0,
                        content: String::new(),
                        version: v.clone(),
                        deleted: *deleted,
                    },
                )
            })
            .collect()
    }

    fn remote(path: &str, v: VersionVector, deleted: bool) -> RemoteFile {
        RemoteFile {
            path: path.to_string(),
            size: 1,
            version: v,
            content: String::new(),
            deleted,
        }
    }

    /// A remote file that also states its content hash.
    fn remote_with(path: &str, v: VersionVector, content: &str) -> RemoteFile {
        RemoteFile {
            path: path.to_string(),
            size: 1,
            version: v,
            content: content.to_string(),
            deleted: false,
        }
    }

    fn local_with(entries: &[(&str, VersionVector, bool, &str)]) -> BTreeMap<String, IndexEntry> {
        entries
            .iter()
            .map(|(p, v, deleted, content)| {
                (
                    (*p).to_string(),
                    IndexEntry {
                        path: (*p).to_string(),
                        size: 1,
                        modified: 0,
                        content: (*content).to_string(),
                        version: v.clone(),
                        deleted: *deleted,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_file_only_the_peer_has_is_fetched() {
        let acts = reconcile(&local(&[]), &[remote("a", vv(&[("b", 1)]), false)], "bob");
        assert_eq!(
            acts,
            vec![Action::Fetch {
                path: "a".to_string()
            }]
        );
    }

    #[test]
    fn a_file_only_we_have_is_pushed() {
        let acts = reconcile(&local(&[("a", vv(&[("me", 1)]), false)]), &[], "bob");
        assert_eq!(
            acts,
            vec![Action::Push {
                path: "a".to_string()
            }]
        );
    }

    #[test]
    fn matching_versions_need_nothing() {
        let v = vv(&[("me", 1)]);
        let acts = reconcile(
            &local(&[("a", v.clone(), false)]),
            &[remote("a", v, false)],
            "bob",
        );
        assert!(acts.is_empty());
    }

    /// **The case that makes this bidirectional.** Both edited; neither is
    /// discarded and neither is declared correct.
    #[test]
    fn concurrent_edits_become_a_conflict_that_keeps_both() {
        let acts = reconcile(
            &local(&[("notes.txt", vv(&[("me", 2), ("bob", 1)]), false)]),
            &[remote("notes.txt", vv(&[("me", 1), ("bob", 2)]), false)],
            "bob",
        );
        assert_eq!(
            acts,
            vec![Action::Conflict {
                path: "notes.txt".to_string(),
                keep_as: "notes.sync-conflict-bob.txt".to_string(),
            }]
        );
    }

    /// A deletion on one side against an edit on the other is **still** a
    /// conflict. Deleting somebody's edit because the other side removed the
    /// file is exactly the loss this design exists to prevent.
    #[test]
    fn a_delete_against_a_concurrent_edit_is_a_conflict_not_a_delete() {
        let acts = reconcile(
            &local(&[("a", vv(&[("me", 2), ("bob", 1)]), false)]),
            &[remote("a", vv(&[("me", 1), ("bob", 2)]), true)],
            "bob",
        );
        assert!(matches!(acts[0], Action::Conflict { .. }));
    }

    #[test]
    fn a_deletion_that_descends_from_our_copy_is_applied() {
        // They deleted a file we had not touched since receiving it. Not a
        // conflict — a straightforward delete.
        let acts = reconcile(
            &local(&[("a", vv(&[("bob", 1)]), false)]),
            &[remote("a", vv(&[("bob", 2)]), true)],
            "bob",
        );
        assert_eq!(
            acts,
            vec![Action::Delete {
                path: "a".to_string()
            }]
        );
    }

    #[test]
    fn a_deletion_we_already_applied_does_nothing() {
        let v = vv(&[("bob", 2)]);
        let acts = reconcile(
            &local(&[("a", v.clone(), true)]),
            &[remote("a", v, true)],
            "bob",
        );
        assert!(acts.is_empty());
    }

    #[test]
    fn our_deletion_is_pushed_when_the_peer_still_has_the_file() {
        let acts = reconcile(
            &local(&[("a", vv(&[("bob", 1), ("me", 1)]), true)]),
            &[remote("a", vv(&[("bob", 1)]), false)],
            "bob",
        );
        assert_eq!(
            acts,
            vec![Action::Push {
                path: "a".to_string()
            }]
        );
    }

    #[test]
    fn a_local_deletion_the_peer_never_knew_about_is_not_pushed() {
        // Telling a peer "this file you never had is gone" is noise.
        let acts = reconcile(&local(&[("a", vv(&[("me", 2)]), true)]), &[], "bob");
        assert!(acts.is_empty());
    }

    /// **A move is a move, not a re-download.** The peer deleted `old` and has
    /// `new` with the same bytes; we already hold those bytes, so the file is
    /// renamed rather than fetched.
    #[test]
    fn a_delete_and_a_fetch_of_the_same_bytes_become_a_rename() {
        let acts = reconcile(
            &local_with(&[("old.txt", vv(&[("bob", 1)]), false, "sha-of-bytes")]),
            &[
                remote("old.txt", vv(&[("bob", 2)]), true),
                remote_with("new.txt", vv(&[("bob", 2)]), "sha-of-bytes"),
            ],
            "bob",
        );
        assert_eq!(
            acts,
            vec![Action::Rename {
                from: "old.txt".to_string(),
                to: "new.txt".to_string(),
            }],
            "the move was not collapsed: {acts:?}"
        );
    }

    /// Different bytes are never collapsed, however suggestive the timing. A
    /// wrong pairing costs the file; a re-send costs bandwidth.
    #[test]
    fn a_delete_and_a_fetch_of_different_bytes_stay_separate() {
        let acts = reconcile(
            &local_with(&[("old.txt", vv(&[("bob", 1)]), false, "hash-a")]),
            &[
                remote("old.txt", vv(&[("bob", 2)]), true),
                remote_with("new.txt", vv(&[("bob", 2)]), "hash-b"),
            ],
            "bob",
        );
        assert!(
            acts.contains(&Action::Delete {
                path: "old.txt".to_string()
            }),
            "{acts:?}"
        );
        assert!(
            acts.contains(&Action::Fetch {
                path: "new.txt".to_string()
            }),
            "{acts:?}"
        );
        assert!(!acts.iter().any(|a| matches!(a, Action::Rename { .. })));
    }

    /// A peer that cannot state a content hash sends an empty one, and an empty
    /// hash must never pair — otherwise every unhashed delete/fetch pair in one
    /// sync would collapse into an arbitrary move.
    #[test]
    fn an_empty_content_hash_never_pairs() {
        let acts = reconcile(
            &local_with(&[("old.txt", vv(&[("bob", 1)]), false, "")]),
            &[
                remote("old.txt", vv(&[("bob", 2)]), true),
                remote_with("new.txt", vv(&[("bob", 2)]), ""),
            ],
            "bob",
        );
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Rename { .. })),
            "{acts:?}"
        );
    }

    #[test]
    fn a_conflict_name_keeps_the_extension_so_the_file_still_opens() {
        assert_eq!(
            conflict_name("report.pdf", "bob"),
            "report.sync-conflict-bob.pdf"
        );
        assert_eq!(
            conflict_name("sub/dir/a.txt", "bob"),
            "sub/dir/a.sync-conflict-bob.txt"
        );
    }

    #[test]
    fn a_conflict_name_handles_files_without_an_extension() {
        assert_eq!(conflict_name("README", "bob"), "README.sync-conflict-bob");
        // A leading dot is a hidden file, not an extension.
        assert_eq!(conflict_name(".bashrc", "bob"), ".bashrc.sync-conflict-bob");
    }
}
