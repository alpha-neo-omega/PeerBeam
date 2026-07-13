//! Stress: many transfers running at once must each complete and verify
//! independently, with no cross-talk between concurrent pipelines.

mod common;

use common::{pattern, MemLink};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl, TransferOutcome};
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_concurrent_transfers_all_verify() {
    const N: usize = 16;
    let dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new();

    // Each transfer gets a distinct payload size + content so a mix-up between
    // pipelines would surface as a byte or length mismatch.
    let mut sources = Vec::new();
    for i in 0..N {
        let bytes = pattern(64 * 1024 + i * 4096 + i);
        let src = dir.path().join(format!("src-{i}.bin"));
        std::fs::write(&src, &bytes).unwrap();
        sources.push((i, src, bytes));
    }

    let transfers = sources.iter().map(|(i, src, bytes)| {
        let storage = &storage;
        let out = dir.path().join(format!("out-{i}"));
        async move {
            let (mut la, mut lb) = MemLink::pair(4);
            let ctrl_s = TransferControl::new();
            let ctrl_r = TransferControl::new();
            let (ptx, _prx) = mpsc::unbounded_channel();
            let req = SendRequest {
                transfer_id: format!("t{i}"),
                name: format!("src-{i}.bin"),
                path: src.to_string_lossy().into(),
                size: bytes.len() as u64,
                chunk_size: 8 * 1024,
            };
            let out_str = out.to_string_lossy().to_string();
            let send = send_file(&mut la, storage, req, &ctrl_s, &ptx, 3);
            let recv = receive_file(&mut lb, storage, &out_str, &ctrl_r, &ptx);
            let (rs, rr) = tokio::join!(send, recv);
            assert_eq!(rs.unwrap(), TransferOutcome::Completed, "send {i}");
            assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed, "recv {i}");
            let written = std::fs::read(out.join(format!("src-{i}.bin"))).unwrap();
            assert_eq!(&written, bytes, "payload {i} must match byte-for-byte");
        }
    });

    futures::future::join_all(transfers).await;
}
