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
    /// SHA-256 of the file's bytes when last indexed, hex.
    ///
    /// What makes a rename recognisable: a deletion and a creation carrying the
    /// same hash are one file that moved, not a re-transfer waiting to happen.
    /// `default` so an index written before hashing still loads — an entry with
    /// no hash simply never pairs, which costs a re-send rather than risking a
    /// wrong one.
    #[serde(default)]
    pub content: String,
    /// Whether this records a deletion. The entry stays so the deletion can
    /// reach peers; a removed record would look like a file nobody ever had.
    #[serde(default)]
    pub deleted: bool,
}

/// Per-folder file versions, persisted in the encrypted AppStore.
#[derive(Clone)]
pub struct SyncIndex {
    store: Arc<dyn AppStore>,
    /// Chunk maps, so a rescan populates what delta transfer reads from.
    chunks: crate::store::ChunkStore,
    /// This device's own id — the counter `bump` raises.
    device: String,
}

impl SyncIndex {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>, device: &str) -> Self {
        SyncIndex {
            chunks: crate::store::ChunkStore::new(store.clone()),
            store,
            device: device.to_string(),
        }
    }

    /// The chunk maps this index records as it scans.
    #[must_use]
    pub fn chunks(&self) -> &crate::store::ChunkStore {
        &self.chunks
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

    /// Hash a file, record how it chunks, and return the content hash.
    ///
    /// A failure to store the map is not fatal: the file still syncs, just
    /// whole rather than by delta. Losing an optimisation is a far better
    /// outcome than failing a scan.
    fn record_chunks(&self, folder: &str, path: &Path) -> String {
        let Some((content, chunks)) = hash_and_chunk(path) else {
            return String::new();
        };
        if !chunks.is_empty() {
            let _ = self.chunks.put(folder, &content, &chunks);
        }
        content
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
                    e.content = self.record_chunks(folder, &root.join(&path));
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
                            content: self.record_chunks(folder, &root.join(&path)),
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
            // The hash is **kept**, not cleared: it is the only thing that can
            // pair this deletion with a creation elsewhere and recognise a
            // rename. Clearing it would turn every move back into a re-send.
            e.version.bump(&self.device);
            self.put(folder, &e)?;
            changed.push(path);
        }
        Ok(changed)
    }
}

/// SHA-256 of a file's bytes **and** its chunk map, or `None` if unreadable.
///
/// Hashed and chunked in one pass: both need every byte of the file, and
/// reading it twice would double the cost of a rescan for no benefit.
fn hash_and_chunk(path: &Path) -> Option<(String, Vec<peerbeam_chunk::Chunk>)> {
    // Chunking needs the whole file in memory to find content boundaries.
    // Bounded deliberately: above the limit the file is hashed by streaming and
    // no chunk map is produced, so it syncs whole rather than by delta. A
    // multi-gigabyte file must never be loaded to save bandwidth on it.
    const MAX_CHUNKABLE: u64 = 256 * 1024 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_CHUNKABLE {
        let h = content_hash(path);
        return (!h.is_empty()).then_some((h, Vec::new()));
    }
    let bytes = std::fs::read(path).ok()?;
    Some((peerbeam_chunk::hash(&bytes), peerbeam_chunk::split(&bytes)))
}

/// SHA-256 of a file's bytes, or empty if it cannot be read.
///
/// Empty on failure rather than an error: a file that vanished between the walk
/// and the hash is a race the next scan resolves, and failing the whole rescan
/// over one unreadable file would strand the rest of the folder. An empty hash
/// simply never pairs as a rename, which costs a re-send rather than risking a
/// wrong pairing.
fn content_hash(path: &Path) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    // Streamed in fixed blocks: a file must never be read into memory whole.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return String::new(),
        }
    }
    let digest = hasher.finalize();
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
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
    fn a_files_content_hash_is_recorded_and_tracks_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"first").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();
        let first = idx.load("f").unwrap()["a.txt"].content.clone();
        assert_eq!(first.len(), 64, "not a sha-256 hex digest: {first}");

        std::fs::write(root.join("a.txt"), b"second").unwrap();
        idx.rescan("f", &root).unwrap();
        assert_ne!(
            idx.load("f").unwrap()["a.txt"].content,
            first,
            "the hash did not follow the edit"
        );
    }

    /// **A tombstone keeps its hash.** It is the only thing that can pair the
    /// deletion with a creation elsewhere and recognise a rename; clearing it
    /// would turn every move back into a full re-send.
    #[test]
    fn a_deleted_entry_keeps_the_hash_that_makes_a_rename_visible() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"contents").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();
        let hash = idx.load("f").unwrap()["a.txt"].content.clone();

        std::fs::remove_file(root.join("a.txt")).unwrap();
        idx.rescan("f", &root).unwrap();
        let entry = &idx.load("f").unwrap()["a.txt"];
        assert!(entry.deleted);
        assert_eq!(entry.content, hash, "the tombstone lost its content hash");
    }

    /// A rename is a delete and a create in one scan, and the two entries carry
    /// the same hash — which is exactly what `detect_renames` pairs on.
    #[test]
    fn renaming_a_file_leaves_two_entries_sharing_one_hash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("old.txt"), b"unchanged bytes").unwrap();
        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();

        std::fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
        idx.rescan("f", &root).unwrap();

        let loaded = idx.load("f").unwrap();
        assert!(loaded["old.txt"].deleted);
        assert!(!loaded["new.txt"].deleted);
        assert_eq!(
            loaded["old.txt"].content, loaded["new.txt"].content,
            "the moved file's hash changed, so no rename could be detected"
        );

        let (renames, deleted, created) = crate::rename::detect(
            &[("old.txt".to_string(), loaded["old.txt"].content.clone())],
            &[("new.txt".to_string(), loaded["new.txt"].content.clone())],
        );
        assert_eq!(renames.len(), 1, "the rename was not recognised");
        assert!(deleted.is_empty() && created.is_empty());
    }

    /// A rescan must leave behind everything delta transfer needs: without the
    /// chunk map, a peer's request for "the parts I am missing" has nothing to
    /// answer from and the file syncs whole.
    #[test]
    fn a_rescan_records_the_chunk_map_delta_transfer_reads_from() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        // Comfortably more than one average chunk, or it is a single chunk and
        // proves nothing about mapping.
        let mut bytes = Vec::new();
        let mut x: u32 = 12345;
        for _ in 0..(peerbeam_chunk::AVG_CHUNK * 8) {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            bytes.push((x >> 16) as u8);
        }
        std::fs::write(root.join("big.bin"), &bytes).unwrap();

        let idx = index(dir.path());
        idx.rescan("f", &root).unwrap();

        let content = idx.load("f").unwrap()["big.bin"].content.clone();
        let map = idx
            .chunks()
            .get("f", &content)
            .expect("the rescan recorded no chunk map");
        assert!(
            map.len() > 1,
            "a multi-chunk file mapped to {} chunk(s)",
            map.len()
        );
        assert!(idx.chunks().have("f").contains(&map[0].hash));

        // And the bytes are findable through the store, which is what a delta
        // fetch actually calls.
        let loaded = idx.load("f").unwrap();
        let got = idx
            .chunks()
            .read("f", &root, &loaded, &map[1].hash)
            .expect("a recorded chunk could not be read back");
        assert_eq!(peerbeam_chunk::hash(&got), map[1].hash);
    }

    #[test]
    fn an_unreadable_file_hashes_to_empty_rather_than_failing_the_scan() {
        // One unreadable file must not strand the rest of the folder.
        assert_eq!(content_hash(Path::new("/definitely/not/here")), "");
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
