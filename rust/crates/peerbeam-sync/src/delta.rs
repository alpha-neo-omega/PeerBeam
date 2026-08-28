//! Sending only the parts of a file that changed.
//!
//! Both sides split a file into content-defined chunks ([`peerbeam_chunk`]) and
//! exchange the chunk *hashes*, which are tiny. The receiver then asks for only
//! the chunks it does not already hold — and it may already hold them from
//! **anywhere**: an earlier version of this file, a copy under another name, or
//! a different file that happens to share content. Identity is the content
//! hash, so where a chunk came from never matters.
//!
//! # What this is not
//!
//! It is not compression and not a diff format. Nothing is computed *between*
//! two versions; each side describes what it has, and the difference falls out.
//! That is what lets a receiver reuse chunks from files the sender has never
//! heard of.

use std::collections::BTreeSet;

use peerbeam_chunk::Chunk;
use serde::{Deserialize, Serialize};

/// How a file is built out of chunks — the small thing sent instead of bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMap {
    /// Path relative to the synced folder.
    pub path: String,
    /// Every chunk, in file order. Order matters: this is the reassembly plan.
    pub chunks: Vec<Chunk>,
}

impl ChunkMap {
    /// Total size of the file this map describes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.chunks.iter().map(|c| u64::from(c.len)).sum()
    }
}

/// What a receiver needs in order to build the sender's version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Need {
    /// Chunk hashes to request, in file order and without repeats.
    pub fetch: Vec<String>,
    /// Bytes that must cross the wire.
    pub fetch_bytes: u64,
    /// Bytes the receiver already holds and will reuse.
    pub reuse_bytes: u64,
}

impl Need {
    /// Whether the file is already identical.
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.fetch.is_empty()
    }
}

/// Work out which chunks the receiver must fetch.
///
/// `have` is every chunk hash the receiver already holds, from any file.
///
/// **A repeated chunk is requested once.** A file containing the same block a
/// thousand times costs one transfer, and asking again for something already in
/// flight would be pure waste.
#[must_use]
pub fn plan(want: &ChunkMap, have: &BTreeSet<String>) -> Need {
    let mut need = Need::default();
    let mut asked: BTreeSet<&str> = BTreeSet::new();
    for chunk in &want.chunks {
        if have.contains(&chunk.hash) {
            need.reuse_bytes += u64::from(chunk.len);
            continue;
        }
        if asked.insert(chunk.hash.as_str()) {
            need.fetch.push(chunk.hash.clone());
            need.fetch_bytes += u64::from(chunk.len);
        } else {
            // Already being fetched; it will be written to both places.
            need.reuse_bytes += u64::from(chunk.len);
        }
    }
    need
}

/// Rebuild a file's bytes from chunks.
///
/// `supply` answers with the bytes for a hash, from wherever the receiver has
/// them. Returns `None` if any chunk is missing or its content does not match
/// its hash.
///
/// **Every chunk is verified before it is used.** A chunk arrives identified
/// only by a hash, and a peer that sent the wrong bytes under a right-looking
/// name would otherwise have its content written into a file the user believes
/// is a faithful copy. Checking is cheap; not checking makes the hash a label
/// rather than a guarantee.
/// The largest reassembly this code will attempt, matching the chunkable-file
/// ceiling used by `index` and `handler`.
///
/// A bound rather than a hint: it is checked against a figure the peer chose.
pub const MAX_REASSEMBLE: u64 = 256 * 1024 * 1024;

/// Whether a chunk map is small enough to reassemble — asked **before**
/// fetching, which is the only place the answer is worth anything.
///
/// [`reassemble`] enforces the same ceiling, but it runs after every chunk has
/// been downloaded and buffered, so on the production path the guard was dead
/// for the accumulation that actually matters: a caller fetched the whole
/// declared map into memory and only then asked whether it was allowed to. On a
/// 32-bit ABI — `armeabi-v7a`, which ships — an allocation failure is not an
/// `Err` but `handle_alloc_error`, so the process aborts before the caller's
/// whole-file fallback can run. `Need::fetch_bytes` exists for exactly this
/// question and was computed and never read.
#[must_use]
pub fn fits_in_memory(map: &ChunkMap) -> bool {
    map.total_bytes() <= MAX_REASSEMBLE
}

