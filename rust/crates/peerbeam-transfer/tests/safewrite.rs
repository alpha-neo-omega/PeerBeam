//! Safe file writing: received data lands in `.part` and only becomes the
//! final file atomically on verified completion; existing files are never
//! clobbered; a checksum-mismatch failure leaves no final file and no
//! poisoned `.part` (removed so a retry starts clean rather than being stuck
//! resuming from corrupt bytes forever).

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link};
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

/// Flips one byte of the first chunk to force an integrity failure.
struct CorruptingLink {
    inner: MemLink,
    hit: bool,
}
#[async_trait]
impl Link for CorruptingLink {
    async fn send_frame(&mut self, mut frame: Frame) -> Result<()> {
        if !self.hit && frame.kind == FrameKind::Chunk && !frame.payload.is_empty() {
            self.hit = true;
            let mut d = frame.payload.to_vec();
            d[0] ^= 0xFF;
            frame.payload = Bytes::from(d);
        }
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        self.inner.recv_frame().await
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

fn req(src: &std::path::Path, len: usize) -> SendRequest {
    SendRequest {
        transfer_id: "sw".into(),
        name: "f.bin".into(),
        path: src.to_string_lossy().into(),
        size: len as u64,
        chunk_size: 64 * 1024,
    }
}

#[tokio::test]
async fn does_not_overwrite_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes: Vec<u8> = (0..80 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();

    // A file with the same name already exists at the destination.
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("f.bin"), b"pre-existing").unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_file(&mut la, &storage, req(&src, bytes.len()), &cs, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);
    rs.unwrap();
    let received = rr.unwrap();

    // Original untouched; new file written under a non-colliding name.
    assert_eq!(std::fs::read(out.join("f.bin")).unwrap(), b"pre-existing");
    assert_eq!(received.name, "f (1).bin");
    assert_eq!(std::fs::read(out.join("f (1).bin")).unwrap(), bytes);
    // No leftover .part.
    assert!(!out.join("f.bin.part").exists());
}

#[tokio::test]
async fn failed_transfer_leaves_no_final_file_and_no_poisoned_part() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes: Vec<u8> = (0..80 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (la, mut lb) = MemLink::pair(4);
    let mut sender = CorruptingLink {
        inner: la,
        hit: false,
    };
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_file(&mut sender, &storage, req(&src, bytes.len()), &cs, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &cr, &ptx);
    let (_rs, rr) = tokio::join!(send, recv);

    // Integrity failure → no final file, and the poisoned .part is removed
    // (not left around to be silently re-hashed and fail forever).
    assert!(matches!(rr, Err(DomainError::Integrity(_))));
    assert!(!out.join("f.bin").exists(), "no final file on failure");
    assert!(
        !out.join("f.bin.part").exists(),
        "poisoned .part must be removed, not retained"
    );
}
