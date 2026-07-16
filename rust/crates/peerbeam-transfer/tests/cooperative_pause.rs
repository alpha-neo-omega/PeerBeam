//! Cooperative pause: pausing *either* side must stop *both* sides, and
//! resuming must restart both.
//!
//! Two independent signal paths make this work:
//!
//! - **Sender → receiver**, on the main stream: `send_file`/`send_folder`
//!   edge-detect their own `ctrl.paused()` transition and send a
//!   `Control::Pause`/`Resume` (or `FolderMessage::Pause`/`Resume`) frame.
//!   The receiver reacts by mirroring the status into its own `Progress`
//!   stream (never by pausing its own `ctrl` — see `stream::receive_file`'s
//!   `Control::Pause` arm for why that would deadlock).
//! - **Receiver → sender**, over a back-channel this crate doesn't touch
//!   directly: a *local* receiver-side pause edge-detects the same way and
//!   emits a `Progress{status: Paused}`/`{status: Transferring}`, which
//!   `peerbeam-ffi`'s `drive()` turns into a `BACK_PAUSE`/`BACK_RESUME`
//!   sentinel on the real back-channel. This crate can only prove its half
//!   (the `Progress` signal) since raw `send_file`/`receive_file` never call
//!   `Link::progress_sink`/`progress_source` themselves — see
//!   `peerbeam-ffi`'s `transfer::tests::drive_*` tests for the sentinel
//!   translation and the sender-side `ctrl.pause()`/`resume()` it drives.
//!
//! Both edge-detectors fire at most once per transition, so a redundant
//! signal (e.g. the sender's own echoed `Pause` when *it* was the one told
//! to pause by the back-channel) is idempotent — no signal loop.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use common::{drain, pattern, MemLink};
use peerbeam_domain::entity::{Direction, TransferStatus};
use peerbeam_domain::error::Result;
use peerbeam_domain::port::{Frame, FrameKind, Link};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file, receive_folder, send_file, send_folder, Control, FolderSendRequest, SendRequest,
    TransferControl, TransferOutcome,
};

// ── Frame-counting link wrapper ──────────────────────────────────

/// Wraps a `MemLink` and counts `Control::Pause`/`Control::Resume` frames
/// sent through it, so tests can assert the edge-detector fires exactly once
/// per transition rather than flooding the stream.
struct CountingLink {
    inner: MemLink,
    pauses: Arc<AtomicUsize>,
    resumes: Arc<AtomicUsize>,
}

