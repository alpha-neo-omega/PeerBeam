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
const MAX_REASSEMBLE: u64 = 256 * 1024 * 1024;

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
