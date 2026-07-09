//! Integrity verification: a link that corrupts a chunk in flight must cause
//! both sides to fail with an integrity error, not silently accept bad data.

use async_trait::async_trait;
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

/// Flips one byte of the first chunk it forwards.
struct CorruptingLink {
    inner: MemLink,
    hit: bool,
}

#[async_trait]
impl Link for CorruptingLink {
    async fn send_frame(&mut self, mut frame: Frame) -> Result<()> {
        if !self.hit && frame.kind == FrameKind::Chunk && !frame.payload.is_empty() {
            self.hit = true;
            let mut data = frame.payload.to_vec();
            data[0] ^= 0xFF;
            frame = Frame {
                kind: FrameKind::Chunk,
                payload: data.into(),
            };
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

#[tokio::test]
async fn corrupted_chunk_fails_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes: Vec<u8> = (0..100 * 1024).map(|i| (i % 251) as u8).collect();
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

    let req = SendRequest {
        transfer_id: "corrupt-1".into(),
        name: "f.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file(&mut sender, &storage, req, &cs, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert!(
        matches!(rs, Err(DomainError::Integrity(_))),
        "sender should see integrity failure, got {rs:?}"
    );
    assert!(
        matches!(rr, Err(DomainError::Integrity(_))),
        "receiver should reject corrupted data, got {rr:?}"
    );
}

#[tokio::test]
async fn clean_transfer_verifies_ok() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes: Vec<u8> = (0..100 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "clean-1".into(),
        name: "f.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    rs.unwrap();
    rr.unwrap();
    assert_eq!(std::fs::read(out.join("f.bin")).unwrap(), bytes);
}
