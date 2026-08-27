//! End-to-end recursive folder transfer tests over an in-memory link and
//! real temp directories: structure preservation, fresh-overwrite of
//! pre-existing destination files (folder transfers are never resumed), and
//! cancel-then-rerun.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use peerbeam_domain::entity::Direction;
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

    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
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

/// Regression test: folder receives must never blind-resume from whatever
/// happens to already exist at the destination path. Before the fix, a
/// same-size pre-existing file was treated as "already complete" and left
/// untouched forever (permanently stale), and a smaller pre-existing file
/// was blind-appended-to (mixing stale bytes with fresh ones). Every file
/// must instead be written fresh (`open_write`, create/truncate).
#[tokio::test]
async fn receive_overwrites_preexisting_destination_files() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let root = base.join("myfolder");
    let f1 = pattern(1, 100 * 1024);
    let f2 = pattern(2, 50 * 1024);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("f1.bin"), &f1).unwrap();
    std::fs::write(root.join("f2.bin"), &f2).unwrap();

    // Pre-populate the destination with STALE garbage — not a legitimate
    // partial transfer. f1's garbage is the same size as the real file (the
    // old code would treat that as "already complete" and skip it, leaving
    // the garbage in place forever); f2's garbage is a different size (the
    // old code would blind-append the real bytes onto it, corrupting it).
    let out = base.join("out");
    std::fs::create_dir_all(out.join("myfolder")).unwrap();
    let stale_f1 = vec![0xAAu8; f1.len()]; // same size, wrong content
    let stale_f2 = vec![0xBBu8; 12 * 1024]; // different size, wrong content
    std::fs::write(out.join("myfolder/f1.bin"), &stale_f1).unwrap();
    std::fs::write(out.join("myfolder/f2.bin"), &stale_f2).unwrap();
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
        false,
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);

    // No per-file resume: every byte of both files crosses the wire
    // regardless of what pre-existed at the destination.
    assert_eq!(
        sent.load(Ordering::SeqCst),
        (f1.len() + f2.len()) as u64,
        "folder receives are always fresh — nothing is skipped as already complete"
    );

    // Both destination files end up exactly the fresh source content — not
    // skipped, not appended-to, not left mixed with stale bytes.
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
        let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
        let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
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
        let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
        let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
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

    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
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
        let _ = send_folder(&mut la, &storage_send, send_req, &cs, &ptx_send, 3, false).await;
    });

    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
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

/// **A ceiling must apply to a folder too.** The throttle lived only in the
/// single-file loop, so `transfer.max_send_bytes_per_sec` metered `send file`
/// and did nothing at all for `send folder/` — which is the case somebody sets a
/// ceiling *for*: a large thing going out over a link other people are using.
/// A configured ceiling that silently does not apply is worse than none, because
/// the number is right there in the config saying it does.
///
/// Asserted as "the send took time", not as a rate: a rate assertion on a shared
/// CI box measures the box. The bucket banks one second of credit, so the first
/// second's worth is free and everything past it must wait.
#[tokio::test]
async fn a_send_ceiling_slows_a_folder_send() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("big");
    std::fs::create_dir_all(&root).unwrap();
    // Two files, comfortably past one second of credit at the ceiling below.
    std::fs::write(root.join("a.bin"), vec![7u8; 300 * 1024]).unwrap();
    std::fs::write(root.join("b.bin"), vec![9u8; 300 * 1024]).unwrap();
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    // 200 KiB/s against ~600 KiB of payload: about two seconds of waiting once
    // the first second's credit is spent.
    cs.set_rate_limit(200 * 1024);
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let started = std::time::Instant::now();
    let send = send_folder(
        &mut la,
        &storage,
        FolderSendRequest {
            transfer_id: "throttled-1".into(),
            root_path: root.to_string_lossy().into_owned(),
            chunk_size: 64 * 1024,
        },
        &cs,
        &ptx,
        3,
        false,
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    let (rs, rr) = tokio::join!(send, recv);
    let elapsed = started.elapsed();

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert!(
        elapsed >= Duration::from_millis(500),
        "a folder send ignored the ceiling: finished in {elapsed:?}"
    );
    // And it still arrived whole — a throttle that dropped bytes would be a
    // worse bug than the one it fixes.
    assert_eq!(
        std::fs::read(out.join("big").join("a.bin")).unwrap().len(),
        300 * 1024
    );
}

