//! Folder-transfer hardening: unicode / long / hidden filenames, deep trees,
//! empty dirs, symlink handling, unreadable source files, destination
//! path/type collisions, and a destination that cannot be flushed. Real
//! send_folder → receive_folder over an in-memory link.

mod common;

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};

use bytes::Bytes;
use common::{pattern, MemLink};
use peerbeam_domain::error::Result;
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};
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
    std::fs::set_permissions(
        root.join("locked.bin"),
        std::fs::Permissions::from_mode(0o000),
    )
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

/// A writer that takes every byte and only fails when asked to make them
/// durable — the shape of a disk that fills up, a quota that is reached, or a
/// volume that goes away mid-folder. Buffered writers behave exactly like
/// this: `write_all` succeeds against the buffer, and the truth arrives at
/// flush time.
struct DiskFullWriter;

impl AsyncWrite for DiskFullWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("No space left on device")))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("No space left on device")))
    }
}

/// Real storage in every respect except that nothing it writes can be made
/// durable. Delegating the rest to [`FsStorage`] keeps the double honest: the
/// receive path under test is the real one, with one failure injected.
struct FullDiskStorage(FsStorage);

#[async_trait::async_trait]
impl StorageProvider for FullDiskStorage {
    async fn open_write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        Ok(Box::new(DiskFullWriter))
    }
    async fn open_append(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        Ok(Box::new(DiskFullWriter))
    }
    async fn open_read(
        &self,
        path: &str,
        offset: u64,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>> {
        self.0.open_read(path, offset).await
    }
    async fn size(&self, path: &str) -> Result<Option<u64>> {
        self.0.size(path).await
    }
    async fn list_files(&self, root: &str) -> Result<Vec<(String, u64)>> {
        self.0.list_files(root).await
    }
    async fn finalize(&self, temp: &str, dest: &str) -> Result<String> {
        self.0.finalize(temp, dest).await
    }
}

/// A folder receive whose files cannot be flushed must FAIL, not report a
/// completed folder.
///
/// `close_writer` used to drop both `flush()` and `close()`, so a disk that
/// filled up mid-folder produced `outcome: Completed` with every entry
/// counted: the CLI printed "received folder payload (2 files)", the app
/// settled the transfer as done, and the sender was free to delete its copy of
/// files whose tails never reached the platter. `write_all` failures have
/// always propagated and `stream::receive_file` has always propagated flush
/// and close — this is the same failure on the folder path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_receive_that_cannot_flush_fails_instead_of_reporting_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("first.txt"), pattern(64)).unwrap();
    std::fs::write(root.join("second.txt"), pattern(96)).unwrap();

    // A generous cap, not the usual 4: the receiver gives up at the first
    // `FileEnd`, and a sender blocked forever on a full in-memory channel
    // would hang the test rather than fail it.
    let (mut la, mut lb) = MemLink::pair(64);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "nospace".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let sender_storage = FsStorage::new();
    let receiver_storage = FullDiskStorage(FsStorage::new());
    let send = send_folder(&mut la, &sender_storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &receiver_storage, &out_str, &cr, &ptx2);
    let (_so, ro) = tokio::join!(send, recv);

    let err = ro.expect_err("a folder whose files could not be flushed must not report success");
    let msg = err.to_string();
    assert!(
        msg.contains("No space left on device"),
        "the failure must name what actually went wrong, got: {msg}"
    );
}

