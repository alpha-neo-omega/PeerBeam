//! End-to-end recursive folder transfer tests over an in-memory link and
//! real temp directories: structure preservation, resume (skip complete +
//! append partial), and cancel-then-rerun.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_folder, send_folder, FolderSendRequest, TransferControl, TransferOutcome,
};

// ── In-memory link (+ chunk-byte counter) ───────────────────────

struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
    /// Counts bytes of Chunk frames sent through this end.
    sent_chunks: Arc<AtomicU64>,
}

impl MemLink {
    fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a_tx, b_rx) = mpsc::channel(cap);
        let (b_tx, a_rx) = mpsc::channel(cap);
        (
            MemLink {
                tx: a_tx,
                rx: a_rx,
                sent_chunks: Arc::new(AtomicU64::new(0)),
            },
            MemLink {
                tx: b_tx,
                rx: b_rx,
                sent_chunks: Arc::new(AtomicU64::new(0)),
            },
        )
    }
}

#[async_trait]
impl Link for MemLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if frame.kind == FrameKind::Chunk {
            self.sent_chunks
                .fetch_add(frame.payload.len() as u64, Ordering::SeqCst);
        }
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

// ── Helpers ─────────────────────────────────────────────────────

fn pattern(seed: u8, size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| ((i + seed as usize) % 251) as u8)
        .collect()
}

/// Build a nested source tree, return (root_path, [(rel, bytes)]).
fn build_tree(base: &std::path::Path) -> (String, Vec<(String, Vec<u8>)>) {
    let root = base.join("myfolder");
    let files = vec![
        ("a.txt".to_string(), pattern(1, 40 * 1024)),
        ("sub/b.bin".to_string(), pattern(2, 130 * 1024)),
        ("sub/deep/c.txt".to_string(), pattern(3, 7 * 1024)),
    ];
    for (rel, bytes) in &files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    (root.to_string_lossy().to_string(), files)
}

fn req(root_path: &str) -> FolderSendRequest {
    FolderSendRequest {
        transfer_id: "folder-1".into(),
        root_path: root_path.to_string(),
        chunk_size: 64 * 1024,
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn transfers_folder_preserving_structure() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    let rr = rr.unwrap();
    assert_eq!(rr.outcome, TransferOutcome::Completed);
    assert_eq!(rr.root, "myfolder");
    assert_eq!(rr.files, files.len());

    // Structure and content preserved under out/myfolder/…
    for (rel, bytes) in &files {
        let dest = out.join("myfolder").join(rel);
        assert!(dest.exists(), "missing {}", dest.display());
        assert_eq!(&std::fs::read(&dest).unwrap(), bytes, "content of {rel}");
    }
}

#[tokio::test]
async fn resume_skips_complete_and_appends_partial() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let root = base.join("myfolder");
    let f1 = pattern(1, 100 * 1024); // will be pre-delivered in full
    let f2 = pattern(2, 50 * 1024); // will be 20 KiB partial
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("f1.bin"), &f1).unwrap();
    std::fs::write(root.join("f2.bin"), &f2).unwrap();

    // Pre-populate the destination: f1 complete, f2 first 20 KiB.
    let out = base.join("out");
    let partial = 20 * 1024usize;
    std::fs::create_dir_all(out.join("myfolder")).unwrap();
    std::fs::write(out.join("myfolder/f1.bin"), &f1).unwrap();
    std::fs::write(out.join("myfolder/f2.bin"), &f2[..partial]).unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let sent = la.sent_chunks.clone();
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_folder(
        &mut la,
        &storage,
        req(&root.to_string_lossy()),
        &cs,
        &ptx,
        3,
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);

    // Only the missing remainder of f2 crossed the wire.
    assert_eq!(
        sent.load(Ordering::SeqCst),
        (f2.len() - partial) as u64,
        "should resend only f2's remainder, skip f1 entirely"
    );

    // Both files now complete and correct.
    assert_eq!(std::fs::read(out.join("myfolder/f1.bin")).unwrap(), f1);
    assert_eq!(std::fs::read(out.join("myfolder/f2.bin")).unwrap(), f2);
}

