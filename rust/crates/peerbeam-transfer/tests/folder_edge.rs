//! Folder-transfer hardening: unicode / long / hidden filenames, deep trees,
//! empty dirs, symlink handling, unreadable source files, and destination
//! path/type collisions. Real send_folder → receive_folder over an in-memory
//! link.

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

/// Zero-byte files must arrive: `0 >= 0` used to match the "receiver already
/// has it" resume skip, so empty files silently vanished from folder sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_byte_files_are_created() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("empty.bin"), b"").unwrap();
    std::fs::write(root.join("sub/also-empty"), b"").unwrap();
    std::fs::write(root.join("real.txt"), b"data").unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "zeroes".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);
    assert_eq!(so.unwrap(), TransferOutcome::Completed);
    let fr = ro.unwrap();
    assert_eq!(fr.outcome, TransferOutcome::Completed);
    assert_eq!(fr.files, 3, "all files counted, including empty ones");

    let got = out.join("payload");
    assert_eq!(std::fs::read(got.join("real.txt")).unwrap(), b"data");
    assert_eq!(
        std::fs::metadata(got.join("empty.bin")).unwrap().len(),
        0,
        "top-level empty file created"
    );
    assert_eq!(
        std::fs::metadata(got.join("sub/also-empty")).unwrap().len(),
        0,
        "nested empty file created"
    );
}

/// A source file that becomes unreadable (deleted/locked/permission-denied)
/// between the manifest snapshot and the send loop must not abort the whole
/// folder transfer — only that file is skipped (with a warning), and it must
/// not appear as a phantom/partial entry on the receiver.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_folder_skips_unreadable_file_delivers_rest() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("good.txt"), b"fine data").unwrap();
    std::fs::write(root.join("locked.bin"), b"unreadable").unwrap();
    std::fs::set_permissions(root.join("locked.bin"), std::fs::Permissions::from_mode(0o000))
        .unwrap();

    if std::fs::read(root.join("locked.bin")).is_ok() {
        // Running as root (or another context where permission bits don't
        // block reads) — this test can't demonstrate the unreadable-file
        // path here, so skip rather than assert something not being tested.
        let _ = std::fs::set_permissions(
            root.join("locked.bin"),
            std::fs::Permissions::from_mode(0o644),
        );
        eprintln!(
            "skipping send_folder_skips_unreadable_file_delivers_rest: \
             chmod 000 did not block reads (running as root?)"
        );
        return;
    }

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "unreadable".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);

    // Restore perms so the tempdir can be cleaned up.
    let _ = std::fs::set_permissions(
        root.join("locked.bin"),
        std::fs::Permissions::from_mode(0o644),
    );

    assert_eq!(
        so.unwrap(),
        TransferOutcome::Completed,
        "one unreadable file must not abort the whole folder send"
    );
    let ro = ro.unwrap();
    assert_eq!(ro.outcome, TransferOutcome::Completed);

    let got = out.join("payload");
    assert_eq!(
        std::fs::read(got.join("good.txt")).unwrap(),
        b"fine data",
        "the readable file still arrives"
    );
    assert!(
        !got.join("locked.bin").exists(),
        "an unreadable source file must not appear as a phantom/partial entry on the receiver"
    );
}

/// A destination path that collides with an existing directory (a
/// file/dir type mismatch) must not abort the whole folder receive — only
/// that entry is skipped (with a warning), and the rest of the folder still
/// arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_folder_skips_path_type_collision_delivers_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("good.txt"), b"fine data").unwrap();
    std::fs::write(root.join("collide.bin"), b"should not land").unwrap();

    // Pre-create a DIRECTORY at the destination path where the incoming
    // file "collide.bin" wants to land — a file/dir type collision.
    std::fs::create_dir_all(out.join("payload/collide.bin")).unwrap();

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(4);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "collide".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);

    assert_eq!(
        so.unwrap(),
        TransferOutcome::Completed,
        "the collision is a receiver-side condition — the sender is unaffected"
    );
    let ro = ro.unwrap();
    assert_eq!(
        ro.outcome,
        TransferOutcome::Completed,
        "a single path collision must not abort the whole folder receive"
    );
    assert_eq!(
        ro.files, 1,
        "only the non-colliding file counts as completed"
    );

    let got = out.join("payload");
    assert_eq!(
        std::fs::read(got.join("good.txt")).unwrap(),
        b"fine data",
        "unaffected file still arrives"
    );
    assert!(
        got.join("collide.bin").is_dir(),
        "the colliding destination must be left alone, not clobbered or half-written"
    );
}
