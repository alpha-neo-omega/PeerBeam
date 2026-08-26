//! The Sync channel's MessageHandler: answer manifests, honour file requests.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_browse::Shares;
use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::manifest::{
    FileEntry, FileRequest, Manifest, ManifestRequest, MAX_FILES, MSG_CHUNKMAP,
    MSG_CHUNKMAP_REQUEST, MSG_CHUNK_DATA, MSG_CHUNK_REQUEST, MSG_FILE_REQUEST, MSG_MANIFEST,
    MSG_MANIFEST_REQUEST,
};

/// Sends a manifest back to the asking peer.
pub type ManifestSink = Arc<dyn Fn(Manifest) + Send + Sync>;

/// Asked to send one file, by absolute local path, to the peer.
///
/// A callback because sending is the Transfer channel's job and this crate has
/// no business owning a second bulk path — the registry's "reuses Transfer for
/// bytes", made structural.
pub type SendFile = Arc<dyn Fn(std::path::PathBuf) + Send + Sync>;

/// Delivers a manifest *this* device asked for to whoever is waiting.
pub type IncomingSink = Arc<dyn Fn(Manifest) + Send + Sync>;

/// Sends a chunk map back to the asking peer.
pub type ChunkMapSink = Arc<dyn Fn(crate::manifest::ChunkMapResponse) + Send + Sync>;
/// Sends one chunk's bytes back to the asking peer.
pub type ChunkDataSink = Arc<dyn Fn(crate::manifest::ChunkData) + Send + Sync>;

pub struct SyncHandler {
    shares: Shares,
    trust: Arc<dyn TrustStore>,
    peer: Arc<OnceLock<DeviceId>>,
    answer: ManifestSink,
    send_file: SendFile,
    incoming: IncomingSink,
    /// Answers to chunk questions the peer asked us.
    chunk_map: ChunkMapSink,
    chunk_data: ChunkDataSink,
    /// Answers to chunk questions *we* asked, routed to whoever is waiting.
    incoming_chunk_map: ChunkMapSink,
    incoming_chunk: ChunkDataSink,
}

impl SyncHandler {
    #[must_use]
    pub fn new(
        shares: Shares,
        trust: Arc<dyn TrustStore>,
        answer: ManifestSink,
        send_file: SendFile,
        incoming: IncomingSink,
    ) -> (Arc<SyncHandler>, Arc<OnceLock<DeviceId>>) {
        Self::with_chunks(
            shares,
            trust,
            answer,
            send_file,
            incoming,
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
    }

    /// As [`new`](Self::new), plus the four chunk sinks that make delta
    /// transfer work.
    ///
    /// Separate constructor rather than four more arguments on `new`: a caller
    /// that does not do delta transfer — the CLI serving nothing, a test —
    /// should not have to name four no-op closures to say so.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_chunks(
        shares: Shares,
        trust: Arc<dyn TrustStore>,
        answer: ManifestSink,
        send_file: SendFile,
        incoming: IncomingSink,
        chunk_map: ChunkMapSink,
        chunk_data: ChunkDataSink,
        incoming_chunk_map: ChunkMapSink,
        incoming_chunk: ChunkDataSink,
    ) -> (Arc<SyncHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(SyncHandler {
            shares,
            trust,
            peer: peer.clone(),
            answer,
            send_file,
            incoming,
            chunk_map,
            chunk_data,
            incoming_chunk_map,
            incoming_chunk,
        });
        (handler, peer)
    }
}

/// Split a file into chunks, or `None` if it cannot be read or is too large to
/// hold in memory.
///
/// The ceiling is the same judgement the index makes: above it a file syncs
/// whole rather than by delta, because loading a multi-gigabyte file to save
/// bandwidth on it trades a real problem for a worse one.
///
/// # Why this is awaited off the runtime
///
/// It reads the whole file and runs a content-defined chunking pass over it —
/// up to 256 MiB of blocking I/O and CPU. Called inline from an async handler,
/// that occupies a runtime worker for the duration: on a small executor it
/// stalls *every* other session — heartbeats, transfers in flight, the accept
/// loop — for as long as the pass takes, and a peer can ask for it once per
/// file in a manifest. `spawn_blocking` moves it to the pool that exists for
/// exactly this, so a large file costs one blocking thread instead of the
/// engine's responsiveness.
async fn chunk_file(path: &std::path::Path) -> Option<Vec<peerbeam_chunk::Chunk>> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || chunk_file_blocking(&owned))
        .await
        .ok()?
}