#[async_trait]
impl Link for CountingLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if frame.kind == FrameKind::Control {
            if let Ok(c) = serde_json::from_slice::<Control>(&frame.payload) {
                match c {
                    Control::Pause => {
                        self.pauses.fetch_add(1, Ordering::SeqCst);
                    }
                    Control::Resume => {
                        self.resumes.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
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

/// Race both sides against a short deadline and assert neither completes.
async fn assert_stays_paused<S, R>(send: &mut S, recv: &mut R, millis: u64)
where
    S: std::future::Future + Unpin,
    R: std::future::Future + Unpin,
{
    let raced = tokio::time::timeout(Duration::from_millis(millis), async {
        tokio::select! {
            _ = send => "send",
            _ = recv => "recv",
        }
    })
    .await;
    assert!(raced.is_err(), "transfer must not complete while paused");
}

// ── Tests: single file ───────────────────────────────────────────

/// The sender pausing must stop the *receiver* too (not just the sender):
/// the receiver observes a `Control::Pause` frame on the main stream and
/// mirrors it into its own progress, never advancing further until a
/// matching `Control::Resume` arrives.
#[tokio::test]
async fn sender_pause_stops_receiver_and_resume_completes_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(2 * 1024 * 1024); // 2 MiB — many chunks if it ran on.
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (raw_a, mut lb) = MemLink::pair(2);
    let pauses = Arc::new(AtomicUsize::new(0));
    let resumes = Arc::new(AtomicUsize::new(0));
    let mut la = CountingLink {
        inner: raw_a,
        pauses: pauses.clone(),
        resumes: resumes.clone(),
    };

    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new(); // independent — never paused directly
    let (ptx, mut prx) = mpsc::unbounded_channel();

    // Pause before any chunk, exactly like the existing single-sided pause
    // tests: deterministic, since the sender's very first loop iteration
    // edge-detects and sends `Control::Pause` before streaming anything.
    ctrl_s.pause();

    let req = SendRequest {
        transfer_id: "t-coop-send-pause".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let out_str = out.to_string_lossy().to_string();
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    assert_stays_paused(&mut send, &mut recv, 150).await;

    // The receiver must have actually observed the frame (not just gone
    // silent because nothing arrived) and must not have advanced.
    let progress = drain(&mut prx);
    assert!(
        progress
            .iter()
            .any(|p| p.direction == Direction::Receiving && p.status == TransferStatus::Paused),
        "receiver must observe the sender's Control::Pause frame: {progress:?}"
    );
    assert!(
        !progress
            .iter()
            .any(|p| p.direction == Direction::Receiving
                && p.status == TransferStatus::Transferring),
        "receiver must not advance while the sender is paused: {progress:?}"
    );

    ctrl_s.resume();
    let (rs, rr) = tokio::join!(send, recv);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(
        std::fs::read(out.join("src.bin")).unwrap(),
        bytes,
        "byte-exact after a cooperative pause/resume"
    );

    // Loop-freedom: exactly one Pause and one Resume frame for one cycle —
    // the edge-detector never floods the stream.
    assert_eq!(
        pauses.load(Ordering::SeqCst),
        1,
        "exactly one Control::Pause frame"
    );
    assert_eq!(
        resumes.load(Ordering::SeqCst),
        1,
        "exactly one Control::Resume frame"
    );
}

/// Repeated pause/resume cycles must still produce exactly one frame per
/// edge each — proof this is bounded by transitions, not by chunks/time (the
/// concrete loop-freedom guarantee: an edge-triggered signal can never turn
/// into a flood no matter how many times it's toggled).
///
/// Deliberately never lets the transfer run to completion (a large file over
/// a `cap(1)` link, so a handful of short probing windows can't possibly
/// drain it) and ends the cycle paused — so this test only has to prove the
/// frame count, with no race against the transfer finishing mid-toggle to
/// worry about. `sender_pause_stops_receiver_and_resume_completes_byte_exact`
/// above already proves one full pause→resume→completion round trip.
#[tokio::test]
async fn sender_pause_resume_cycle_is_bounded_across_multiple_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    // Large relative to the chunk size, and the heaviest possible
    // backpressure (`cap(1)`): even fully unpaused, a handful of millisecond
    // -scale probing windows cannot drain this, so the transfer is still
    // running (not finished) throughout the whole toggle loop below.
    let bytes = pattern(64 * 1024 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (raw_a, mut lb) = MemLink::pair(1);
    let pauses = Arc::new(AtomicUsize::new(0));
    let resumes = Arc::new(AtomicUsize::new(0));
    let mut la = CountingLink {
        inner: raw_a,
        pauses: pauses.clone(),
        resumes: resumes.clone(),
    };

    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "t-coop-cycles".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 4 * 1024,
    };

    let out_str = out.to_string_lossy().to_string();
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    // A bare `sleep` never polls `send`/`recv`, so pausing/resuming around
    // one would toggle `ctrl_s` without either future ever having run in
    // between — nothing would actually observe the transition. Race a short
    // timeout against `select!`-polling both futures instead, so they make
    // real progress (and the edge-detector actually gets to run) during each
    // phase. Neither future is expected to resolve here (see above), but
    // guard against it anyway rather than panic-on-poll-after-completion.
    async fn drive_for<S, R>(millis: u64, send: &mut S, recv: &mut R) -> bool
    where
        S: std::future::Future + Unpin,
        R: std::future::Future + Unpin,
    {
        tokio::time::timeout(Duration::from_millis(millis), async {
            tokio::select! { _ = send => {}, _ = recv => {} }
        })
        .await
        .is_ok()
    }

    let mut finished_early = false;
    for _ in 0..3 {
        if finished_early {
            break;
        }
        ctrl_s.pause();
        finished_early |= drive_for(15, &mut send, &mut recv).await;
        if finished_early {
            break;
        }
        ctrl_s.resume();
        finished_early |= drive_for(15, &mut send, &mut recv).await;
    }
    assert!(
        !finished_early,
        "the transfer must still be running throughout this test — a 64 MiB \
         file over a cap(1) link finishing inside a few 15ms windows would \
         defeat the point of this test; widen the size/lower the chunk size \
         further if this ever trips"
    );

    let cycles = 3;
    assert_eq!(
        pauses.load(Ordering::SeqCst),
        cycles,
        "N pause edges must send exactly N Control::Pause frames, not a flood"
    );
    assert_eq!(
        resumes.load(Ordering::SeqCst),
        cycles,
        "N resume edges must send exactly N Control::Resume frames, not a flood"
    );

    // Clean up: cancel rather than leaking the still-in-flight transfer.
    ctrl_s.cancel();
    ctrl_r.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        tokio::join!(send, recv)
    })
    .await;
}

/// The receiver's *own* local pause must emit a `Progress{status: Paused}`
/// (and `{status: Transferring}` on resume) — this is the half of
/// cooperative pause `peerbeam-ffi`'s `drive()` turns into the real
/// `BACK_PAUSE`/`BACK_RESUME` back-channel sentinel (see that crate's
/// `drive_translates_receiver_pause_progress_into_back_channel_sentinels`
/// test for the other half). The receive loop must also actually still stop
/// (the pre-existing, still-required fallback behaviour for a peer that
/// doesn't support the back-channel).
#[tokio::test]
async fn receiver_local_pause_emits_progress_signal_for_the_back_channel() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(4 * 1024 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    // Small capacity: once the receiver stops draining, the sender (whose
    // own `ctrl` is never paused here) soon blocks on backpressure instead
    // of silently buffering the whole file.
    let (mut la, mut lb) = MemLink::pair(1);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();

    // Pause the receiver up front so its very first loop iteration emits the
    // Paused signal before touching any frame.
    ctrl_r.pause();

    let req = SendRequest {
        transfer_id: "t-coop-recv-pause".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };

    let out_str = out.to_string_lossy().to_string();
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    assert_stays_paused(&mut send, &mut recv, 150).await;

    let progress = drain(&mut prx);
    assert!(
        progress
            .iter()
            .any(|p| p.direction == Direction::Receiving && p.status == TransferStatus::Paused),
        "a local receiver pause must emit Progress{{status: Paused}}: {progress:?}"
    );
    assert!(
        !progress
            .iter()
            .any(|p| p.direction == Direction::Receiving
                && p.status == TransferStatus::Transferring),
        "the receive loop must actually still stop, not just report itself paused: {progress:?}"
    );

    ctrl_r.resume();
    let (rs, rr) = tokio::join!(send, recv);
    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
}

// ── Tests: folder ─────────────────────────────────────────────────

fn folder_pattern(seed: u8, size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| ((i + seed as usize) % 251) as u8)
        .collect()
}