/// **Two entries differing only in case are one file on macOS and Android.**
/// Their filesystems are case-insensitive, so a folder holding `Notes.txt` and
/// `notes.txt` had its second entry silently overwrite the first — and the
/// transfer still counted two files completed, so it reported that everything
/// arrived while the user was one file short. Linux is case-sensitive, which is
/// why this never showed up here.
///
/// The test asserts the *content* of both entries survives, which holds on
/// either kind of filesystem: on Linux the two names are distinct and both land
/// as sent; on a case-insensitive one the second is given a free name. What must
/// never happen — one of the two payloads being lost — fails on both.
#[tokio::test]
async fn two_entries_differing_only_by_case_both_survive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("notes");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Notes.txt"), b"upper").unwrap();
    std::fs::write(root.join("notes.txt"), b"lower").unwrap();

    // **The sender needs a filesystem that can hold both.** On macOS and
    // Windows those two writes are one file, so the folder being sent has a
    // single entry and there is no collision to test — the interesting case
    // there is a *case-sensitive sender* reaching a case-insensitive receiver,
    // which one machine cannot stage. Skipping says so out loud rather than
    // passing vacuously on the platforms the bug is actually about.
    if std::fs::read(root.join("Notes.txt")).unwrap() == b"lower" {
        eprintln!(
            "skipped: this filesystem is case-insensitive, so the sender cannot \
             hold two names differing only in case"
        );
        return;
    }
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_folder(
        &mut la,
        &storage,
        FolderSendRequest {
            transfer_id: "case-1".into(),
            root_path: root.to_string_lossy().into_owned(),
            chunk_size: 64 * 1024,
        },
        &cs,
        &ptx,
        3,
        false,
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    let (rs, rr) = tokio::join!(send, recv);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);

    // Both payloads are on disk somewhere under the received folder, whatever
    // the filesystem decided to call them.
    let landed: Vec<Vec<u8>> = std::fs::read_dir(out.join("notes"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| std::fs::read(e.path()).unwrap())
        .collect();
    assert!(
        landed.iter().any(|b| b == b"upper"),
        "the first entry was overwritten: {landed:?}"
    );
    assert!(
        landed.iter().any(|b| b == b"lower"),
        "the second entry was lost: {landed:?}"
    );
}

