//! Regression tests — pin fixes for specific past/again-possible bugs.

mod common;

use common::{pattern, MemLink};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl, TransferOutcome};
use tokio::sync::mpsc;

/// A malicious sender that names the file with a traversal path must not be
/// able to write outside the destination directory: the receiver keeps only
/// the base name.
#[tokio::test]
async fn receive_ignores_traversal_in_sender_filename() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("payload.bin");
    let out = dir.path().join("downloads");
    let bytes = pattern(64 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "evil".into(),
        // Attacker-chosen name trying to escape the download dir.
        name: "../../../../tmp/pwned.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 16 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    let received = rr.unwrap();
    assert_eq!(received.outcome, TransferOutcome::Completed);

    // Written safely under the destination as the base name only.
    assert_eq!(received.name, "pwned.bin");
    assert!(out.join("pwned.bin").exists());
    // Nothing escaped upward, and the whole payload arrived intact.
    assert!(!dir.path().join("pwned.bin").exists());
    assert_eq!(std::fs::read(out.join("pwned.bin")).unwrap(), bytes);
}
