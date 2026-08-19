//! Where the receiver looks for chunks it already has.
//!
//! Delta transfer is only worth anything if "do I already hold this chunk?" can
//! be answered cheaply, and answered across **every** file rather than just the
//! one being synced. A chunk is identified by its content hash, so a block from
//! an older version of the file, a copy under another name, or an unrelated file
//! that happens to share content are all equally good sources.
//!
//! # What is stored
//!
//! One chunk map per *content hash*, not per path. Two identical files share a
//! map, and a file that is renamed keeps its map without rewriting anything —
//! the map describes bytes, and the bytes did not move.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use peerbeam_chunk::Chunk;
use peerbeam_domain::port::AppStore;

use crate::index::IndexEntry;
use crate::manifest::SyncError;

/// The AppStore namespace chunk maps live in.
pub const NS: &str = "sync-chunks";

/// Chunk maps for a synced folder, and the lookup that makes reuse possible.
#[derive(Clone)]
pub struct ChunkStore {
    store: Arc<dyn AppStore>,
}

impl ChunkStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>) -> ChunkStore {
        ChunkStore { store }
    }

    fn namespace(folder: &str) -> String {
        format!("{NS}-{folder}")
    }

    /// Record how a file's content splits into chunks.
    ///
    /// Keyed by the file's content hash, so recording the same content twice is
    /// idempotent and a rename costs nothing.
    pub fn put(&self, folder: &str, content: &str, chunks: &[Chunk]) -> Result<(), SyncError> {
        let bytes =
            serde_json::to_vec(chunks).map_err(|e| SyncError::Serialization(e.to_string()))?;
        self.store
            .put(&Self::namespace(folder), content, &bytes)
            .map_err(|e| SyncError::Serialization(e.to_string()))
    }

    /// The chunk map for some content, if it is known.
    pub fn get(&self, folder: &str, content: &str) -> Option<Vec<Chunk>> {
        let raw = self.store.get(&Self::namespace(folder), content).ok()??;
        serde_json::from_slice(&raw).ok()
    }

    /// Every chunk hash this folder holds, from every file.
    ///
    /// **Deliberately across all files.** Restricting the answer to the file
    /// being synced would throw away the case delta transfer is best at: a large
    /// file copied under a new name, where every chunk is already on disk.
    pub fn have(&self, folder: &str) -> BTreeSet<String> {
        let Ok(rows) = self.store.list(&Self::namespace(folder)) else {
            return BTreeSet::new();
        };
        rows.into_iter()
            .filter_map(|(_, v)| serde_json::from_slice::<Vec<Chunk>>(&v).ok())
            .flatten()
            .map(|c| c.hash)
            .collect()
    }

    /// Find a chunk's bytes on disk.
    ///
    /// Searches the live index for a file whose content hash has a map
    /// containing this chunk, then reads that range. Returns `None` rather than
    /// guessing if the file has changed underneath — the caller verifies every
    /// chunk against its hash anyway, so a wrong answer here is caught, but
    /// returning one would waste a read on every attempt.
    pub fn read(
        &self,
        folder: &str,
        root: &Path,
        index: &std::collections::BTreeMap<String, IndexEntry>,
        want: &str,
    ) -> Option<Vec<u8>> {
        for entry in index.values() {
            if entry.deleted || entry.content.is_empty() {
                continue;
            }
            let Some(chunks) = self.get(folder, &entry.content) else {
                continue;
            };
            let Some(c) = chunks.iter().find(|c| c.hash == want) else {
                continue;
            };
            if let Some(bytes) = read_range(&root.join(&entry.path), c.offset, c.len) {
                // Verified here as well as by the caller: a file edited since
                // the index was written would otherwise hand back the wrong
                // bytes under a right-looking name, and the cheapest place to
                // notice is where the read happened.
                if peerbeam_chunk::hash(&bytes) == want {
                    return Some(bytes);
                }
            }
        }
        None
    }
}