/// **An interrupted folder receive must not leave a truncated file under the
/// name a complete one would have.**
///
/// This is the defect that made `sends_a_folder_between_two_processes_over_quic`
/// fail intermittently on CI, and it was a real data-integrity bug rather than a
/// flaky test: the folder path wrote straight to the destination with
/// `open_write`, so a connection lost mid-file left a plausible-looking, short
/// file at exactly the path the user expected. `docs/SECURITY.md`'s "Safe file
/// writing" promised staging-and-rename and the single-file path did it; this
/// path never had.
///
/// The receive is cut off partway, which is the same shape as a link that dies:
/// bytes have been accepted for an entry whose `FileEnd` never arrives. What
/// must be true afterwards is not that the bytes are somewhere — they may well
/// be, in a `.part` — but that **nothing is sitting at the real name pretending
/// to be whole**.
///
/// One big file, not `build_tree`'s three small ones: the whole tree crossed a
/// `MemLink` in under 20ms, so the cancel arrived after `Completed` and the
/// interesting state was never reached. Six megabytes at a 64 KiB chunk is ~96
/// chunks through a 4-frame channel, which cannot finish inside the window.
#[tokio::test]
async fn an_interrupted_folder_receive_leaves_no_file_at_the_real_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("myfolder");
    std::fs::create_dir_all(&root).unwrap();
    let big = pattern(7, 6 * 1024 * 1024);
    std::fs::write(root.join("big.bin"), &big).unwrap();
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let storage_send = storage.clone();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();
    let ptx_send = ptx.clone();

    let send_req = req(&root.to_string_lossy());
    let send_task = tokio::spawn(async move {
        let _ = send_folder(&mut la, &storage_send, send_req, &cs, &ptx_send, 3, false).await;
    });

    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    // **Cancel on data, not on a clock.** A `MemLink` moves six megabytes in
    // well under any sleep worth writing, so a timed cancel always arrived
    // after `Completed` and never entered the window this test exists to
    // cover. Waiting for a receive-side progress report that has bytes but is
    // not yet the whole file puts the cancel exactly where it belongs, on
    // every machine.
    let canceller = async {
        while let Some(p) = prx.recv().await {
            if p.direction == Direction::Receiving && p.transferred_bytes > 0 {
                break;
            }
        }
        cr.cancel();
    };
    let (rr, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(recv, canceller)
    })
    .await
    .expect("the receive must end, not hang");
    assert_eq!(
        rr.unwrap().outcome,
        TransferOutcome::Cancelled,
        "the payload finished before the cancel landed, so the partial-write \
         window was never entered — make the file bigger"
    );
    send_task.abort();

    // The whole point: if anything exists under the real name, it is whole.
    // A partial file there is indistinguishable from a complete one to anything
    // that opens it, including the user.
    let landed = out.join("myfolder").join("big.bin");
    if let Ok(on_disk) = std::fs::read(&landed) {
        assert_eq!(
            on_disk.len(),
            big.len(),
            "{} exists under its final name with {} of {} bytes — a truncated \
             folder entry must stay staged, never be published",
            landed.display(),
            on_disk.len(),
            big.len()
        );
        assert_eq!(on_disk, big, "published file differs from the source");
    }
}

/// **With the feature negotiated, a folder send waits to be told it landed.**
///
/// This closes the other half of the truncation bug. Staging stopped a partial
/// file appearing under a real name; this stops the *sender* claiming success
/// for bytes the receiver never got. `send_frame` returns once the transport
/// accepts the bytes, and the session then closes the shared QUIC connection —
/// which quinn documents as licence for the peer to discard stream data it has
/// not yet handed to the application.
#[tokio::test]
async fn a_folder_send_waits_for_the_receiver_to_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let send = send_folder(
        &mut la,
        &storage,
        req(&root_path),
        &cs,
        &ptx,
        3,
        true, // the peer advertised TRANSFER_FEAT_FOLDER_ACK
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, true);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    for (rel, bytes) in &files {
        assert_eq!(
            &std::fs::read(out.join("myfolder").join(rel)).unwrap(),
            bytes
        );
    }
}

/// **A receiver that never confirms fails the send.** Silence must not be read
/// as success — that is exactly the bug the acknowledgement exists to close, and
/// treating a missing answer as `Completed` would reintroduce it wholesale.
///
/// Driven by letting the receiver finish and go away rather than by waiting out
/// `ACK_TIMEOUT`: the sender is told the peer supports the feature, the peer
/// sends no answer and drops the link, and the sender must fail. Same verdict,
/// same code path into `wait_for_folder_ack`, without spending thirty seconds
/// of every test run proving a constant.
#[tokio::test]
async fn a_folder_send_fails_when_the_confirmation_never_comes() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, _files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let storage_r = storage.clone();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();
    let ptx_r = ptx.clone();

    // `lb` is moved in, so it is dropped the moment the receive returns — the
    // shape of a peer that got every byte and then went away without
    // answering, which is precisely what an older build does.
    let recv_task = tokio::spawn(async move {
        receive_folder(&mut lb, &storage_r, &out_str, &cr, &ptx_r, false).await
    });

    let rs = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, true).await;
    let rr = recv_task.await.expect("receive task panicked");

    // The receive itself succeeded — the files are on disk — but the sender
    // must not claim a delivery it was never told about.
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    let err = rs.expect_err("silence must not be reported as a delivered folder");
    assert!(
        format!("{err}").contains("confirm"),
        "the failure does not say what went wrong: {err}"
    );
}