#[tokio::test]
async fn cancel_then_rerun_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();
    let storage = FsStorage::new();

    // First attempt: pause immediately, then cancel — leaves a partial tree.
    {
        let (mut la, mut lb) = MemLink::pair(1);
        let cs = TransferControl::new();
        let cr = TransferControl::new();
        let (ptx, _prx) = mpsc::unbounded_channel();
        cs.pause();
        let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3);
        let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            cs.cancel();
        };
        let (rs, rr, _) = tokio::join!(send, recv, canceller);
        assert_eq!(rs.unwrap(), TransferOutcome::Cancelled);
        assert_eq!(rr.unwrap().outcome, TransferOutcome::Cancelled);
    }

    // Second attempt: fresh link, resumes and completes.
    {
        let (mut la, mut lb) = MemLink::pair(4);
        let cs = TransferControl::new();
        let cr = TransferControl::new();
        let (ptx, _prx) = mpsc::unbounded_channel();
        let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3);
        let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
        let (rs, rr) = tokio::join!(send, recv);
        assert_eq!(rs.unwrap(), TransferOutcome::Completed);
        assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    }

    // Everything intact after resume.
    for (rel, bytes) in &files {
        let dest = out.join("myfolder").join(rel);
        assert_eq!(&std::fs::read(&dest).unwrap(), bytes, "content of {rel}");
    }
}

/// Regression test for a receiver-side pause being a no-op in
/// `receive_folder`: before the fix, its loop never checked `ctrl`'s pause,
/// so bytes kept being written while the sender streamed on. Pausing the
/// *receiver's* own control before anything starts must park the receive
/// loop until resumed instead of draining/writing any frames.
#[tokio::test]
async fn receiver_pause_actually_stops_progress() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    // Small capacity: once the receiver stops draining, the sender soon
    // blocks on backpressure instead of buffering everything unseen.
    let (mut la, mut lb) = MemLink::pair(1);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // Pause the receiver up front so its very first loop iteration blocks in
    // wait_while_paused rather than proceeding into the frame select.
    cr.pause();

    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    let raced = tokio::time::timeout(Duration::from_millis(200), async {
        tokio::select! {
            _ = &mut send => "send",
            _ = &mut recv => "recv",
        }
    })
    .await;
    assert!(
        raced.is_err(),
        "folder receive must stay parked while the receiver is paused, not complete"
    );

    cr.resume();
    let (rs, rr) = tokio::join!(send, recv);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    let rr = rr.unwrap();
    assert_eq!(rr.outcome, TransferOutcome::Completed);
    assert_eq!(rr.files, files.len());
    for (rel, bytes) in &files {
        let dest = out.join("myfolder").join(rel);
        assert_eq!(&std::fs::read(&dest).unwrap(), bytes, "content of {rel}");
    }
}

/// Regression test for the folder receive loop never observing cancellation
/// while parked on `recv_frame` — the folder counterpart of
/// `transfer.rs`'s `cancel_interrupts_parked_receive`.
///
/// The sender pauses right after the manifest/resume-state handshake and
/// never sends a `FileHeader`, standing in for a peer that has stalled; it's
/// spawned rather than joined because its own control (`cs`) is never
/// resumed or cancelled, so it never resolves on its own. Only the
/// **receiver's** control (`cr`) is cancelled — the receive loop must
/// interrupt its own parked `recv_frame` rather than depend on anything
/// arriving from the sender.
#[tokio::test]
async fn cancel_interrupts_parked_receive() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, _files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let storage_send = storage.clone();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();
    let ptx_send = ptx.clone();

    cs.pause();
    let send_req = req(&root_path);
    let send_task = tokio::spawn(async move {
        let _ = send_folder(&mut la, &storage_send, send_req, &cs, &ptx_send, 3).await;
    });

    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx);
    let canceller = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        cr.cancel();
    };

    let (rr, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(recv, canceller)
    })
    .await
    .expect("cancel must interrupt a folder receive parked on recv_frame, not hang");

    assert_eq!(rr.unwrap().outcome, TransferOutcome::Cancelled);
    send_task.abort();
}
