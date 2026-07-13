//! Interrupted-transfer recovery: a connection that fails on the first
//! attempt, then succeeds; the drivers reconnect and resume from a
//! pre-existing partial file, verify integrity, and clear the checkpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::mpsc;

use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{Frame, FrameKind, Link, ReliabilityStore};
use peerbeam_reliability_fs::FsReliability;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file_recover, send_file_recover, LinkFactory, SendRequest, TransferControl,
    TransferOutcome,
};

// ── In-memory link + chunk-byte counter ─────────────────────────

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

struct CountingLink {
    inner: MemLink,
    sent: Arc<AtomicU64>,
}

#[async_trait]
impl Link for CountingLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if frame.kind == FrameKind::Chunk {
            self.sent
                .fetch_add(frame.payload.len() as u64, Ordering::SeqCst);
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

// ── A broker that fails the first connect on each side ───────────

struct Shared {
    send_fails: usize,
    recv_fails: usize,
    made: bool,
    la: Option<Box<dyn Link>>,
    lb: Option<Box<dyn Link>>,
    sent: Arc<AtomicU64>,
}

impl Shared {
    fn ensure(&mut self) {
        if !self.made {
            let (a, b) = MemLink::pair(8);
            self.la = Some(Box::new(CountingLink {
                inner: a,
                sent: self.sent.clone(),
            }));
            self.lb = Some(Box::new(b));
            self.made = true;
        }
    }
}

struct SendFactory(Arc<Mutex<Shared>>);
struct RecvFactory(Arc<Mutex<Shared>>);

#[async_trait]
impl LinkFactory for SendFactory {
    async fn connect(&mut self) -> Result<Box<dyn Link>> {
        let mut s = self.0.lock().unwrap();
        if s.send_fails > 0 {
            s.send_fails -= 1;
            return Err(DomainError::Connection("send outage".into()));
        }
        s.ensure();
        s.la.take()
            .ok_or_else(|| DomainError::Connection("no link".into()))
    }
}

#[async_trait]
impl LinkFactory for RecvFactory {
    async fn connect(&mut self) -> Result<Box<dyn Link>> {
        let mut s = self.0.lock().unwrap();
        if s.recv_fails > 0 {
            s.recv_fails -= 1;
            return Err(DomainError::Connection("recv outage".into()));
        }
        s.ensure();
        s.lb.take()
            .ok_or_else(|| DomainError::Connection("no link".into()))
    }
}

fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn checkpoint(id: &str) -> TransferSession {
    TransferSession {
        id: TransferId::from(id),
        peer: DeviceId::from("peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: vec![],
        total_bytes: 50 * 1024,
        transferred_bytes: 0,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
    }
}

#[tokio::test]
async fn reconnects_resumes_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes = pattern(50 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    // A prior interrupted run left the first 20 KiB in the `.part` file.
    let partial = 20 * 1024usize;
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("f.bin.part"), &bytes[..partial]).unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let reliability = FsReliability::new(dir.path().join("checkpoints"));

    let shared = Arc::new(Mutex::new(Shared {
        send_fails: 1, // first send-side connect fails
        recv_fails: 1, // first recv-side connect fails
        made: false,
        la: None,
        lb: None,
        sent: Arc::new(AtomicU64::new(0)),
    }));
    let sent = shared.lock().unwrap().sent.clone();
    let mut sf = SendFactory(shared.clone());
    let mut rf = RecvFactory(shared.clone());

    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "recov-1".into(),
        name: "f.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file_recover(
        &mut sf,
        &storage,
        &reliability,
        req,
        checkpoint("recov-1"),
        &cs,
        &ptx,
        5,
        3,
    );
    let recv = receive_file_recover(&mut rf, &storage, &out_str, &cr, &ptx, 5);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);

    // File is whole and correct after resume.
    assert_eq!(std::fs::read(out.join("f.bin")).unwrap(), bytes);

    // Only the missing remainder crossed the wire (resume, not restart).
    assert_eq!(sent.load(Ordering::SeqCst), (bytes.len() - partial) as u64);

    // Checkpoint was persisted during the run and cleared on success.
    assert!(reliability
        .load_checkpoint(&TransferId::from("recov-1"))
        .unwrap()
        .is_none());
}
