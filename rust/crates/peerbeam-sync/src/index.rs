//! What this device knows about the files it syncs.
//!
//! # Why an index has to exist
//!
//! A version vector cannot be derived from a file: the filesystem records a
//! size and an mtime, not who edited it or how many times. So the vectors live
//! here, keyed by folder and path, and are updated whenever this device changes
//! a file or accepts a peer's version.
//!
//! That makes the index the one thing that must not silently disagree with the
//! disk. Two rules keep it honest:
//!
//! * **A file whose size or mtime no longer matches its index entry has been
//!   edited outside PeerBeam**, and that counts as an edit by this device — the
//!   user changing a file in their editor is exactly as real as receiving one.
//! * **An index entry with no file is a deletion**, not a missing record, and
//!   it keeps its vector so the deletion can propagate.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use peerbeam_domain::port::AppStore;
use serde::{Deserialize, Serialize};

use crate::manifest::SyncError;
use crate::version::VersionVector;

/// The AppStore namespace the index lives in.
pub const NS: &str = "sync-index";

/// What this device last knew about one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexEntry {
    /// Path relative to the synced folder.
    pub path: String,
    /// Size when last indexed, for spotting an outside edit.
    pub size: u64,
    /// Mtime when last indexed, same purpose.
    pub modified: i64,
    pub version: VersionVector,
    /// Whether this records a deletion. The entry stays so the deletion can
    /// reach peers; a removed record would look like a file nobody ever had.
    #[serde(default)]
    pub deleted: bool,
}

/// Per-folder file versions, persisted in the encrypted AppStore.
#[derive(Clone)]
pub struct SyncIndex {
    store: Arc<dyn AppStore>,
    /// This device's own id — the counter `bump` raises.
    device: String,
}

impl SyncIndex {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>, device: &str) -> Self {
        SyncIndex {
            store,
            device: device.to_string(),
        }
    }

    fn namespace(folder: &str) -> String {
        format!("{NS}-{folder}")
    }

    /// Everything known about `folder`, keyed by relative path.
    pub fn load(&self, folder: &str) -> Result<BTreeMap<String, IndexEntry>, SyncError> {
        let rows = self
            .store
            .list(&Self::namespace(folder))
            .map_err(|e| SyncError::Serialization(e.to_string()))?;
        Ok(rows
            .into_iter()
            // An unreadable row is skipped rather than fatal: one bad record
            // must not make a whole folder unsyncable.
            .filter_map(|(_, v)| serde_json::from_slice::<IndexEntry>(&v).ok())
            .map(|e| (e.path.clone(), e))
            .collect())
    }

    pub fn put(&self, folder: &str, entry: &IndexEntry) -> Result<(), SyncError> {
        let bytes =
            serde_json::to_vec(entry).map_err(|e| SyncError::Serialization(e.to_string()))?;
        self.store
            .put(&Self::namespace(folder), &key(&entry.path), &bytes)
            .map_err(|e| SyncError::Serialization(e.to_string()))
    }

    /// Bring the index in line with what is actually on disk, and report the
    /// paths this device is now responsible for having changed.
    ///
    /// A file that differs from its entry — or has no entry at all — has been
    /// edited outside PeerBeam, and its vector gains one of **this** device's
    /// edits. A file whose entry exists but whose bytes are gone is recorded as
    /// deleted, keeping its vector so the deletion propagates rather than
    /// looking like a file nobody ever had.
    pub fn rescan(&self, folder: &str, root: &Path) -> Result<Vec<String>, SyncError> {
        let mut known = self.load(folder)?;
        let mut changed = Vec::new();

        for (path, size, modified) in walk(root) {
            match known.remove(&path) {
                Some(mut e) if !e.deleted && e.size == size && e.modified == modified => {
                    // Unchanged; keep the entry as-is.
                    e.deleted = false;
                    self.put(folder, &e)?;
                }
                Some(mut e) => {
                    // Edited outside PeerBeam, or resurrected after a delete.
                    e.size = size;
                    e.modified = modified;
                    e.deleted = false;
                    e.version.bump(&self.device);
                    self.put(folder, &e)?;
                    changed.push(path);
                }
                None => {
                    let mut version = VersionVector::new();
                    version.bump(&self.device);
                    self.put(
                        folder,
                        &IndexEntry {
                            path: path.clone(),
                            size,
                            modified,
                            version,
                            deleted: false,
                        },
                    )?;
                    changed.push(path);
                }
            }
        }

        // Whatever is left in `known` had an entry but no file on disk.
        for (path, mut e) in known {
            if e.deleted {
                continue;
            }
            e.deleted = true;
            e.size = 0;
            e.version.bump(&self.device);
            self.put(folder, &e)?;
            changed.push(path);
        }
        Ok(changed)
    }
}

