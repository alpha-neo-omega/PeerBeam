//! A whole delta exchange, end to end: does editing one line of a large file
//! actually cost one line on the wire?
//!
//! The unit tests prove each piece in isolation. This proves they are connected
//! — the part that was missing while the modules existed but nothing called
//! them.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use peerbeam_domain::entity::{Permission, PermissionSet, TrustRecord};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{ChannelId, MessageHandler};
use peerbeam_sync::manifest_wire::{ChunkData, ChunkMapRequest, ChunkMapResponse, ChunkRequest};
use peerbeam_sync::{ChunkMap, SyncHandler};

/// A trust store granting exactly what a test asks for.
struct Grants(PermissionSet);

impl TrustStore for Grants {
    fn record(&self, _r: TrustRecord) -> peerbeam_domain::error::Result<()> {
        Ok(())
    }
    fn lookup(&self, _d: &DeviceId) -> peerbeam_domain::error::Result<Option<TrustRecord>> {
        Ok(None)
    }
    fn is_trusted(&self, _d: &DeviceId) -> bool {
        true
    }
    fn may(&self, _d: &DeviceId, p: Permission) -> bool {
        self.0.grants(p)
    }
}

/// Browse **and** Files, spelled out.
///
/// Not `granted_on_approval()`: that is deliberately the five permissions that
/// existed when the set was introduced, and Browse came later. Using it here
/// would test a peer that cannot browse — which is a real case, but not this
/// one, and it took two failing tests to notice.
/// A share-relative path: the share's own directory name, then the file.
///
/// `Shares::resolve` matches the first segment against a share root's name, so
/// a bare `"big.bin"` resolves to nothing — which is what "the file mapped to
/// one chunk" was really telling me.
fn shared(root: &std::path::Path, name: &str) -> String {
    format!("{}/{name}", root.file_name().unwrap().to_string_lossy())
}

fn full_access() -> PermissionSet {
    PermissionSet::granted_on_approval()
        .set(Permission::Browse, true)
        .set(Permission::Files, true)
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

struct Served {
    handler: Arc<SyncHandler>,
    maps: Arc<Mutex<Vec<ChunkMapResponse>>>,
    chunks: Arc<Mutex<Vec<ChunkData>>>,
}

fn serve(root: &std::path::Path, perms: PermissionSet) -> Served {
    let maps = Arc::new(Mutex::new(Vec::new()));
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let m = maps.clone();
    let c = chunks.clone();
    let shares = peerbeam_browse::Shares::new(&[root.to_string_lossy().into_owned()]);
    let (handler, peer): (Arc<SyncHandler>, Arc<OnceLock<DeviceId>>) = SyncHandler::with_chunks(
        shares,
        Arc::new(Grants(perms)),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Arc::new(move |r| m.lock().unwrap().push(r)),
        Arc::new(move |d| c.lock().unwrap().push(d)),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
    );
    let _ = peer.set(DeviceId::from("pb-asker"));
    Served {
        handler,
        maps,
        chunks,
    }
}

async fn ask_map(s: &Served, path: &str) -> ChunkMapResponse {
    let req = ChunkMapRequest {
        path: path.to_string(),
    };
    s.handler
        .handle(req.to_frame(ChannelId::new(1)).unwrap())
        .await
        .unwrap();
    s.maps.lock().unwrap().pop().expect("no chunk map answered")
}

async fn ask_chunks(s: &Served, path: &str, hashes: &[String]) -> Vec<ChunkData> {
    let req = ChunkRequest {
        path: path.to_string(),
        hashes: hashes.to_vec(),
    };
    s.handler
        .handle(req.to_frame(ChannelId::new(1)).unwrap())
        .await
        .unwrap();
    std::mem::take(&mut *s.chunks.lock().unwrap())
}

/// **The claim the changelog makes.** Edit a little of a large file and only a
/// little crosses the wire — and what arrives rebuilds the file exactly.
#[tokio::test]
async fn a_small_edit_costs_a_small_transfer_and_rebuilds_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let original = data(2_000_000, 5);
    let mut edited = original.clone();
    for b in edited.iter_mut().skip(900_000).take(64) {
        *b ^= 0xFF;
    }
    std::fs::write(root.join("big.bin"), &edited).unwrap();

    let served = serve(root, full_access());
    let path = shared(root, "big.bin");
    let answer = ask_map(&served, &path).await;
    assert!(!answer.denied);
    assert!(answer.chunks.len() > 1, "the file mapped to one chunk");

    let have: BTreeSet<String> = peerbeam_chunk::split(&original)
        .into_iter()
        .map(|c| c.hash)
        .collect();
    let map = ChunkMap {
        path: path.clone(),
        chunks: answer.chunks.clone(),
    };
    let need = peerbeam_sync::plan_delta(&map, &have);

    assert!(!need.is_satisfied(), "an edited file needed nothing");
    assert!(
        need.fetch_bytes < original.len() as u64 / 10,
        "a 64-byte edit asked for {} of {} bytes",
        need.fetch_bytes,
        original.len()
    );

    // Printed so the saving is a number in the log rather than a claim in a
    // comment: `cargo test -- --nocapture` shows what a 64-byte edit costs.
    println!(
        "delta: fetched {} of {} bytes ({:.2}%), reused {}",
        need.fetch_bytes,
        edited.len(),
        100.0 * need.fetch_bytes as f64 / edited.len() as f64,
        need.reuse_bytes
    );

    let fetched = ask_chunks(&served, &path, &need.fetch).await;
    assert_eq!(fetched.len(), need.fetch.len(), "the peer withheld a chunk");

    let mut pool: std::collections::HashMap<String, Vec<u8>> =
        fetched.into_iter().map(|d| (d.hash, d.bytes)).collect();
    for c in peerbeam_chunk::split(&original) {
        let s = c.offset as usize;
        pool.entry(c.hash.clone())
            .or_insert_with(|| original[s..s + c.len as usize].to_vec());
    }

    let rebuilt = peerbeam_sync::reassemble(&map, |h| pool.get(h).cloned())
        .expect("the file could not be rebuilt");
    assert_eq!(rebuilt, edited, "the rebuilt file is not the sender's file");
}