/// A colon is legal in a Unix filename, and rejecting it used to abort the
/// whole receive: one `service 14:30:02.log` in a shared folder lost every
/// file behind it. All three files must arrive.
///
/// Unix only, because the point is a source file Windows cannot even create.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_colon_in_a_name_does_not_kill_the_rest_of_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(root.join("logs")).unwrap();
    // Sorted order (`list_files` sorts): the awkward name sits between the two
    // ordinary ones, so an abort is visible as a *missing* `z-last.txt`.
    std::fs::write(root.join("a-first.txt"), b"first").unwrap();
    std::fs::write(root.join("logs/service 14:30:02.log"), b"log line").unwrap();
    std::fs::write(root.join("z-last.txt"), b"last").unwrap();

    let storage = FsStorage::new();
    // Capacity above the whole frame count on purpose: a receiver that gives
    // up early must not leave the sender blocked on a full channel, or a
    // regression here would hang the test instead of failing it.
    let (mut la, mut lb) = MemLink::pair(64);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "colon".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);

    assert_eq!(so.unwrap(), TransferOutcome::Completed);
    let ro = ro.unwrap();
    assert_eq!(ro.outcome, TransferOutcome::Completed);
    assert_eq!(ro.files, 3, "every file arrives, awkward name included");

    let got = out.join("payload");
    assert_eq!(std::fs::read(got.join("a-first.txt")).unwrap(), b"first");
    assert_eq!(
        std::fs::read(got.join("logs/service 14:30:02.log")).unwrap(),
        b"log line",
        "a name a Unix receiver can hold is written as sent"
    );
    assert_eq!(
        std::fs::read(got.join("z-last.txt")).unwrap(),
        b"last",
        "the files behind the awkward one are not lost"
    );
}

/// Rewrites the `path` of every `FileHeader` on its way out, so the receiver
/// can be handed a header no conforming sender would ever produce. Folder
/// messages are JSON, so a substring swap is enough — and driving a real
/// `send_folder` keeps the rest of the conversation (manifest, chunks,
/// `FileEnd`) exactly as the receiver expects it.
struct HeaderPathRewriter {
    inner: MemLink,
    from: &'static str,
    to: &'static str,
}

#[async_trait::async_trait]
impl Link for HeaderPathRewriter {
    async fn send_frame(&mut self, mut frame: Frame) -> Result<()> {
        if frame.kind == FrameKind::Control {
            let json = String::from_utf8_lossy(&frame.payload).into_owned();
            if json.starts_with(r#"{"FileHeader""#) {
                frame.payload = Bytes::from(json.replace(self.from, self.to));
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

/// A `FileHeader` whose path cannot be made safe (here a `..` traversal) must
/// skip that one entry, not abort the folder: nothing is written for it either
/// way, so refusing the whole transfer only costs the user the files behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsafe_path_skips_only_that_entry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("payload");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&root).unwrap();
    // Sorted first, so an abort takes `zz-good.txt` down with it.
    std::fs::write(root.join("aa-evil.bin"), b"traversal").unwrap();
    std::fs::write(root.join("zz-good.txt"), b"innocent").unwrap();

    let storage = FsStorage::new();
    // See the capacity note in `a_colon_in_a_name_does_not_kill_the_rest_of_the_folder`.
    let (la, mut lb) = MemLink::pair(64);
    let mut la = HeaderPathRewriter {
        inner: la,
        from: "aa-evil.bin",
        to: "../escaped.bin",
    };
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();

    let req = FolderSendRequest {
        transfer_id: "unsafe".into(),
        root_path: root.to_string_lossy().into(),
        chunk_size: 64 * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let send = send_folder(&mut la, &storage, req, &cs, &ptx, 3);
    let recv = receive_folder(&mut lb, &storage, &out_str, &cr, &ptx2);
    let (so, ro) = tokio::join!(send, recv);

    assert_eq!(so.unwrap(), TransferOutcome::Completed);
    let ro = ro.unwrap();
    assert_eq!(
        ro.outcome,
        TransferOutcome::Completed,
        "one unsafe path must not abort the whole folder receive"
    );
    assert_eq!(ro.files, 1, "the skipped entry is not counted as completed");

    assert_eq!(
        std::fs::read(out.join("payload/zz-good.txt")).unwrap(),
        b"innocent",
        "the entry behind the unsafe one still arrives"
    );
    assert!(
        !out.join("escaped.bin").exists(),
        "the traversal target must never be written"
    );
    assert!(
        !out.join("payload/aa-evil.bin").exists(),
        "and nothing lands under the original name either"
    );
}