/// **A peer that predates the feature is unaffected.** Nothing is sent, nothing
/// is waited for, and the send completes exactly as it did before — which is
/// what makes this additive rather than a wire break.
#[tokio::test]
async fn a_folder_send_to_an_older_peer_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // Both false: the negotiated set ANDed the bit away.
    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    for (rel, bytes) in &files {
        assert_eq!(
            &std::fs::read(out.join("myfolder").join(rel)).unwrap(),
            bytes
        );
    }
}

/// A link that flips one byte of the **first** chunk it carries, standing in
/// for a corruption the transport did not catch.
///
/// Wrapping the receiving end rather than the sending one is deliberate: the
/// sender's digest must be computed over what it *read from disk*, so a test
/// that corrupted before hashing would prove nothing.
struct CorruptingLink {
    inner: MemLink,
    done: bool,
}

#[async_trait]
impl Link for CorruptingLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let frame = self.inner.recv_frame().await?;
        match frame {
            Some(mut f) if !self.done && f.kind == FrameKind::Chunk && !f.payload.is_empty() => {
                self.done = true;
                let mut bytes = f.payload.to_vec();
                bytes[0] ^= 0xff;
                f.payload = bytes.into();
                Ok(Some(f))
            }
            other => Ok(other),
        }
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

/// **A folder entry whose bytes changed in flight is never published, and the
/// send fails.**
///
/// Staging alone could not catch this: the entry arrives complete, so it would
/// have been flushed and renamed into place looking perfectly whole. Only a
/// digest computed by the sender over what it read, and checked by the receiver
/// before the rename, can tell the difference — which is why "arrived" and
/// "correct" are two different guarantees.
#[tokio::test]
async fn a_corrupted_folder_entry_is_withheld_and_fails_the_send() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("myfolder");
    std::fs::create_dir_all(&root).unwrap();
    let payload = pattern(5, 40 * 1024);
    std::fs::write(root.join("only.bin"), &payload).unwrap();
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, lb) = MemLink::pair(4);
    let mut lb = CorruptingLink {
        inner: lb,
        done: false,
    };
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
        true,
    );
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, true);
    let (rs, rr) = tokio::join!(send, recv);

    // The receive completes — every frame arrived — but the entry is withheld.
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert!(
        !out.join("myfolder").join("only.bin").exists(),
        "a corrupted entry was published under its real name"
    );

    // And the sender is told, rather than reporting a folder it did not deliver.
    let err = rs.expect_err("a corrupted folder must not be reported as sent");
    let msg = format!("{err}");
    assert!(
        msg.contains("checksum"),
        "the failure does not name the cause: {msg}"
    );
}

/// **An older sender sends no digest, and its folders still land.** There is
/// nothing to verify, and refusing the entry would punish the user for the
/// peer's age — the same rule every other negotiated feature here follows.
#[tokio::test]
async fn a_folder_from_a_sender_with_no_checksums_still_lands() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    // `false`/`false` is the negotiated state against a peer that predates all
    // of this; the sender still emits `FileEnd`, just without a digest.
    let send = send_folder(&mut la, &storage, req(&root_path), &cs, &ptx, 3, false);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx, false);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    for (rel, bytes) in &files {
        assert_eq!(
            &std::fs::read(out.join("myfolder").join(rel)).unwrap(),
            bytes
        );
    }
}