/// Build a small source tree, return `(root_path, [(rel, bytes)])`.
fn build_tree(base: &std::path::Path) -> (String, Vec<(String, Vec<u8>)>) {
    let root = base.join("myfolder");
    let files = vec![
        ("a.txt".to_string(), folder_pattern(1, 40 * 1024)),
        ("sub/b.bin".to_string(), folder_pattern(2, 130 * 1024)),
    ];
    for (rel, bytes) in &files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    (root.to_string_lossy().to_string(), files)
}

fn folder_req(root_path: &str) -> FolderSendRequest {
    FolderSendRequest {
        transfer_id: "t-coop-folder".into(),
        root_path: root_path.to_string(),
        chunk_size: 64 * 1024,
    }
}

/// Folder counterpart of `sender_pause_stops_receiver_and_resume_completes_byte_exact`:
/// a sender-side pause on a *folder* transfer must stop the receiver via
/// `FolderMessage::Pause`, and resuming must complete every file byte-exact.
#[tokio::test]
async fn folder_sender_pause_stops_receiver_and_resume_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (root_path, files) = build_tree(dir.path());
    let out = dir.path().join("out");
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(2);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();

    ctrl_s.pause();

    let send = send_folder(&mut la, &storage, folder_req(&root_path), &ctrl_s, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    tokio::pin!(send);
    tokio::pin!(recv);

    assert_stays_paused(&mut send, &mut recv, 150).await;

    let progress = drain(&mut prx);
    assert!(
        progress
            .iter()
            .any(|p| p.direction == Direction::Receiving && p.status == TransferStatus::Paused),
        "folder receiver must observe the sender's FolderMessage::Pause: {progress:?}"
    );

    ctrl_s.resume();
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