/// A storage key for a path.
///
/// Paths contain `/` and the store keys on a flat name, so the separator is
/// escaped rather than left to collide: `a/b` and `a-b` must not become the
/// same record.
fn key(path: &str) -> String {
    path.replace('%', "%25").replace('/', "%2F")
}

/// Every regular file under `root`, relative path with size and mtime.
fn walk(root: &Path) -> Vec<(String, u64, i64)> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, u64, i64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // Symlinks are skipped for the reason the manifest skips them: following
        // one can leave the folder or loop, and the file it names is not this
        // folder's to sync.
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect(root, &path, out);
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            out.push((
                rel.to_string_lossy().into_owned(),
                meta.len(),
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::port::EncryptionProvider;

    fn index(dir: &Path) -> SyncIndex {
        let enc: Arc<dyn EncryptionProvider> = Arc::new(peerbeam_crypto::AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[11u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.join("appstore"),
            key,
            enc,
        ));
        SyncIndex::new(app, "pb-me")
    }

    #[test]
    fn a_new_file_gains_one_of_this_devices_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();
        let idx = index(dir.path());

        let changed = idx.rescan("f", &root).unwrap();
        assert_eq!(changed, vec!["a.txt"]);
        let loaded = idx.load("f").unwrap();
        assert_eq!(loaded["a.txt"].version.get("pb-me"), 1);
    }

    #[test]
    fn an_unchanged_file_does_not_gain_an_edit() {
        // Otherwise every rescan would look like an edit and every sync would
        // re-send the whole folder.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();
        let idx = index(dir.path());

        idx.rescan("f", &root).unwrap();
        let changed = idx.rescan("f", &root).unwrap();
        assert!(changed.is_empty(), "an untouched file counted as edited");
        assert_eq!(idx.load("f").unwrap()["a.txt"].version.get("pb-me"), 1);
    }

    /// **Editing a file in an editor is exactly as real as receiving one.**
    /// A change PeerBeam did not make still has to raise the vector, or the
    /// next sync would quietly overwrite it with a peer's older copy.
    #[test]
    fn a_file_edited_outside_peerbeam_counts_as_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, b"first").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();

        std::fs::write(&file, b"edited outside").unwrap();
        let changed = idx.rescan("f", &root).unwrap();
        assert_eq!(changed, vec!["a.txt"]);
        assert_eq!(idx.load("f").unwrap()["a.txt"].version.get("pb-me"), 2);
    }

    /// **A deletion is a fact with a version, not a missing record.** Removing
    /// the entry would make the file look like one nobody ever had, and the
    /// next sync would take it back from the peer.
    #[test]
    fn a_deleted_file_keeps_its_entry_and_gains_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();

        std::fs::remove_file(root.join("a.txt")).unwrap();
        let changed = idx.rescan("f", &root).unwrap();
        assert_eq!(changed, vec!["a.txt"]);
        let loaded = idx.load("f").unwrap();
        assert!(loaded["a.txt"].deleted);
        assert_eq!(loaded["a.txt"].version.get("pb-me"), 2);
    }

    #[test]
    fn a_deletion_is_not_re_reported_on_every_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        idx.rescan("f", &root).unwrap();

        let changed = idx.rescan("f", &root).unwrap();
        assert!(changed.is_empty(), "a settled deletion was re-reported");
        assert_eq!(idx.load("f").unwrap()["a.txt"].version.get("pb-me"), 2);
    }

    #[test]
    fn a_recreated_file_is_an_edit_on_top_of_its_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        idx.rescan("f", &root).unwrap();

        std::fs::write(root.join("a.txt"), b"back").unwrap();
        idx.rescan("f", &root).unwrap();
        let loaded = idx.load("f").unwrap();
        assert!(!loaded["a.txt"].deleted);
        assert_eq!(loaded["a.txt"].version.get("pb-me"), 3);
    }

    #[test]
    fn nested_paths_do_not_collide_in_storage() {
        // `a/b` and `a-b` are different files and must not share a record.
        assert_ne!(key("a/b"), key("a-b"));
        assert_ne!(key("a/b"), key("a%2Fb"));
    }

    #[test]
    fn subdirectories_are_indexed_by_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"deep").unwrap();
        let idx = index(dir.path());

        idx.rescan("f", &root).unwrap();
        assert!(idx.load("f").unwrap().contains_key("sub/b.txt"));
    }
}
