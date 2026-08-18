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
    FileEntry, FileRequest, Manifest, ManifestRequest, MAX_FILES, MSG_FILE_REQUEST, MSG_MANIFEST,
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

pub struct SyncHandler {
    shares: Shares,
    trust: Arc<dyn TrustStore>,
    peer: Arc<OnceLock<DeviceId>>,
    answer: ManifestSink,
    send_file: SendFile,
    incoming: IncomingSink,
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
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(SyncHandler {
            shares,
            trust,
            peer: peer.clone(),
            answer,
            send_file,
            incoming,
        });
        (handler, peer)
    }
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
            // Unknown OPTIONAL types are skipped (MESSAGE_REGISTRY.md §6).
            _ => Ok(()),
        }
    }
}

/// Build the manifest for one share-relative path.
///
/// Recursive, unlike browsing's single-level listing: a mirror needs the whole
/// subtree, and asking per directory would cost a round trip per folder.
/// Bounded at [`MAX_FILES`], and says when it was.
#[must_use]
pub fn build(shares: &Shares, path: &str) -> Manifest {
    let Ok(root) = shares.resolve(path) else {
        return Manifest::denied(path);
    };
    let mut files = Vec::new();
    let mut truncated = false;
    walk(&root, &root, &mut files, &mut truncated);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Manifest {
        path: path.to_string(),
        files,
        truncated,
        denied: false,
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<FileEntry>, truncated: &mut bool) {
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
            walk(root, &path, out, truncated);
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            out.push(FileEntry {
                path: rel.to_string_lossy().into_owned(),
                size: meta.len(),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64),
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