/// Read `len` bytes at `offset`, or `None`.
fn read_range(path: &Path, offset: u64, len: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::port::EncryptionProvider;

    fn setup() -> (tempfile::TempDir, ChunkStore, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        std::fs::create_dir(&root).unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(peerbeam_crypto::AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[17u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (dir, ChunkStore::new(app), root)
    }

    fn data(n: usize, seed: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        let mut x = u32::from(seed) | 1;
        for _ in 0..n {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            v.push((x >> 16) as u8);
        }
        v
    }

    fn entry(path: &str, content: &str) -> IndexEntry {
        IndexEntry {
            path: path.to_string(),
            size: 0,
            modified: 0,
            content: content.to_string(),
            version: crate::version::VersionVector::new(),
            deleted: false,
        }
    }

    fn index_of(entries: &[IndexEntry]) -> std::collections::BTreeMap<String, IndexEntry> {
        entries
            .iter()
            .map(|e| (e.path.clone(), e.clone()))
            .collect()
    }

    #[test]
    fn a_recorded_map_comes_back() {
        let (_d, store, _root) = setup();
        let bytes = data(300_000, 2);
        let chunks = peerbeam_chunk::split(&bytes);
        let content = peerbeam_chunk::hash(&bytes);
        store.put("f", &content, &chunks).unwrap();
        assert_eq!(store.get("f", &content), Some(chunks));
    }

    #[test]
    fn have_reports_every_chunk_across_every_file() {
        // Restricting this to one file would throw away the case delta transfer
        // is best at: content already on disk under another name.
        let (_d, store, _root) = setup();
        let a = data(200_000, 3);
        let b = data(200_000, 4);
        store
            .put("f", &peerbeam_chunk::hash(&a), &peerbeam_chunk::split(&a))
            .unwrap();
        store
            .put("f", &peerbeam_chunk::hash(&b), &peerbeam_chunk::split(&b))
            .unwrap();

        let have = store.have("f");
        for c in peerbeam_chunk::split(&a) {
            assert!(
                have.contains(&c.hash),
                "a chunk of the first file is missing"
            );
        }
        for c in peerbeam_chunk::split(&b) {
            assert!(
                have.contains(&c.hash),
                "a chunk of the second file is missing"
            );
        }
    }

    #[test]
    fn a_chunk_is_read_back_from_the_file_that_holds_it() {
        let (_d, store, root) = setup();
        let bytes = data(300_000, 5);
        std::fs::write(root.join("a.bin"), &bytes).unwrap();
        let chunks = peerbeam_chunk::split(&bytes);
        let content = peerbeam_chunk::hash(&bytes);
        store.put("f", &content, &chunks).unwrap();

        let idx = index_of(&[entry("a.bin", &content)]);
        let wanted = &chunks[1];
        let got = store.read("f", &root, &idx, &wanted.hash).expect("chunk");
        assert_eq!(peerbeam_chunk::hash(&got), wanted.hash);
        assert_eq!(got.len(), wanted.len as usize);
    }

    /// **Reuse across files is the point.** A chunk requested while syncing one
    /// file must be found in a completely different one.
    #[test]
    fn a_chunk_is_found_in_an_unrelated_file() {
        let (_d, store, root) = setup();
        let shared = data(400_000, 6);
        std::fs::write(root.join("original.bin"), &shared).unwrap();
        let chunks = peerbeam_chunk::split(&shared);
        let content = peerbeam_chunk::hash(&shared);
        store.put("f", &content, &chunks).unwrap();

        // The index knows only `original.bin`; we ask while syncing something
        // else entirely.
        let idx = index_of(&[entry("original.bin", &content)]);
        let got = store.read("f", &root, &idx, &chunks[2].hash);
        assert!(got.is_some(), "a chunk on disk was not found");
    }

    /// A file edited since the index was written must not hand back wrong bytes
    /// under a right-looking name.
    #[test]
    fn a_stale_index_entry_yields_nothing_rather_than_wrong_bytes() {
        let (_d, store, root) = setup();
        let bytes = data(300_000, 7);
        std::fs::write(root.join("a.bin"), &bytes).unwrap();
        let chunks = peerbeam_chunk::split(&bytes);
        let content = peerbeam_chunk::hash(&bytes);
        store.put("f", &content, &chunks).unwrap();
        let idx = index_of(&[entry("a.bin", &content)]);

        // Rewrite the file with different content of the same length.
        std::fs::write(root.join("a.bin"), data(300_000, 8)).unwrap();
        assert!(
            store.read("f", &root, &idx, &chunks[1].hash).is_none(),
            "stale bytes were returned as if they matched"
        );
    }

    #[test]
    fn a_deleted_entry_is_not_searched() {
        let (_d, store, root) = setup();
        let bytes = data(200_000, 9);
        let chunks = peerbeam_chunk::split(&bytes);
        let content = peerbeam_chunk::hash(&bytes);
        store.put("f", &content, &chunks).unwrap();
        let mut e = entry("gone.bin", &content);
        e.deleted = true;
        assert!(store
            .read("f", &root, &index_of(&[e]), &chunks[0].hash)
            .is_none());
    }

    #[test]
    fn an_unknown_chunk_is_not_found() {
        let (_d, store, root) = setup();
        assert!(store.read("f", &root, &index_of(&[]), "deadbeef").is_none());
        assert!(store.get("f", "deadbeef").is_none());
        assert!(store.have("f").is_empty());
    }
}