/// Write the file `map` describes into `out`, verifying every chunk.
///
/// **Streaming: never holds more than one chunk.** [`reassemble`] builds the
/// whole file in memory, which is why it needs [`MAX_REASSEMBLE`] and why any
/// file above that ceiling could not be synced at all — the whole-file fallback
/// that refusal handed it to did nothing on either caller. Writing as we go
/// removes both the ceiling and the allocation, and honours the workspace rule
/// that no file is ever fully resident.
///
/// Returns the bytes written, or `None` if a chunk was missing, the wrong
/// length, or did not hash to the name it was supplied under. **Every chunk is
/// verified before it is written**, for the reason [`reassemble`] gives: a
/// chunk arrives identified only by a hash, and a peer that sent the wrong
/// bytes under a right-looking name would otherwise have its content written
/// into a file the user believes is a faithful copy.
///
/// A refusal can leave a partially written `out`. That is the caller's to
/// handle, and the reason both callers write to a staging file and rename only
/// on success rather than over the user's copy.
pub fn reassemble_into<W: std::io::Write>(
    map: &ChunkMap,
    mut supply: impl FnMut(&str) -> Option<Vec<u8>>,
    out: &mut W,
) -> Option<u64> {
    // No ceiling here, deliberately. `reassemble` needs one because it
    // accumulates; this does not, so the only limit that applies is disk space,
    // which the write below reports honestly.
    let mut written = 0u64;
    for chunk in &map.chunks {
        let bytes = supply(&chunk.hash)?;
        if bytes.len() != chunk.len as usize {
            return None;
        }
        if peerbeam_chunk::hash(&bytes) != chunk.hash {
            return None;
        }
        out.write_all(&bytes).ok()?;
        written += bytes.len() as u64;
    }
    Some(written)
}

/// Fetch one file's chunks and write it into `dest`, streaming.
///
/// This is the whole receive half of a delta sync, in one place because both
/// callers had their own copy of it and both copies had the same two defects.
///
/// # What it fixes
///
/// **Files larger than [`MAX_REASSEMBLE`] never synced.** Both callers refused
/// an over-large chunk map and handed the file to a whole-file fallback whose
/// only handler emitted an event nothing consumes — so the file silently never
/// arrived, while the sync result counted it as being fetched. Chunks are now
/// fetched a window at a time and written as they arrive, so a file's size is
/// the filesystem's business rather than memory's.
///
/// **A failed sync destroyed the user's copy.** Both callers wrote straight to
/// the destination, so a chunk that did not verify, a disconnect or a full disk
/// left the file truncated — having already replaced one that was fine. This
/// writes to a staging file beside the destination and renames only once the
/// whole file is written and flushed, which is what the transfer path has
/// always done.
///
/// # Arguments
///
/// - `have` — chunk hashes already held locally, so they are never requested.
/// - `local` — reads one of those chunks back from disk.
/// - `fetch` — asks the peer for a batch of chunks. Called once per window.
///
/// Returns the bytes that were reused rather than transferred, or `None` if the
/// file could not be built — in which case `dest` is left exactly as it was.
pub async fn fetch_streamed<Fetch, Fut>(
    map: &ChunkMap,
    dest: &std::path::Path,
    have: &BTreeSet<String>,
    mut local: impl FnMut(&str) -> Option<Vec<u8>>,
    mut fetch: Fetch,
) -> Option<u64>
where
    Fetch: FnMut(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = std::collections::HashMap<String, Vec<u8>>>,
{
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let part = staging_path(dest);
    let file = std::fs::File::create(&part).ok()?;
    let mut out = std::io::BufWriter::new(file);

    let mut reuse = 0u64;
    let mut built = true;
    // One window of chunks resident at a time. A chunk is at most
    // `peerbeam_chunk::MAX_CHUNK` (256 KiB) and averages 64 KiB, so a window
    // costs a few megabytes however large the file is.
    for window in map.chunks.chunks(FETCH_WINDOW) {
        let missing: Vec<String> = window
            .iter()
            .filter(|c| !have.contains(&c.hash))
            .map(|c| c.hash.clone())
            .collect();
        let fetched = if missing.is_empty() {
            std::collections::HashMap::new()
        } else {
            fetch(missing).await
        };

        let slice = ChunkMap {
            path: map.path.clone(),
            chunks: window.to_vec(),
        };
        let wrote = reassemble_into(
            &slice,
            |h| match fetched.get(h) {
                Some(b) => Some(b.clone()),
                None => {
                    let bytes = local(h);
                    if let Some(b) = &bytes {
                        reuse += b.len() as u64;
                    }
                    bytes
                }
            },
            &mut out,
        );
        if wrote.is_none() {
            built = false;
            break;
        }
    }

    // Flushed before the rename, or the rename publishes a file whose tail is
    // still in this buffer.
    let flushed = std::io::Write::flush(&mut out).is_ok();
    drop(out);
    if !built || !flushed || std::fs::rename(&part, dest).is_err() {
        // The destination is untouched on every failure path, which is the
        // point of staging. The staging file is not left behind to be mistaken
        // for a synced one.
        let _ = std::fs::remove_file(&part);
        return None;
    }
    Some(reuse)
}

/// How many chunks are fetched, and held, at once.
const FETCH_WINDOW: usize = 64;

/// Where a streamed sync writes before it may replace the real file.
///
/// A sibling of the destination rather than a temp directory, so the rename
/// that publishes it cannot cross a filesystem: `fs::rename` fails across
/// devices rather than falling back to a copy.
fn staging_path(dest: &std::path::Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".pbsync");
    dest.with_file_name(name)
}