/// The blocking body, kept separate so it stays directly testable.
fn chunk_file_blocking(path: &std::path::Path) -> Option<Vec<peerbeam_chunk::Chunk>> {
    const MAX_CHUNKABLE: u64 = 256 * 1024 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_CHUNKABLE {
        return None;
    }
    Some(peerbeam_chunk::split(&std::fs::read(path).ok()?))
}

/// Read `len` bytes at `offset`.
fn read_range(path: &std::path::Path, offset: u64, len: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[async_trait]
impl MessageHandler for SyncHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::SYNC
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("sync peer not bound".into()));
        };
        match frame.message_type.get() {
            MSG_MANIFEST => {
                // An answer to something we asked.
                if let Ok(m) = Manifest::from_frame(&frame) {
                    (self.incoming)(m);
                }
                Ok(())
            }
            MSG_MANIFEST_REQUEST => {
                let req = ManifestRequest::from_frame(&frame)
                    .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
                // Seeing what exists is the browse question, so it is the browse
                // permission — one grant, one meaning, rather than a second
                // permission that means the same thing on a different channel.
                if !self.trust.may(peer, Permission::Browse) {
                    (self.answer)(Manifest::denied(&req.path));
                    return Ok(());
                }
                (self.answer)(build(&self.shares, &req.path));
                Ok(())
            }
            MSG_FILE_REQUEST => {
                let req = FileRequest::from_frame(&frame)
                    .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
                // **Two permissions, because these are two questions.** Browse
                // says the peer may know the file exists; `files` says it may
                // receive it. A device allowed to read a folder listing has not
                // thereby been allowed to pull every byte out of it.
                if !self.trust.may(peer, Permission::Browse)
                    || !self.trust.may(peer, Permission::Files)
                {
                    return Ok(());
                }
                // Containment is `Shares::resolve` — the same canonicalise-then-
                // compare that browsing uses, so a request cannot climb out of a
                // share by any route browsing already refuses.
                let Ok(real) = self.shares.resolve(&req.path) else {
                    return Ok(());
                };
                if real.is_file() {
                    (self.send_file)(real);
                }
                Ok(())
            }
            MSG_CHUNKMAP_REQUEST => {
                let req = crate::manifest::ChunkMapRequest::from_frame(&frame)
                    .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
                // Describing a file by chunks says how big its pieces are and
                // where they repeat, which is information about content — so it
                // takes the same two grants as fetching the bytes, not the
                // weaker browse-only one.
                if !self.trust.may(peer, Permission::Browse)
                    || !self.trust.may(peer, Permission::Files)
                {
                    (self.chunk_map)(crate::manifest::ChunkMapResponse {
                        path: req.path.clone(),
                        chunks: Vec::new(),
                        denied: true,
                    });
                    return Ok(());
                }
                let chunks = self.shares.resolve(&req.path).ok().filter(|p| p.is_file());
                let chunks = match chunks {
                    Some(p) => chunk_file(&p).await.unwrap_or_default(),
                    None => Vec::new(),
                };
                (self.chunk_map)(crate::manifest::ChunkMapResponse {
                    path: req.path,
                    chunks,
                    denied: false,
                });
                Ok(())
            }
            MSG_CHUNK_REQUEST => {
                let req = crate::manifest::ChunkRequest::from_frame(&frame)
                    .map_err(|e| SessionError::FrameDecode(e.to_string()))?;
                if !self.trust.may(peer, Permission::Browse)
                    || !self.trust.may(peer, Permission::Files)
                {
                    return Ok(());
                }
                let Ok(real) = self.shares.resolve(&req.path) else {
                    return Ok(());
                };
                if !real.is_file() {
                    return Ok(());
                }
                // Bounded per request, so one message cannot ask a peer to read
                // an unbounded amount of its own disk back at the asker.
                const MAX_PER_REQUEST: usize = 64;
                let Some(chunks) = chunk_file(&real).await else {
                    return Ok(());
                };
                for hash in req.hashes.iter().take(MAX_PER_REQUEST) {
                    // Served **only** from the file that was asked about. A
                    // request naming one path must never return bytes from
                    // another, or the path check above would mean nothing.
                    if let Some(c) = chunks.iter().find(|c| &c.hash == hash) {
                        if let Some(bytes) = read_range(&real, c.offset, c.len) {
                            (self.chunk_data)(crate::manifest::ChunkData {
                                hash: hash.clone(),
                                bytes,
                            });
                        }
                    }
                }
                Ok(())
            }
            // **Answers, and only from a device this one approved.**
            //
            // Both of these are replies to a request *this* side made, so the
            // capability checks the request arms use are the wrong test: `may(peer,
            // Browse)` asks whether the peer may browse *us*, which has nothing to
            // do with whether we asked them for a chunk. They were therefore
            // ungated entirely — the only two arms in this handler that were —
            // and both hand their payload straight into an unbounded queue, so any
            // device that completed a handshake could push bytes into this
            // process's memory without ever having been asked for them.
            //
            // Approval is the honest gate here: a delta fetch only ever goes to a
            // device the user approved, so an answer from anything else was not one
            // we asked for. It is not full request/response correlation — that
            // wants a request id on the wire, which these messages do not carry —
            // so it narrows who can do it rather than closing it outright.
            MSG_CHUNKMAP => {
                if !self.trust.is_approved(peer) {
                    return Ok(());
                }
                if let Ok(m) = crate::manifest::ChunkMapResponse::from_frame(&frame) {
                    (self.incoming_chunk_map)(m);
                }
                Ok(())
            }
            MSG_CHUNK_DATA => {
                if !self.trust.is_approved(peer) {
                    return Ok(());
                }
                if let Ok(d) = crate::manifest::ChunkData::from_frame(&frame) {
                    (self.incoming_chunk)(d);
                }
                Ok(())
            }
            // Unknown OPTIONAL types are skipped (MESSAGE_REGISTRY.md §6).
            _ => Ok(()),
        }
    }
}

