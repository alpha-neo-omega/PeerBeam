//! Resume: a partially-received `.part` file must be continued, not restarted,
//! and the final whole-file checksum must still verify.

mod common;

use common::{pattern, MemLink};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl, TransferOutcome};
use tokio::sync::mpsc;

async fn run(src: &std::path::Path, out: &std::path::Path, size: u64) -> (TransferOutcome, u64) {
    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();
    let req = SendRequest {
        transfer_id: "resume".into(),
        name: "src.bin".into(),
        path: src.to_string_lossy().into(),
        size,
        chunk_size: 16 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, &out_str, &ctrl_r, &ptx);
    let (rs, rr) = tokio::join!(send, recv);
    let rr = rr.unwrap();
    assert_eq!(rs.unwrap(), rr.outcome);
    (rr.outcome, rr.bytes)
}

#[tokio::test]
async fn resumes_from_partial_part_file_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(1024 * 1024); // 1 MiB
    std::fs::write(&src, &bytes).unwrap();

    // Simulate an earlier, interrupted transfer: the receiver already has the
    // first 300 KiB on disk as `<dest>.part`.
    std::fs::create_dir_all(&out).unwrap();
    let already = 300 * 1024;
    std::fs::write(out.join("src.bin.part"), &bytes[..already]).unwrap();

    let (outcome, received) = run(&src, &out, bytes.len() as u64).await;
    assert_eq!(outcome, TransferOutcome::Completed);
    assert_eq!(received, bytes.len() as u64);
    // The final file is the whole thing, byte-for-byte — the resumed prefix
    // was folded into the checksum, not re-sent as duplicate data.
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
    // The `.part` is gone once finalized.
    assert!(!out.join("src.bin.part").exists());
}

#[tokio::test]
async fn resume_when_part_already_complete_still_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(256 * 1024);
    std::fs::write(&src, &bytes).unwrap();

    // The `.part` already holds the whole file (0 remaining bytes to send).
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("src.bin.part"), &bytes).unwrap();

    let (outcome, received) = run(&src, &out, bytes.len() as u64).await;
    assert_eq!(outcome, TransferOutcome::Completed);
    assert_eq!(received, bytes.len() as u64);
    assert_eq!(std::fs::read(out.join("src.bin")).unwrap(), bytes);
}