/// [`reassemble_into`], accumulating into memory.
///
/// Kept for callers that genuinely want the bytes in hand, and bounded by
/// [`MAX_REASSEMBLE`] because it accumulates. The sync path uses
/// [`reassemble_into`]: it must handle files larger than any ceiling worth
/// setting.
pub fn reassemble(
    map: &ChunkMap,
    mut supply: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    // **The declared size is refused before it is trusted, and never used to
    // allocate.**
    //
    // `total_bytes()` sums `u32` chunk lengths off the wire into a `u64`, and
    // the chunk-map decoder bounds neither the count nor the lengths. A peer
    // can therefore declare terabytes in a frame of a few hundred bytes;
    // `Vec::with_capacity` on that figure aborts the process on allocation
    // failure, which is a remote crash for the cost of one message — and it
    // happens before a single chunk has been supplied or verified.
    //
    // The same ceiling the rest of this crate uses for a chunkable file: a
    // reassembly larger than that is not a file this code would ever have
    // asked for. Capacity is left to the `extend_from_slice` growth below,
    // which can only ever allocate what has actually been supplied and hashed.
    if map.total_bytes() > MAX_REASSEMBLE {
        return None;
    }
    let mut out = Vec::new();
    for chunk in &map.chunks {
        let bytes = supply(&chunk.hash)?;
        if bytes.len() != chunk.len as usize {
            return None;
        }
        if peerbeam_chunk::hash(&bytes) != chunk.hash {
            return None;
        }
        out.extend_from_slice(&bytes);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, seed: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        let mut x = u32::from(seed) | 1;
        for _ in 0..n {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            v.push((x >> 16) as u8);
        }
        v
    }

    fn map_of(path: &str, bytes: &[u8]) -> ChunkMap {
        ChunkMap {
            path: path.to_string(),
            chunks: peerbeam_chunk::split(bytes),
        }
    }

    fn store(bytes: &[u8]) -> std::collections::HashMap<String, Vec<u8>> {
        peerbeam_chunk::split(bytes)
            .into_iter()
            .map(|c| {
                let s = c.offset as usize;
                (c.hash, bytes[s..s + c.len as usize].to_vec())
            })
            .collect()
    }

    /// **The bug this function exists for.** A file over [`MAX_REASSEMBLE`]
    /// could not be synced at all: `reassemble` refused it, and the whole-file
    /// fallback that refusal handed it to did nothing. Streaming has no
    /// ceiling, so the same content writes correctly.
    #[test]
    fn a_map_too_large_to_hold_in_memory_still_writes() {
        let body = data(3 * peerbeam_chunk::MIN_CHUNK, 7);
        let map = map_of("big", &body);
        let have = store(&body);

        // Declared over the ceiling without allocating it: the check
        // `reassemble` makes is against the map's own arithmetic.
        let mut oversized = map.clone();
        let filler = Chunk {
            offset: 0,
            len: u32::MAX,
            hash: "not-supplied".into(),
        };
        for _ in 0..((MAX_REASSEMBLE / u64::from(u32::MAX)) + 1) {
            oversized.chunks.push(filler.clone());
        }
        assert!(
            oversized.total_bytes() > MAX_REASSEMBLE,
            "the fixture must exceed the in-memory ceiling"
        );
        assert!(
            reassemble(&oversized, |h| have.get(h).cloned()).is_none(),
            "the in-memory path refuses it, which is what stranded the file"
        );

        let mut out = Vec::new();
        let n = reassemble_into(&map, |h| have.get(h).cloned(), &mut out)
            .expect("a streamed reassembly has no memory ceiling to hit");
        assert_eq!(n, body.len() as u64);
        assert_eq!(out, body);
    }

    /// Every chunk is verified before it reaches the file. Deleting either
    /// check in [`reassemble_into`] must fail this.
    #[test]
    fn a_chunk_that_does_not_match_its_hash_is_never_written() {
        let body = data(3 * peerbeam_chunk::MIN_CHUNK, 11);
        let map = map_of("f", &body);
        let mut have = store(&body);
        // Same length, different bytes — so only the hash check can catch it.
        let victim = map.chunks[1].hash.clone();
        let good = have.get(&victim).cloned().unwrap();
        have.insert(victim, vec![0xAA; good.len()]);

        let mut out = Vec::new();
        assert!(
            reassemble_into(&map, |h| have.get(h).cloned(), &mut out).is_none(),
            "a chunk whose bytes do not hash to its name must be refused"
        );
        assert!(
            out.len() < body.len(),
            "it stopped at the bad chunk rather than writing the whole file"
        );
    }

    /// A chunk the supplier does not have stops the reassembly rather than
    /// silently producing a short file.
    #[test]
    fn a_missing_chunk_refuses_rather_than_truncating() {
        let body = data(3 * peerbeam_chunk::MIN_CHUNK, 13);
        let map = map_of("f", &body);
        let mut have = store(&body);
        have.remove(&map.chunks[1].hash);

        let mut out = Vec::new();
        assert!(reassemble_into(&map, |h| have.get(h).cloned(), &mut out).is_none());
    }

    /// The streamed and buffered paths agree, so switching a caller over
    /// cannot change what lands on disk.
    #[test]
    fn streaming_and_buffering_produce_the_same_bytes() {
        let body = data(6 * peerbeam_chunk::MIN_CHUNK, 3);
        let map = map_of("f", &body);
        let have = store(&body);

        let buffered = reassemble(&map, |h| have.get(h).cloned()).unwrap();
        let mut streamed = Vec::new();
        reassemble_into(&map, |h| have.get(h).cloned(), &mut streamed).unwrap();
        assert_eq!(buffered, streamed);
        assert_eq!(streamed, body);
    }

    // ── fetch_streamed: the whole receive half ──────────────────

    /// Nothing held locally, everything fetched — the ordinary first sync of a
    /// new file, and the case the old code could not do above 256 MiB.
    #[tokio::test]
    async fn a_file_is_fetched_and_written_whole() {
        let body = data(8 * peerbeam_chunk::MIN_CHUNK, 21);
        let map = map_of("f", &body);
        let pool = store(&body);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested").join("f.bin");

        let reuse = fetch_streamed(
            &map,
            &dest,
            &BTreeSet::new(),
            |_| None,
            |hashes| {
                let pool = pool.clone();
                async move {
                    hashes
                        .into_iter()
                        .filter_map(|h| pool.get(&h).map(|b| (h, b.clone())))
                        .collect()
                }
            },
        )
        .await
        .expect("a first sync writes the file");

        assert_eq!(reuse, 0, "nothing was held locally");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(
            !staging_path(&dest).exists(),
            "the staging file is renamed away, not left behind"
        );
    }

    /// A chunk already on disk is never asked for, and is counted as reused.
    #[tokio::test]
    async fn chunks_already_held_are_not_requested() {
        let body = data(8 * peerbeam_chunk::MIN_CHUNK, 22);
        let map = map_of("f", &body);
        let pool = store(&body);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.bin");

        let have: BTreeSet<String> = map.chunks.iter().map(|c| c.hash.clone()).collect();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = asked.clone();

        let reuse = fetch_streamed(
            &map,
            &dest,
            &have,
            |h| pool.get(h).cloned(),
            |hashes| {
                seen.lock().unwrap().extend(hashes);
                async move { std::collections::HashMap::new() }
            },
        )
        .await
        .expect("everything was already held");

        assert!(
            asked.lock().unwrap().is_empty(),
            "a chunk already on disk must not be requested"
        );
        assert_eq!(reuse, body.len() as u64, "all of it was reuse");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    /// **The data-loss fix.** A peer that answers with bytes that do not match
    /// the hash must not leave the user's existing copy damaged. The old code
    /// wrote straight to the destination, so a failure part-way through
    /// replaced a good file with a truncated one.
    #[tokio::test]
    async fn a_bad_chunk_leaves_the_existing_file_untouched() {
        let body = data(8 * peerbeam_chunk::MIN_CHUNK, 23);
        let map = map_of("f", &body);
        let mut pool = store(&body);
        assert!(
            map.chunks.len() >= 2,
            "the fixture needs a chunk after the first, so the failure happens \
             once bytes are already staged"
        );
        // Same length, wrong bytes: only the hash check catches it. The LAST
        // chunk, so the write has already got somewhere before it fails.
        let victim = map.chunks[map.chunks.len() - 1].hash.clone();
        let len = pool.get(&victim).unwrap().len();
        pool.insert(victim, vec![0x5A; len]);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.bin");
        let previous = b"the copy the user already had".to_vec();
        std::fs::write(&dest, &previous).unwrap();

        let out = fetch_streamed(
            &map,
            &dest,
            &BTreeSet::new(),
            |_| None,
            |hashes| {
                let pool = pool.clone();
                async move {
                    hashes
                        .into_iter()
                        .filter_map(|h| pool.get(&h).map(|b| (h, b.clone())))
                        .collect()
                }
            },
        )
        .await;

        assert!(
            out.is_none(),
            "a chunk that does not verify fails the fetch"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            previous,
            "the user's file must survive a failed sync"
        );
        assert!(
            !staging_path(&dest).exists(),
            "a failed staging file is cleaned up, not left to be mistaken for a sync"
        );
    }

    /// A peer that simply stops answering is the same story: no partial file.
    #[tokio::test]
    async fn a_peer_that_stops_answering_leaves_no_partial_file() {
        let body = data(8 * peerbeam_chunk::MIN_CHUNK, 24);
        let map = map_of("f", &body);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.bin");

        let out = fetch_streamed(
            &map,
            &dest,
            &BTreeSet::new(),
            |_| None,
            |_| async move { std::collections::HashMap::new() },
        )
        .await;

        assert!(out.is_none());
        assert!(!dest.exists(), "nothing is published from a failed fetch");
        assert!(!staging_path(&dest).exists());
    }

    fn hashes(bytes: &[u8]) -> BTreeSet<String> {
        peerbeam_chunk::split(bytes)
            .into_iter()
            .map(|c| c.hash)
            .collect()
    }

    #[test]
    fn an_identical_file_needs_nothing() {
        let d = data(500_000, 2);
        let need = plan(&map_of("a", &d), &hashes(&d));
        assert!(need.is_satisfied());
        assert_eq!(need.fetch_bytes, 0);
        assert_eq!(need.reuse_bytes, d.len() as u64);
    }

    #[test]
    fn a_receiver_with_nothing_fetches_everything() {
        let d = data(300_000, 3);
        let need = plan(&map_of("a", &d), &BTreeSet::new());
        assert_eq!(need.fetch_bytes, d.len() as u64);
        assert_eq!(need.reuse_bytes, 0);
    }

    /// **The whole point.** A small edit to a large file must cost a small
    /// transfer, not a large one.
    #[test]
    fn a_small_edit_to_a_large_file_transfers_only_a_little() {
        let original = data(2_000_000, 5);
        let mut edited = original.clone();
        for b in edited.iter_mut().skip(1_000_000).take(32) {
            *b ^= 0xFF;
        }
        let need = plan(&map_of("a", &edited), &hashes(&original));
        assert!(
            need.fetch_bytes < original.len() as u64 / 10,
            "a 32-byte edit cost {} of {} bytes",
            need.fetch_bytes,
            original.len()
        );
        assert!(need.fetch_bytes > 0, "the edit was not detected at all");
    }

    /// Chunks are reused from **anywhere**, including a file the sender has
    /// never heard of. Identity is content, so provenance is irrelevant.
    #[test]
    fn chunks_are_reused_from_an_unrelated_file() {
        let shared = data(1_000_000, 9);
        let mut sender_file = shared.clone();
        sender_file.extend_from_slice(&data(50_000, 4));

        // The receiver holds `shared` under some other name entirely.
        let need = plan(&map_of("theirs", &sender_file), &hashes(&shared));
        assert!(
            need.reuse_bytes > shared.len() as u64 / 2,
            "only {} bytes were reused from an identical prefix",
            need.reuse_bytes
        );
    }

    #[test]
    fn a_repeated_chunk_is_requested_once() {
        // A file containing the same block many times costs one transfer.
        let block = data(peerbeam_chunk::AVG_CHUNK * 4, 6);
        let mut doubled = block.clone();
        doubled.extend_from_slice(&block);
        let map = map_of("a", &doubled);
        let need = plan(&map, &BTreeSet::new());

        let distinct: BTreeSet<&String> = need.fetch.iter().collect();
        assert_eq!(
            distinct.len(),
            need.fetch.len(),
            "the same chunk was requested more than once"
        );
    }

    #[test]
    fn reassembling_reproduces_the_original_bytes() {
        let d = data(400_000, 7);
        let map = map_of("a", &d);
        let have = store(&d);
        let rebuilt = reassemble(&map, |h| have.get(h).cloned()).expect("rebuild");
        assert_eq!(rebuilt, d);
    }

    #[test]
    fn a_missing_chunk_fails_rather_than_producing_a_short_file() {
        let d = data(400_000, 8);
        let map = map_of("a", &d);
        assert!(
            reassemble(&map, |_| None).is_none(),
            "a file was produced from nothing"
        );
    }

    /// **A hash is a guarantee, not a label.** A peer that sends wrong bytes
    /// under a right-looking name must not have them written into a file the
    /// user believes is a faithful copy.
    #[test]
    fn bytes_that_do_not_match_their_hash_are_refused() {
        let d = data(400_000, 10);
        let map = map_of("a", &d);
        let have = store(&d);
        let first = map.chunks[0].hash.clone();

        let rebuilt = reassemble(&map, |h| {
            if h == first {
                // Right length, wrong content.
                Some(vec![0xEE; have[h].len()])
            } else {
                have.get(h).cloned()
            }
        });
        assert!(rebuilt.is_none(), "forged chunk content was accepted");
    }

    #[test]
    fn a_chunk_of_the_wrong_length_is_refused() {
        let d = data(200_000, 11);
        let map = map_of("a", &d);
        let have = store(&d);
        let first = map.chunks[0].hash.clone();
        let rebuilt = reassemble(&map, |h| {
            if h == first {
                Some(vec![0u8; 3])
            } else {
                have.get(h).cloned()
            }
        });
        assert!(rebuilt.is_none());
    }

    #[test]
    fn an_empty_file_reassembles_to_nothing() {
        let map = ChunkMap {
            path: "empty".into(),
            chunks: Vec::new(),
        };
        assert_eq!(reassemble(&map, |_| None), Some(Vec::new()));
        assert_eq!(map.total_bytes(), 0);
    }

    #[test]
    fn a_chunk_map_round_trips_on_the_wire() {
        let map = map_of("a", &data(200_000, 12));
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(serde_json::from_str::<ChunkMap>(&json).unwrap(), map);
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;
    use peerbeam_chunk::Chunk;

    /// **A declared size is refused, never allocated.**
    ///
    /// The chunk-map decoder bounds neither the count nor the lengths, so a
    /// peer can declare terabytes in a frame of a few hundred bytes. Passing
    /// that figure to `Vec::with_capacity` aborts the process on allocation
    /// failure — a remote crash for the price of one message, before a single
    /// chunk has been supplied or verified.
    #[test]
    fn a_map_declaring_more_than_the_ceiling_is_refused_not_allocated() {
        // Four chunks of 1 GiB each: cheap to describe, impossible to hold.
        let chunks: Vec<Chunk> = (0..4)
            .map(|i| Chunk {
                hash: format!("{i:064x}"),
                len: 1024 * 1024 * 1024,
                offset: i * 1024 * 1024 * 1024,
            })
            .collect();
        let map = ChunkMap {
            path: "big.bin".into(),
            chunks,
        };
        assert!(map.total_bytes() > MAX_REASSEMBLE);

        // Refused without ever asking for a chunk: the supply closure panics if
        // it is called, so reaching for the bytes fails the test rather than
        // the machine.
        let out = reassemble(&map, |_| panic!("must refuse before supplying"));
        assert!(out.is_none(), "an oversized map was accepted");
    }

    /// **The same answer, before a byte is fetched.** `reassemble`'s refusal
    /// arrives after every chunk has been downloaded and buffered, so on the
    /// production delta path it protected nothing that mattered: the caller had
    /// already spent the memory by the time it was told the map was too big. On
    /// `armeabi-v7a` — a shipped ABI — that spend is an abort rather than an
    /// error, so the caller's whole-file fallback never ran either.
    #[test]
    fn an_oversized_map_is_refusable_before_any_chunk_is_fetched() {
        let chunks: Vec<Chunk> = (0..4)
            .map(|i| Chunk {
                hash: format!("{i:064x}"),
                len: 1024 * 1024 * 1024,
                offset: i * 1024 * 1024 * 1024,
            })
            .collect();
        let map = ChunkMap {
            path: "big.bin".into(),
            chunks,
        };

        assert!(
            !fits_in_memory(&map),
            "a map four times the ceiling must be refusable up front"
        );
        // And the two answers agree, so a caller that asks early gets exactly
        // what `reassemble` would have told it late.
        assert!(reassemble(&map, |_| panic!("must refuse before supplying")).is_none());
    }

    /// A map inside the ceiling still reassembles, so the bound is a bound and
    /// not a wall.
    #[test]
    fn an_ordinary_map_still_reassembles() {
        let body = b"hello".to_vec();
        let hash = peerbeam_chunk::hash(&body);
        let map = ChunkMap {
            path: "small.txt".into(),
            chunks: vec![Chunk {
                hash: hash.clone(),
                len: body.len() as u32,
                offset: 0,
            }],
        };
        let out = reassemble(&map, |h| (h == hash).then(|| body.clone()));
        assert_eq!(out.as_deref(), Some(b"hello".as_slice()));
    }
}