/// Build a manifest with no version information — the shape a device serving a
/// read-only share sends, since it keeps no index of what it changed.
#[must_use]
pub fn build(shares: &Shares, path: &str) -> Manifest {
    build_with(shares, path, None)
}

/// Build the manifest for one share-relative path, with version vectors from
/// `index` when one is supplied.
///
/// Without an index the entries carry empty vectors, which relate as `Behind`
/// to anything — the safe reading for a device that cannot say what it changed,
/// since its files are taken rather than treated as conflicts.
///
/// Build the manifest for one share-relative path.
///
/// Recursive, unlike browsing's single-level listing: a mirror needs the whole
/// subtree, and asking per directory would cost a round trip per folder.
/// Bounded at [`MAX_FILES`], and says when it was.
#[must_use]
pub fn build_with(
    shares: &Shares,
    path: &str,
    versions: Option<&std::collections::BTreeMap<String, crate::version::VersionVector>>,
) -> Manifest {
    build_full(shares, path, versions, None)
}

/// [`build_with`], also stating each file's content hash so a peer can spot a
/// move.
#[must_use]
pub fn build_full(
    shares: &Shares,
    path: &str,
    versions: Option<&std::collections::BTreeMap<String, crate::version::VersionVector>>,
    hashes: Option<&std::collections::BTreeMap<String, String>>,
) -> Manifest {
    let Ok(root) = shares.resolve(path) else {
        return Manifest::denied(path);
    };
    let mut files = Vec::new();
    let mut truncated = false;
    walk(&root, &root, &mut files, &mut truncated, versions, hashes);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Manifest {
        path: path.to_string(),
        files,
        truncated,
        denied: false,
    }
}

fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<FileEntry>,
    truncated: &mut bool,
    versions: Option<&std::collections::BTreeMap<String, crate::version::VersionVector>>,
    hashes: Option<&std::collections::BTreeMap<String, String>>,
) {
    if *truncated {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= MAX_FILES {
            *truncated = true;
            return;
        }
        let path = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // Symlinks are skipped entirely rather than followed. Following one
        // could walk out of the share (and `Shares::resolve` would then refuse
        // to serve the file anyway, producing a manifest full of entries that
        // can never be fetched), or walk in a circle.
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(root, &path, out, truncated, versions, hashes);
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel_path = peerbeam_domain::wire_path(rel);
            let version = versions
                .and_then(|v| v.get(&rel_path))
                .cloned()
                .unwrap_or_default();
            // Hashed from the chunk map when one is available — the map already
            // covers every byte, so this costs a read the manifest was going to
            // need anyway rather than a second pass over the file.
            let content = hashes
                .and_then(|h| h.get(&rel_path))
                .cloned()
                .unwrap_or_default();
            out.push(FileEntry {
                path: rel_path,
                size: meta.len(),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64),
                content,
                version,
                deleted: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, Shares) {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        std::fs::create_dir(&share).unwrap();
        std::fs::write(share.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(share.join("sub")).unwrap();
        std::fs::write(share.join("sub").join("b.txt"), b"deep").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"nope").unwrap();
        (dir, Shares::new([share]))
    }

    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::session::ChannelId;
    use std::sync::Mutex;

    struct Trust(TrustRecord);

    impl TrustStore for Trust {
        fn record(&self, _record: TrustRecord) -> peerbeam_domain::Result<()> {
            Ok(())
        }
        fn lookup(&self, _device: &DeviceId) -> peerbeam_domain::Result<Option<TrustRecord>> {
            Ok(Some(self.0.clone()))
        }
        fn is_trusted(&self, _device: &DeviceId) -> bool {
            true
        }
    }

    /// The same fake with approval withheld: a device that completed a
    /// handshake and was pinned, which is what every stranger is.
    fn unapproved_trust() -> Arc<dyn TrustStore> {
        Arc::new(Trust(TrustRecord {
            device: DeviceId::from("pb-bob"),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved: false,
            permissions: PermissionSet::granted_on_approval(),
            expires_at: None,
            mine: false,
        }))
    }

    /// A handler that reports what reached the *answer* sinks — the two arms
    /// that take a reply to a request this side made.
    fn answer_handler(
        trust: Arc<dyn TrustStore>,
    ) -> (
        Arc<SyncHandler>,
        Arc<Mutex<Vec<crate::manifest::ChunkData>>>,
        tempfile::TempDir,
    ) {
        let (dir, shares) = tree();
        let got: Arc<Mutex<Vec<crate::manifest::ChunkData>>> = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let (h, slot) = SyncHandler::with_chunks(
            shares,
            trust,
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(move |d| g.lock().unwrap().push(d)),
        );
        let _ = slot.set(DeviceId::from("pb-bob"));
        (h, got, dir)
    }

    /// **Unsolicited answers from a stranger are dropped.** `MSG_CHUNK_DATA` and
    /// `MSG_CHUNKMAP` are replies to a request this side made, so the capability
    /// checks the request arms use are the wrong question — and they were left
    /// ungated entirely, the only two arms here that were. Both hand their
    /// payload into an unbounded queue, so any device that finished a handshake
    /// could push bytes into this process's memory unasked.
    #[tokio::test]
    async fn chunk_bytes_from_an_unapproved_device_are_dropped() {
        let (h, got, _dir) = answer_handler(unapproved_trust());
        h.handle(
            crate::manifest::ChunkData {
                hash: "00".repeat(32),
                bytes: vec![1, 2, 3],
            }
            .to_frame(ChannelId::new(1))
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            got.lock().unwrap().is_empty(),
            "a device the user never approved queued chunk bytes into this process"
        );
    }

    /// And an approved one still gets through, so the gate is a gate and not a
    /// wall: this is the path a real delta fetch answers on.
    #[tokio::test]
    async fn chunk_bytes_from_an_approved_device_still_arrive() {
        let (h, got, _dir) = answer_handler(trust(true, true));
        h.handle(
            crate::manifest::ChunkData {
                hash: "00".repeat(32),
                bytes: vec![1, 2, 3],
            }
            .to_frame(ChannelId::new(1))
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(got.lock().unwrap().len(), 1);
    }

    fn trust(browse: bool, files: bool) -> Arc<dyn TrustStore> {
        Arc::new(Trust(TrustRecord {
            device: DeviceId::from("pb-bob"),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved: true,
            permissions: PermissionSet::granted_on_approval()
                .set(Permission::Browse, browse)
                .set(Permission::Files, files),
            expires_at: None,
            mine: false,
        }))
    }

    type Sent = Arc<Mutex<Vec<std::path::PathBuf>>>;

    fn handler(
        shares: Shares,
        trust: Arc<dyn TrustStore>,
    ) -> (Arc<SyncHandler>, Arc<Mutex<Vec<Manifest>>>, Sent) {
        let manifests: Arc<Mutex<Vec<Manifest>>> = Arc::new(Mutex::new(Vec::new()));
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let m = manifests.clone();
        let s = sent.clone();
        let (h, slot) = SyncHandler::new(
            shares,
            trust,
            Arc::new(move |x| m.lock().unwrap().push(x)),
            Arc::new(move |p| s.lock().unwrap().push(p)),
            Arc::new(|_| {}),
        );
        let _ = slot.set(DeviceId::from("pb-bob"));
        (h, manifests, sent)
    }

    #[tokio::test]
    async fn a_peer_without_browse_gets_no_manifest() {
        let (_dir, shares) = tree();
        let (h, manifests, _sent) = handler(shares, trust(false, true));
        h.handle(
            ManifestRequest {
                path: "share".into(),
            }
            .to_frame(ChannelId::new(1))
            .unwrap(),
        )
        .await
        .unwrap();
        let got = manifests.lock().unwrap();
        assert!(got[0].denied);
        assert!(got[0].files.is_empty());
    }

    /// **Two permissions, because these are two questions.** Being allowed to
    /// see that a file exists is not being allowed to pull every byte of it.
    #[tokio::test]
    async fn browse_alone_does_not_let_a_peer_take_files() {
        let (_dir, shares) = tree();
        let (h, _m, sent) = handler(shares, trust(true, false));
        h.handle(
            FileRequest {
                path: "share/a.txt".into(),
            }
            .to_frame(ChannelId::new(1))
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            sent.lock().unwrap().is_empty(),
            "a peer with only browse pulled a file"
        );
    }

    #[tokio::test]
    async fn a_peer_with_both_permissions_gets_the_file() {
        let (_dir, shares) = tree();
        let (h, _m, sent) = handler(shares, trust(true, true));
        h.handle(
            FileRequest {
                path: "share/a.txt".into(),
            }
            .to_frame(ChannelId::new(1))
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    /// A file request cannot climb out of a share by any route browsing already
    /// refuses — the containment is the same function.
    #[tokio::test]
    async fn a_file_request_outside_the_share_sends_nothing() {
        let (_dir, shares) = tree();
        let (h, _m, sent) = handler(shares, trust(true, true));
        for path in ["share/../secret.txt", "elsewhere/x", "/etc/passwd"] {
            h.handle(
                FileRequest { path: path.into() }
                    .to_frame(ChannelId::new(1))
                    .unwrap(),
            )
            .await
            .unwrap();
        }
        assert!(
            sent.lock().unwrap().is_empty(),
            "a request escaped the share"
        );
    }

    #[test]
    fn a_manifest_covers_the_whole_subtree_with_relative_paths() {
        // A mirror needs the subtree, and the paths must be relative — an
        // absolute one would leak the peer's filesystem layout.
        let (dir, shares) = tree();
        let m = build(&shares, "share");
        let names: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "sub/b.txt"]);
        let dumped = serde_json::to_string(&m).unwrap();
        assert!(!dumped.contains(&dir.path().to_string_lossy().into_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_skipped_rather_than_followed() {
        // Following one could walk out of the share — and `Shares::resolve`
        // would refuse to serve the file anyway, so the manifest would list
        // entries that can never be fetched.
        let (dir, shares) = tree();
        std::os::unix::fs::symlink(
            dir.path().join("secret.txt"),
            dir.path().join("share").join("escape"),
        )
        .unwrap();
        let m = build(&shares, "share");
        assert!(
            !m.files.iter().any(|f| f.path == "escape"),
            "a symlink was followed out of the share"
        );
    }

    #[test]
    fn a_path_outside_every_share_is_denied() {
        let (_dir, shares) = tree();
        assert!(build(&shares, "share/../secret.txt").denied);
        assert!(build(&shares, "elsewhere").denied);
    }
}