/// Describing a file by chunks reveals how large its pieces are and where they
/// repeat — information about content, so it takes the same grant as the bytes.
/// Browse alone is not enough.
#[tokio::test]
async fn a_peer_without_the_files_permission_is_refused_a_chunk_map() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.bin"), data(200_000, 6)).unwrap();

    let served = serve(
        dir.path(),
        PermissionSet::none().set(Permission::Browse, true),
    );
    let answer = ask_map(&served, &shared(dir.path(), "secret.bin")).await;
    assert!(answer.denied, "a browse-only peer was given a chunk map");
    assert!(answer.chunks.is_empty());

    let got = ask_chunks(
        &served,
        &shared(dir.path(), "secret.bin"),
        &["whatever".to_string()],
    )
    .await;
    assert!(got.is_empty(), "a browse-only peer received chunk bytes");
}

/// A request naming one path must never be answered with another file's bytes,
/// or the containment check would mean nothing.
#[tokio::test]
async fn chunks_are_served_only_from_the_file_that_was_asked_about() {
    let dir = tempfile::tempdir().unwrap();
    let a = data(200_000, 7);
    let b = data(200_000, 8);
    std::fs::write(dir.path().join("a.bin"), &a).unwrap();
    std::fs::write(dir.path().join("b.bin"), &b).unwrap();

    let served = serve(dir.path(), full_access());
    let b_hashes: Vec<String> = peerbeam_chunk::split(&b)
        .into_iter()
        .map(|c| c.hash)
        .collect();

    let got = ask_chunks(&served, &shared(dir.path(), "a.bin"), &b_hashes).await;
    assert!(
        got.is_empty(),
        "another file's chunks were served through a request for a.bin"
    );
}

#[tokio::test]
async fn a_request_for_a_missing_file_answers_empty_rather_than_erroring() {
    let dir = tempfile::tempdir().unwrap();
    let served = serve(dir.path(), full_access());
    let answer = ask_map(&served, &shared(dir.path(), "nope.bin")).await;
    assert!(answer.chunks.is_empty());
    assert!(!answer.denied, "a missing file reported as denied");
}
