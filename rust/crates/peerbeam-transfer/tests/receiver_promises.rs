//! What the receiver is allowed to promise the sender.
//!
//! `Verify { ok: true }` is the one frame that tells a sender its copy is safe
//! to forget. It used to be sent from the `Control::Complete` arm — before the
//! writer was flushed, before it was closed, and before the `.part` was renamed
//! into place. So a full disk, a read-only destination or a failed rename all
//! produced the same conversation: *"received and verified"* on the wire, and
//! no file on disk.
//!
//! The second test here is the other side of the same honesty: the size a user
//! approved is a bound on what may be written, not a hint.

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, Link, StorageProvider};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl};

struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl MemLink {
    fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a_tx, b_rx) = mpsc::channel(cap);
        let (b_tx, a_rx) = mpsc::channel(cap);
        (
            MemLink { tx: a_tx, rx: a_rx },
            MemLink { tx: b_tx, rx: b_rx },
        )
    }
}

#[async_trait]
impl Link for MemLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| DomainError::Connection("peer closed".into()))
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        Ok(self.rx.recv().await)
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Everything works except publishing the file — the realistic shape of a
/// destination that goes read-only, or a rename that loses a race.
struct FinalizeFailsStorage(FsStorage);

#[async_trait]
impl StorageProvider for FinalizeFailsStorage {
    async fn open_write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        self.0.open_write(path).await
    }
    async fn open_append(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        self.0.open_append(path).await
    }
    async fn open_read(
        &self,
        path: &str,
        offset: u64,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>> {
        self.0.open_read(path, offset).await
    }
    async fn size(&self, path: &str) -> Result<Option<u64>> {
        self.0.size(path).await
    }
    async fn list_files(&self, root: &str) -> Result<Vec<(String, u64)>> {
        self.0.list_files(root).await
    }
    async fn finalize(&self, _temp: &str, _dest: &str) -> Result<String> {
        Err(DomainError::Storage("destination is read-only".into()))
    }
    async fn finalize_replacing(&self, _temp: &str, _dest: &str) -> Result<String> {
        Err(DomainError::Storage("destination is read-only".into()))
    }
}

/// **A file that could not be published is not a received file.**
///
/// The sender must learn that, because it is the only party that still has a
/// copy. Before the `Verify` frame was moved after the work it describes, this
/// exchange ended with the sender believing the transfer had completed.
#[tokio::test]
async fn a_receiver_that_cannot_publish_the_file_does_not_claim_it_arrived() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.bin");
    tokio::fs::write(&src, vec![7u8; 40_000]).await.unwrap();
    // A **directory**: `receive_file` names the file from the sender's meta.
    let out_dir = dir.path().join("in");
    let landed = out_dir.join("source.bin");

    let (mut la, mut lb) = MemLink::pair(64);
    let sender_storage = FsStorage::new();
    let receiver_storage = FinalizeFailsStorage(FsStorage::new());
    let (cs, cr) = (TransferControl::new(), TransferControl::new());
    let (ptx, _prx) = mpsc::unbounded_channel();
    let (ptx2, _prx2) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "t-1".into(),
        name: "source.bin".into(),
        path: src.to_string_lossy().into_owned(),
        size: 40_000,
        chunk_size: 8 * 1024,
    };
    let out_str = out_dir.to_string_lossy().into_owned();

    let (sent, received) = tokio::join!(
        send_file(&mut la, &sender_storage, req, &cs, &ptx, 0),
        receive_file(&mut lb, &receiver_storage, &out_str, &cr, &ptx2),
    );

    assert!(
        received.is_err(),
        "the receive itself failed — that half was always right"
    );
    assert!(
        sent.is_err(),
        "the SENDER must be told: it holds the only other copy, and a \
         `Verify {{ ok: true }}` sent before the rename told it to stop caring"
    );
    assert!(
        !landed.exists(),
        "nothing may be published under the final name when the rename failed"
    );
}

/// **The size the user approved is a bound.**
///
/// A sender that declares a small file and streams a large one used to have the
/// whole thing written, hashed and — if it finished with an honest checksum —
/// published under the declared name.
#[tokio::test]
async fn a_sender_that_exceeds_its_declared_size_is_refused() {
    use peerbeam_domain::port::FrameKind;
    use peerbeam_transfer::{Control, TransferMeta};

    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("in");
    let out_str = out_dir.to_string_lossy().into_owned();
    let landed = out_dir.join("photo.jpg");

    let (mut la, mut lb) = MemLink::pair(64);
    let storage = FsStorage::new();
    let (cr, _cs) = (TransferControl::new(), TransferControl::new());
    let (ptx, _prx) = mpsc::unbounded_channel();

    // A dishonest sender, written by hand: declare a small photo, then stream
    // far more than that.
    let liar = async move {
        let meta = TransferMeta {
            transfer_id: "t-2".into(),
            name: "photo.jpg".into(),
            size: 1024,
            chunk_size: 1024,
        };
        la.send_frame(Frame {
            kind: FrameKind::Meta,
            payload: serde_json::to_vec(&meta).unwrap().into(),
        })
        .await
        .unwrap();
        // Ten times what it said, then an **honest** checksum of what it
        // actually sent. That is what makes this the dangerous case rather
        // than a stalled one: without a bound the receiver hashes 10 KiB,
        // matches, and publishes it under the 1 KiB name the user approved.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for _ in 0..10 {
            let chunk = vec![0xAB; 1024];
            hasher.update(&chunk);
            let _ = la
                .send_frame(Frame {
                    kind: FrameKind::Chunk,
                    payload: chunk.into(),
                })
                .await;
        }
        let checksum = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let _ = la
            .send_frame(Frame {
                kind: FrameKind::Control,
                payload: serde_json::to_vec(&Control::Complete { checksum })
                    .unwrap()
                    .into(),
            })
            .await;
        // Stay on the line for the answer. Dropping the link here would make
        // the receive fail with "peer closed" whatever the bound did — which
        // is exactly how this test passed against the unbounded code and
        // proved nothing.
        let _ = la.recv_frame().await;
    };

    let (_, received) = tokio::join!(liar, receive_file(&mut lb, &storage, &out_str, &cr, &ptx),);

    assert!(
        received.is_err(),
        "a sender that writes past the size it declared must be refused"
    );
    assert!(
        !landed.exists(),
        "and nothing may be published under the approved name"
    );
}
