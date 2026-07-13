//! Folder-transfer hardening: unicode / long / hidden filenames, deep trees,
//! empty dirs, and symlink handling. Real send_folder → receive_folder over an
//! in-memory link.

mod common;

use common::{pattern, MemLink};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_folder, send_folder, FolderSendRequest, TransferControl, TransferOutcome,
};
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfers_edge_case_filenames_and_trees() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&root).unwrap();

    // Edge-case files.
    let unicode = "café-日本語-😀.txt";
    let long = format!("{}.dat", "x".repeat(200));
    let hidden = ".hidden-config";
    let deep_rel = "a/b/c/d/e/deep.bin";
    std::fs::write(root.join(unicode), pattern(1234)).unwrap();
    std::fs::write(root.join(&long), pattern(2048)).unwrap();
    std::fs::write(root.join(hidden), b"secret-ish").unwrap();
    std::fs::create_dir_all(root.join("a/b/c/d/e")).unwrap();
    std::fs::write(root.join(deep_rel), pattern(4096)).unwrap();
    // An empty directory (walk lists files only → not recreated; documented).
    std::fs::create_dir_all(root.join("empty-dir")).unwrap();
    // A symlink (must be skipped, never followed — no exfiltration).
    #[cfg(unix)]
    {
        std::fs::write(dir.path().join("outside-secret"), b"DO NOT SEND").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside-secret"), root.join("link")).unwrap();
    }

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "edge".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);
    assert_eq!(so.unwrap(), TransferOutcome::Completed);
    assert_eq!(ro.unwrap().outcome, TransferOutcome::Completed);

    let got = out.join("payload");
    assert_eq!(
        std::fs::read(got.join(unicode)).unwrap(),
        pattern(1234),
        "unicode name"
    );
    assert_eq!(
        std::fs::read(got.join(&long)).unwrap(),
        pattern(2048),
        "long name"
    );
    assert_eq!(
        std::fs::read(got.join(hidden)).unwrap(),
        b"secret-ish",
        "hidden file"
    );
    assert_eq!(
        std::fs::read(got.join(deep_rel)).unwrap(),
        pattern(4096),
        "deep tree"
    );

    // Empty dirs are not recreated (walk is file-only) — known, documented.
    assert!(
        !got.join("empty-dir").exists(),
        "empty dirs not transferred"
    );

    // Symlinks are skipped, never followed → the outside file's content never
    // arrives as a regular file.
    #[cfg(unix)]
    {
        let link_dst = got.join("link");
        if link_dst.exists() {
            assert_ne!(
                std::fs::read(&link_dst).unwrap(),
                b"DO NOT SEND",
                "a symlink target must never be transferred as content"
            );
        }
    }
}
