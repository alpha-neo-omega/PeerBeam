//! End-to-end streaming transfer tests.
//!
//! Uses an in-memory, bounded `Link` (so the channel exerts backpressure like
//! a real socket) and the real filesystem `StorageProvider` over temp files.
//! Together these exercise streaming, chunking, progress, cancel, pause, and
//! retry without any network — and prove nothing loads the whole file (the
//! send buffer is a single chunk).

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use peerbeam_domain::entity::{Direction, Progress};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file, send_file, SendRequest, TransferControl, TransferMeta, TransferOutcome,
};

// ── In-memory link ──────────────────────────────────────────────

/// One end of an in-memory duplex link backed by bounded channels.
struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl MemLink {
    /// A connected pair. `cap` bounds in-flight frames (backpressure).
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

/// Wraps a link and fails the first `fails` sends to exercise retry.
struct FlakyLink {
    inner: MemLink,
    fails: usize,
}

#[async_trait]
impl Link for FlakyLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if self.fails > 0 {
            self.fails -= 1;
            return Err(DomainError::Connection("transient".into()));
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

// ── Helpers ─────────────────────────────────────────────────────

fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn drain(rx: &mut mpsc::UnboundedReceiver<Progress>) -> Vec<Progress> {
    let mut out = Vec::new();
    while let Ok(p) = rx.try_recv() {
        out.push(p);
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn streams_large_file_in_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(2 * 1024 * 1024); // 2 MiB
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4); // small cap → real backpressure
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();

    let chunk_size = 64 * 1024u32;
    let req = SendRequest {
        transfer_id: "t1".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size,
    };

    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    let rr = rr.unwrap();
    assert_eq!(rr.outcome, TransferOutcome::Completed);
    assert_eq!(rr.bytes, bytes.len() as u64);

    let written = std::fs::read(out.join("src.bin")).unwrap();
    assert_eq!(written, bytes, "received file must match byte-for-byte");

    // Progress proves it was chunked and reached the full size.
    let progress = drain(&mut prx);
    let sends: Vec<&Progress> = progress
        .iter()
        .filter(|p| p.direction == Direction::Sending)
        .collect();
    let expected_chunks = bytes.len() as u32 / chunk_size;
    assert!(
        sends.len() as u32 >= expected_chunks,
        "expected many chunk updates, got {}",
        sends.len()
    );
    assert_eq!(sends.last().unwrap().transferred_bytes, bytes.len() as u64);
}

#[tokio::test]
async fn cancel_while_paused_stops_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    std::fs::write(&src, pattern(4 * 1024 * 1024)).unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(1);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // Pause before any chunk, so cancel deterministically catches an
    // in-progress transfer (if pause didn't block, the send would finish
    // before we cancel).
    ctrl_s.pause();

    let req = SendRequest {
        transfer_id: "t2".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: 4 * 1024 * 1024,
        chunk_size: 16 * 1024,
    };

    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let canceller = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        ctrl_s.cancel();
    };

    let (rs, rr, _) = tokio::join!(send, recv, canceller);
    assert_eq!(rs.unwrap(), TransferOutcome::Cancelled);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Cancelled);
}

/// Regression test for the receive side never observing cancellation while
/// parked on `recv_frame`.
///
/// Unlike `cancel_while_paused_stops_both_sides` (where the *sender's*
/// control is cancelled, which makes the sender itself transmit a `Cancel`
/// frame the receiver then reads), this cancels the **receiver's own**
/// control while the peer has gone completely silent after the handshake —
/// nothing ever arrives for the receive loop to read. Before the fix, the
/// loop only checked `ctrl.is_cancelled()` between frames and would then
/// block forever on `recv_frame().await`, so this test would hang (and time
/// out) without `TransferControl::cancelled()` raced into the loop via
/// `select!`.
#[tokio::test]
async fn cancel_interrupts_parked_receive() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // Hand the receiver its Meta frame directly (bypassing `send_file`
    // entirely) so nothing else is ever sent on this link — a stand-in for a
    // sender that stalls right after the handshake.
    let meta = TransferMeta {
        transfer_id: "t-cancel-recv".into(),
        name: "stalled.bin".into(),
        size: 4096,
        chunk_size: 1024,
    };
    la.send_frame(Frame {
        kind: FrameKind::Meta,
        payload: bytes::Bytes::from(serde_json::to_vec(&meta).unwrap()),
    })
    .await
    .unwrap();

    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let canceller = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        ctrl_r.cancel();
    };

    let (rr, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(recv, canceller)
    })
    .await
    .expect("cancel must interrupt a receive parked on recv_frame, not hang");

    assert_eq!(rr.unwrap().outcome, TransferOutcome::Cancelled);
}

#[tokio::test]
async fn pause_then_resume_completes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(1024 * 1024); // 1 MiB
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(2);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    ctrl_s.pause();

    let req = SendRequest {
        transfer_id: "t3".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let resumer = async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        ctrl_s.resume();
    };

    let (rs, rr, _) = tokio::join!(send, recv, resumer);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
}

/// Regression test for a receiver-side pause being a no-op: before the fix,
/// `receive_file`'s loop never checked `ctrl`'s pause, so bytes kept being
/// written while the sender streamed on. Pausing the *receiver's* own
/// control before anything starts must park the receive loop in
/// `wait_while_paused` (not draining/writing any frames) until resumed.
#[tokio::test]
async fn receiver_pause_actually_stops_progress() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(4 * 1024 * 1024); // 4 MiB — many chunks, so a fast
                                          // finish inside the window would
                                          // signal the pause was ignored.
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    // Small capacity: once the receiver stops draining, the sender soon
    // blocks on backpressure instead of buffering the whole file unseen.
    let (mut la, mut lb) = MemLink::pair(1);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // Pause the receiver up front so its very first loop iteration blocks in
    // wait_while_paused rather than proceeding into the frame select.
    ctrl_r.pause();

    let req = SendRequest {
        transfer_id: "t-recv-pause".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    // Neither side should complete while the receiver stays paused.
    let raced = tokio::time::timeout(Duration::from_millis(200), async {
        tokio::select! {
            _ = &mut send => "send",
            _ = &mut recv => "recv",
        }
    })
    .await;
    assert!(
        raced.is_err(),
        "receive must stay parked while the receiver is paused, not complete"
    );

    ctrl_r.resume();
    let (rs, rr) = tokio::join!(send, recv);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
}

#[tokio::test]
async fn retries_transient_link_failures() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(200 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (la, mut lb) = MemLink::pair(8);
    // Fail the first two sends; retries (3) must recover.
    let mut flaky = FlakyLink {
        inner: la,
        fails: 2,
    };
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "t4".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let send = send_file(&mut flaky, &storage, req, &ctrl_s, &ptx, 3);
    let out_str = out.to_string_lossy().to_string();
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
}
