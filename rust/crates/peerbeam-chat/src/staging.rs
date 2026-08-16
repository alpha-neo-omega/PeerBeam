//! The outbox's own copy of a file waiting to be sent.
//!
//! A queued file cannot be a path into the user's filesystem: between queueing
//! and delivery they may delete it, move it, rename it, or edit it, and a queue
//! that silently sends different bytes than the ones chosen is worse than one
//! that fails. So the bytes are copied into storage the outbox owns, and the
//! source stops mattering the moment [`StagingStore::stage`] returns.
//!
//! The copy streams (I10) — no whole file is ever in memory — and is bounded by
//! an explicit size cap and a free-space floor, because staging duplicates
//! whatever it copies. Nothing here ever writes to, moves, or deletes the
//! user's own file: it is opened for reading and nothing else.

use std::collections::HashSet;
use std::sync::Arc;

use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use peerbeam_domain::port::StorageProvider;
use peerbeam_transfer::TransferControl;
use tokio::sync::mpsc::UnboundedSender;

use crate::store::StagedFile;

/// Copy buffer. Matches the transfer engine's own read buffer so a staged copy
/// and a wire send behave alike on the same storage. This is the entire
/// memory footprint of a stage, whatever the file's size.
const COPY_BUF: usize = 64 * 1024;

/// The two bounds a stage is held to, resolved from configuration by the
/// caller so this module never reads config itself.
#[derive(Debug, Clone, Copy)]
pub struct StagingLimits {
    /// Largest file that may be staged, in bytes.
    pub max_bytes: u64,
    /// Refuse to stage if the copy would leave less than this much free.
    pub min_free_bytes: u64,
}

/// Why a stage refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    /// The file is over the configured attachment cap.
    #[error("{size} bytes is over the {max}-byte limit for a chat attachment")]
    TooLarge {
        /// The file's size (or, when a source outran its own metadata, how
        /// much had already been copied when the cap was breached).
        size: u64,
        /// The cap that was exceeded.
        max: u64,
    },
    /// Copying the file would leave the disk below its free-space floor.
    ///
    /// The message names all three numbers deliberately. The rule is
    /// `free - need < floor`, so `need` and `free` alone can describe a refusal
    /// that reads as though it should have succeeded — 5 GiB needed with 5.2 GiB
    /// free looks fine until you know 512 MiB of that must stay free. Every
    /// surface renders this string to a user (the CLI's `stage_refusal`, the
    /// FFI's `chat_status` reason), so the floor belongs here rather than being
    /// re-attached correctly by each one.
    #[error("staging {need} bytes would leave less than the {floor} bytes that must stay free (only {free} available)")]
    NotEnoughSpace {
        /// Bytes the copy would consume.
        need: u64,
        /// Bytes currently available.
        free: u64,
        /// Bytes that must still be free once the copy has landed
        /// (`StagingLimits::min_free_bytes`).
        floor: u64,
    },
    /// The stage was cancelled before it finished.
    #[error("staging cancelled")]
    Cancelled,
    /// The source could not be read, or the blob could not be written.
    #[error("staging failed: {0}")]
    Io(String),
}

/// Owns the directory the outbox's staged blobs live in.
pub struct StagingStore {
    root: String,
    storage: Arc<dyn StorageProvider>,
}

impl StagingStore {
    /// Create a store whose blobs live under `root`.
    ///
    /// The directory is not created here — the first [`stage`](Self::stage)
    /// creates it on the way to writing its blob, so a device that never
    /// attaches a file never grows an empty directory.
    #[must_use]
    pub fn new(root: String, storage: Arc<dyn StorageProvider>) -> StagingStore {
        StagingStore { root, storage }
    }

    /// Where the blob for `id` lives, or a refusal when `id` is not a bare
    /// name.
    ///
    /// The validation is not decoration. This builds a path by interpolation
    /// and `open_write` is `File::create`, so an `id` carrying `..` or a
    /// separator would truncate and replace a file **outside** the blob root,
    /// and [`stage`](Self::stage)'s failure path would then delete it — an
    /// arbitrary write and an arbitrary delete from one string. Outgoing ids
    /// are minted locally, but `FileRef.id` arrives from the peer *unvalidated*
    /// (`validate_name` guards only `FileRef.name`) and is already used as the
    /// transfer id and the record key, so the moment a received id reaches
    /// `stage` this would be remotely reachable. Rejecting here means no
    /// caller can get it wrong.
    ///
    /// The check is the round-trip through `file_name` that `validate_name`
    /// already uses for peer-supplied names — it rejects `..`, `.`, `""` and
    /// any platform separator in one test. `\` is rejected explicitly as well:
    /// it is a legal filename character on Unix but a separator on Windows, and
    /// an id must mean the same thing on every platform (I12).
    ///
    /// It also closes the last soft spot in [`sweep`](Self::sweep): an id
    /// containing a separator would land its blob in a subdirectory that sweep
    /// can neither match nor delete, leaking those bytes permanently.
    ///
    /// The result is always `/`-separated; every platform's file APIs accept
    /// that, and `sweep` never compares these strings against a path the OS
    /// spelled itself.
    fn blob_path(&self, id: &str) -> Result<String, StagingError> {
        let bare = std::path::Path::new(id)
            .file_name()
            .is_some_and(|n| n == std::ffi::OsStr::new(id));
        if !bare || id.contains('\\') {
            return Err(StagingError::Io(format!("invalid staging id: {id}")));
        }
        Ok(format!("{}/{}", self.root.trim_end_matches('/'), id))
    }

    /// Stream `source` into the outbox's storage under `id`.
    ///
    /// Refuses before copying a single byte when the file is over the cap or
    /// when copying it would breach the free-space floor — an honest refusal
    /// now beats filling the disk and failing later, since a full disk can
    /// break unrelated applications on the machine. On cancellation or any IO
    /// error the partial blob is removed, so a failed stage never leaves an
    /// orphan for the sweep to find, and never leaves a half-copied file that
    /// would later be sent as if it were whole.
    ///
    /// The returned [`StagedFile`] carries the number of bytes actually
    /// copied, not the size metadata claimed: those differ when the source is
    /// being written to concurrently, and what matters downstream is the blob.
    pub async fn stage(
        &self,
        id: &str,
        source: &str,
        limits: StagingLimits,
        cancel: &TransferControl,
        progress: &UnboundedSender<u64>,
    ) -> Result<StagedFile, StagingError> {
        // First, before a single syscall: `id` decides where we write, so a
        // malformed one must never reach the filesystem at all.
        let dest = self.blob_path(id)?;
        let meta = std::fs::metadata(source).map_err(|e| StagingError::Io(e.to_string()))?;
        if !meta.is_file() {
            // A folder has its own send path, and a fifo/socket/device node
            // has no size at all — it would stream until the disk filled.
            return Err(StagingError::Io(if meta.is_dir() {
                "folders aren't supported in chat yet — use Send folder".to_string()
            } else {
                format!("{source} is not a regular file")
            }));
        }
        let size = meta.len();
        if size > limits.max_bytes {
            return Err(StagingError::TooLarge {
                size,
                max: limits.max_bytes,
            });
        }
        if limits.min_free_bytes > 0 {
            // `None` means we could not measure; proceed rather than refuse a
            // send because a platform would not answer.
            if let Some(free) = peerbeam_platform::available_bytes(&self.root) {
                if free.saturating_sub(size) < limits.min_free_bytes {
                    return Err(StagingError::NotEnoughSpace {
                        need: size,
                        free,
                        floor: limits.min_free_bytes,
                    });
                }
            }
        }
        let name = std::path::Path::new(source)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| StagingError::Io(format!("no file name in {source}")))?;

        match self
            .copy(source, &dest, limits.max_bytes, cancel, progress)
            .await
        {
            Ok(copied) => Ok(StagedFile {
                name,
                size: copied,
                staged_path: dest,
            }),
            Err(e) => {
                // Never leave a partial blob: the sweep only knows about
                // orphans, and a half-copied file that looks staged would be
                // sent as if it were whole.
                self.remove(&dest);
                Err(e)
            }
        }
    }

    /// Open both ends and pump, then close the writer **before** returning —
    /// the caller may be about to unlink `dest`, and an open handle can block
    /// that on Windows while a buffered writer may still be holding bytes.
    ///
    /// A close failure only matters when the copy itself succeeded; otherwise
    /// the original error is the one worth reporting.
    async fn copy(
        &self,
        source: &str,
        dest: &str,
        max_bytes: u64,
        cancel: &TransferControl,
        progress: &UnboundedSender<u64>,
    ) -> Result<u64, StagingError> {
        let mut reader = self
            .storage
            .open_read(source, 0)
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        let mut writer = self
            .storage
            .open_write(dest)
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;

        // Restrict the blob before a byte lands in it, and inside the same
        // result the close below is sequenced against, so a failure here still
        // closes the writer before `stage` unlinks the file.
        let pumped = match restrict(dest) {
            Ok(()) => pump(&mut reader, &mut writer, max_bytes, cancel, progress).await,
            Err(e) => Err(e),
        };
        let closed = writer
            .close()
            .await
            .map_err(|e| StagingError::Io(e.to_string()));
        match pumped {
            Err(e) => Err(e),
            Ok(copied) => closed.map(|()| copied),
        }
    }

    /// Delete a staged blob. Best-effort: a blob already gone is success.
    pub fn remove(&self, staged_path: &str) {
        let _ = std::fs::remove_file(staged_path);
    }

    /// Delete every blob no queue entry owns, returning how many went.
    ///
    /// This is what a crash between staging and enqueue leaves behind: bytes on
    /// disk nothing will ever send and nothing will ever delete.
    ///
    /// Ownership is matched on the blob's **file name**, not on the whole path
    /// string. `keep` holds paths a queue entry recorded — built by
    /// [`blob_path`](Self::blob_path) with `/` separators — while `read_dir`
    /// yields whatever the OS spells, which on Windows is `root\id`. Comparing
    /// the two as strings would classify every owned blob as an orphan and
    /// delete files the queue still needs. Every blob is a direct child of
    /// `root` named by its message id, so the name identifies it exactly.
    ///
    /// `keep` must be the **complete** set of owned blobs. A caller that could
    /// not read the queue must not call this at all rather than call it with an
    /// empty set — an empty `keep` means "nothing is owned", and every staged
    /// file waiting to be delivered would be deleted.
    ///
    /// Build it with [`ChatStore::outbox_owned_blobs`](crate::ChatStore::outbox_owned_blobs)
    /// and nothing else. That call refuses (returns `Err`) rather than
    /// under-report, which is exactly the completeness this needs; the ordinary
    /// outbox readers deliberately *skip* an unreadable row, so a `keep` built
    /// from one of those would silently promote that row's blob to an orphan.
    ///
    /// This is blocking (`read_dir` + `remove_file`). It is called once at
    /// startup from a synchronous entry point, so it is not wrapped in
    /// `spawn_blocking`; an async caller must do that itself.
    pub fn sweep(&self, keep: &HashSet<String>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        let kept: HashSet<&str> = keep
            .iter()
            .filter_map(|p| std::path::Path::new(p).file_name().and_then(|n| n.to_str()))
            .collect();
        let mut removed = 0;
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(|n| kept.contains(n)) {
                continue;
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// Restrict a freshly-created blob to its owner.
///
/// The blob is a **plaintext** copy of a file the user may deliberately keep
/// private, and it sits in application storage for as long as it stays queued —
/// under keep-forever, indefinitely. `open_write` creates at `0644 & ~umask`,
/// which would hand every local account a readable copy of something the
/// original was protecting. Staging is the first thing in this codebase to
/// write user file *content* into app storage, and every neighbouring store
/// already does better: `FsAppStore` writes its records AEAD-encrypted *and*
/// `0600`, and `FsStorage::finalize` chmods a completed download to `0600`.
/// Privacy First is a charter commitment, not a default.
///
/// Applied before the copy rather than after it, so a partial blob left behind
/// by a `SIGKILL` mid-copy — which no cleanup path can reach — is protected
/// too. A failure is propagated rather than swallowed: writing the user's
/// plaintext where others can read it is not an acceptable degraded mode, and
/// the caller's cleanup removes the blob. Same `#[cfg(unix)]` scope as
/// `FsStorage::finalize`; Windows ACLs have no comparable one-liner.
#[cfg(unix)]
fn restrict(path: &str) -> Result<(), StagingError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| StagingError::Io(format!("restrict {path}: {e}")))
}

/// No-op where the platform has no `chmod` equivalent — see the unix version.
#[cfg(not(unix))]
fn restrict(_path: &str) -> Result<(), StagingError> {
    Ok(())
}

/// Stream reader → writer through one fixed buffer.
///
/// The buffer is allocated once and reused, so peak memory is [`COPY_BUF`] no
/// matter how large the file is (I10). Cancellation is checked every chunk, so
/// a stop lands within one buffer rather than at the end of the file.
///
/// `max_bytes` is re-checked as bytes land, not only against the size metadata
/// reported up front: a source being appended to concurrently would otherwise
/// be copied without any bound at all, which is exactly the disk-filling
/// outcome the up-front check exists to prevent.
async fn pump(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    max_bytes: u64,
    cancel: &TransferControl,
    progress: &UnboundedSender<u64>,
) -> Result<u64, StagingError> {
    let mut buf = vec![0u8; COPY_BUF];
    let mut done: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(StagingError::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .map_err(|e| StagingError::Io(e.to_string()))?;
        done = done.saturating_add(n as u64);
        if done > max_bytes {
            return Err(StagingError::TooLarge {
                size: done,
                max: max_bytes,
            });
        }
        let _ = progress.send(done);
    }
    writer
        .flush()
        .await
        .map_err(|e| StagingError::Io(e.to_string()))?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use async_trait::async_trait;
    use futures::io::{AsyncRead, AsyncWrite};
    use peerbeam_domain::error::Result as DomainResult;
    use peerbeam_domain::port::StorageProvider;
    use peerbeam_storage_fs::FsStorage;
    use peerbeam_transfer::TransferControl;
    use tempfile::TempDir;

    /// A staging store whose blob root is a tempdir of its own, plus a
    /// *separate* tempdir standing in for the user's own files.
    ///
    /// The two are deliberately not nested: every "leaves no blob" assertion
    /// reads the blob root directly, and a source file sitting inside it would
    /// make those assertions vacuous.
    fn new_staging() -> (StagingStore, TempDir, TempDir) {
        with_storage(Arc::new(FsStorage::new()))
    }

    fn with_storage(storage: Arc<dyn StorageProvider>) -> (StagingStore, TempDir, TempDir) {
        let root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let staging = StagingStore::new(
            root.path().join("blobs").to_string_lossy().into_owned(),
            storage,
        );
        (staging, root, src)
    }

    /// The blob root — deliberately a subdirectory that does **not** exist
    /// until something opens a blob for writing. That is what makes "nothing
    /// was copied" observable: an empty directory and a never-created one look
    /// identical to a file count, so a refusal that reached `open_write` and
    /// then cleaned up would be indistinguishable from one that refused before
    /// touching the filesystem at all.
    fn blob_root(root: &TempDir) -> std::path::PathBuf {
        root.path().join("blobs")
    }

    fn generous() -> StagingLimits {
        StagingLimits {
            max_bytes: u64::MAX,
            min_free_bytes: 0,
        }
    }

    /// How many blobs currently sit in the store's root.
    fn blobs(root: &TempDir) -> usize {
        std::fs::read_dir(blob_root(root))
            .map(|d| d.count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn stage_copies_the_bytes_and_survives_the_source_being_deleted() {
        let (staging, _tmp, src_dir) = new_staging();
        let src = src_dir.path().join("report.pdf");
        std::fs::write(&src, b"the original bytes").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let staged = staging
            .stage(
                "id-1",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect("staging a small file succeeds");
        assert_eq!(staged.name, "report.pdf");
        assert_eq!(staged.size, 18);

        // The whole reason staging exists.
        std::fs::remove_file(&src).unwrap();
        assert_eq!(
            std::fs::read(&staged.staged_path).unwrap(),
            b"the original bytes"
        );
    }

    #[tokio::test]
    async fn stage_refuses_a_file_over_the_cap_without_copying_anything() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("big.bin");
        std::fs::write(&src, vec![0u8; 4096]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let err = staging
            .stage(
                "id-2",
                src.to_str().unwrap(),
                StagingLimits {
                    max_bytes: 1024,
                    min_free_bytes: 0,
                },
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("over the cap must refuse");
        assert!(matches!(
            err,
            StagingError::TooLarge {
                size: 4096,
                max: 1024
            }
        ));
        assert!(
            !blob_root(&tmp).exists(),
            "a refusal must not open anything for writing"
        );
        assert!(src.exists(), "the user's own file is never touched");
    }

    #[tokio::test]
    async fn stage_refuses_when_it_would_breach_the_free_space_floor() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("a.bin");
        std::fs::write(&src, vec![0u8; 4096]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // A floor no real disk can satisfy, so the check must fire.
        let err = staging
            .stage(
                "id-3",
                src.to_str().unwrap(),
                StagingLimits {
                    max_bytes: u64::MAX,
                    min_free_bytes: u64::MAX,
                },
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("breaching the floor must refuse");
        assert!(matches!(err, StagingError::NotEnoughSpace { .. }));
        // All three numbers, because two of them describe a refusal that reads
        // as though it should have succeeded: `need` and `free` alone leave out
        // the only reason this failed. Every surface renders this string
        // verbatim, so a floor missing here is a floor missing everywhere.
        let msg = err.to_string();
        assert!(msg.contains("4096 bytes"), "the bytes needed: {msg}");
        assert!(
            msg.contains(&u64::MAX.to_string()),
            "the floor that was breached: {msg}"
        );
        assert!(
            msg.contains("available"),
            "and what is actually free: {msg}"
        );
        assert!(
            !blob_root(&tmp).exists(),
            "a refusal must not open anything for writing"
        );
        assert!(src.exists(), "the user's own file is never touched");
    }

    #[tokio::test]
    async fn a_cancelled_stage_leaves_no_orphan_blob() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("c.bin");
        std::fs::write(&src, vec![7u8; 8 * 1024 * 1024]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        ctrl.cancel(); // cancelled before the first chunk

        let err = staging
            .stage("id-4", src.to_str().unwrap(), generous(), &ctrl, &tx)
            .await
            .expect_err("a cancelled stage does not produce a blob");
        assert!(matches!(err, StagingError::Cancelled));
        // The directory legitimately exists here — the writer WAS opened, then
        // the blob was removed. What must be gone is the blob itself.
        assert_eq!(blobs(&tmp), 0);
        assert!(src.exists(), "the user's own file is never touched");
    }

    #[tokio::test]
    async fn stage_reports_progress_as_it_copies() {
        let (staging, _tmp, src_dir) = new_staging();
        let src = src_dir.path().join("d.bin");
        std::fs::write(&src, vec![1u8; 512 * 1024]).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        staging
            .stage(
                "id-5",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();
        drop(tx);
        let mut seen = Vec::new();
        while let Some(n) = rx.recv().await {
            seen.push(n);
        }
        assert!(seen.len() > 1, "a multi-chunk copy reports more than once");
        assert_eq!(*seen.last().unwrap(), 512 * 1024);

        // I10, asserted rather than assumed: every step of the copy moved at
        // most one buffer. An implementation that read the file into memory
        // first would report the whole 512 KiB in a single jump, and one that
        // grew its buffer would show increasing steps.
        let cap = COPY_BUF as u64;
        assert!(
            seen[0] <= cap,
            "the first write lands after one buffer, not after the whole file"
        );
        assert!(
            seen.windows(2).all(|w| w[1] - w[0] <= cap),
            "no single step may move more than one buffer: {seen:?}"
        );
    }

    #[tokio::test]
    async fn sweep_deletes_blobs_no_queue_entry_owns() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("e.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let keep = staging
            .stage(
                "keep-me",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();
        let orphan = staging
            .stage(
                "orphan",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();

        // What a crash between staging and enqueue leaves behind.
        let mut owned = HashSet::new();
        owned.insert(keep.staged_path.clone());
        assert_eq!(staging.sweep(&owned), 1);
        assert!(std::path::Path::new(&keep.staged_path).exists());
        assert!(!std::path::Path::new(&orphan.staged_path).exists());
        let _ = tmp;
    }

    /// `keep` holds whatever a queue entry recorded, which may not be spelled
    /// byte-for-byte the way `read_dir` spells the same file — most sharply on
    /// Windows, where this store builds `root/id` with a forward slash while
    /// `read_dir` yields `root\id`. A raw string comparison would see every
    /// owned blob as an orphan and delete files the queue still needs, so the
    /// match is by file name. A redundant separator reproduces the same
    /// mismatch on a platform where `\` is not a separator at all.
    #[tokio::test]
    async fn sweep_keeps_a_blob_whose_recorded_path_is_spelled_differently() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("f.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let keep = staging
            .stage(
                "keep-me",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();

        let mut owned = HashSet::new();
        owned.insert(keep.staged_path.replace("/keep-me", "//keep-me"));
        assert_eq!(
            staging.sweep(&owned),
            0,
            "an equivalent spelling of the same path still owns the blob"
        );
        assert!(std::path::Path::new(&keep.staged_path).exists());
        assert_eq!(blobs(&tmp), 1);
    }

    #[tokio::test]
    async fn remove_deletes_a_blob_and_a_missing_blob_is_not_an_error() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("g.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let staged = staging
            .stage(
                "id-6",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();

        staging.remove(&staged.staged_path);
        assert_eq!(blobs(&tmp), 0);
        staging.remove(&staged.staged_path); // already gone: still fine
        staging.remove("/no/such/blob");
    }

    /// A directory is not a chat attachment, and neither is a fifo, socket or
    /// device node — the last of which would stream forever. All are refused
    /// at the gate, before anything is opened for writing.
    #[tokio::test]
    async fn stage_refuses_anything_that_is_not_a_regular_file() {
        let (staging, tmp, src_dir) = new_staging();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = src_dir.path().join("a-folder");
        std::fs::create_dir(&dir).unwrap();

        let err = staging
            .stage(
                "id-7",
                dir.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("a folder is not a chat attachment");
        assert!(matches!(err, StagingError::Io(_)));
        assert!(
            !blob_root(&tmp).exists(),
            "a refusal must not open anything for writing"
        );

        // A source that does not exist at all is an ordinary IO refusal.
        let missing = src_dir.path().join("nope.bin");
        let err = staging
            .stage(
                "id-8",
                missing.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("a missing source must refuse");
        assert!(matches!(err, StagingError::Io(_)));
        assert!(
            !blob_root(&tmp).exists(),
            "a refusal must not open anything for writing"
        );
    }

    /// `blob_path` interpolates `id` into a path and `open_write` is
    /// `File::create`, so an id carrying `..` or a separator would truncate and
    /// replace a file OUTSIDE the blob root — and the failure path's `remove`
    /// would then delete it. One string, an arbitrary write and an arbitrary
    /// delete.
    ///
    /// Unreachable today (nothing calls `stage`, and outgoing ids come from
    /// `mint_id`), but `FileRef.id` arrives from the peer UNVALIDATED —
    /// `validate_name` guards only `FileRef.name` — and is already used as the
    /// transfer id and the record key. The moment a received id reaches
    /// `stage`, this is a remote write-anywhere.
    #[tokio::test]
    async fn stage_refuses_an_id_that_is_not_a_bare_name() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("payload.bin");
        std::fs::write(&src, b"ATTACKER BYTES").unwrap();
        // A file outside the blob root that must survive every attempt.
        let victim = src_dir.path().join("important.txt");
        std::fs::write(&victim, b"ORIGINAL").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        for id in [
            "..",
            "../escape",
            "../important.txt",
            "a/b",
            "a\\b",
            ".",
            "",
            "/etc/passwd",
        ] {
            let err = staging
                .stage(
                    id,
                    src.to_str().unwrap(),
                    generous(),
                    &TransferControl::new(),
                    &tx,
                )
                .await
                .expect_err("an id that is not a bare name must refuse");
            assert!(matches!(err, StagingError::Io(_)), "id {id:?} -> {err:?}");
            assert!(
                !blob_root(&tmp).exists(),
                "id {id:?} must not open anything for writing"
            );
        }
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"ORIGINAL",
            "no file outside the blob root is ever touched"
        );

        // The positive case: a normal minted id still works, so the guard
        // cannot pass merely by refusing everything.
        let ok = staging
            .stage(
                "0000000000001",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect("a bare, minted id stages normally");
        assert!(ok.staged_path.ends_with("/0000000000001"));
        assert_eq!(blobs(&tmp), 1);
    }

    /// A staged blob is a plaintext copy of a file the user may deliberately
    /// keep private, parked in app storage for as long as it stays queued —
    /// indefinitely, under keep-forever. `open_write` creates at `0644 &
    /// ~umask`; both neighbouring stores already do better (`FsAppStore` writes
    /// records `0600`, `FsStorage::finalize` chmods downloads to `0600`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_staged_blob_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("private.txt");
        std::fs::write(&src, b"secret").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let staged = staging
            .stage(
                "id-13",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();
        let mode = std::fs::metadata(&staged.staged_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a staged blob must not be readable by other local accounts"
        );
        assert_eq!(std::fs::read(&staged.staged_path).unwrap(), b"secret");
        assert_eq!(blobs(&tmp), 1);
    }

    /// A source whose bytes outrun its own metadata — a log still being
    /// written, or a stream masquerading as a file — must be stopped by the
    /// cap, not by the disk filling up. Proving this needs a reader the real
    /// filesystem cannot produce, so the test supplies its own
    /// [`StorageProvider`].
    #[tokio::test]
    async fn a_source_that_never_ends_is_stopped_by_the_cap_and_leaves_no_blob() {
        let storage: Arc<dyn StorageProvider> = Arc::new(ScriptedSource::endless());
        let (staging, tmp, src_dir) = with_storage(storage);
        // Metadata says one byte, so the up-front check waves it through.
        let src = src_dir.path().join("growing.log");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let err = staging
            .stage(
                "id-9",
                src.to_str().unwrap(),
                StagingLimits {
                    max_bytes: 1024 * 1024,
                    min_free_bytes: 0,
                },
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("an endless source must be cut off, not copied forever");
        assert!(matches!(err, StagingError::TooLarge { .. }), "{err:?}");
        assert_eq!(blobs(&tmp), 0, "the partial blob is removed");
    }

    /// The read failing partway is the ordinary version of "the source stopped
    /// being readable mid-copy" (unplugged drive, revoked permission). What
    /// must never survive it is a half-copied blob that looks whole: the sweep
    /// only knows about orphans, and this one would have an owner.
    #[tokio::test]
    async fn a_read_that_fails_partway_leaves_no_half_copied_blob() {
        let storage: Arc<dyn StorageProvider> = Arc::new(ScriptedSource::fails_after(200_000));
        let (staging, tmp, src_dir) = with_storage(storage);
        let src = src_dir.path().join("flaky.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let err = staging
            .stage(
                "id-10",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .expect_err("a failed read must fail the stage");
        assert!(matches!(err, StagingError::Io(_)), "{err:?}");
        assert_eq!(blobs(&tmp), 0, "no half-copied blob survives");
    }

    /// Cancellation partway through is the realistic case (the user hits stop
    /// on a large file); the earlier test only covers cancelling before the
    /// first chunk. Both must end with nothing on disk.
    #[tokio::test]
    async fn cancelling_partway_through_leaves_no_blob() {
        let storage: Arc<dyn StorageProvider> = Arc::new(ScriptedSource::endless());
        let (staging, tmp, src_dir) = with_storage(storage);
        let src = src_dir.path().join("big.bin");
        std::fs::write(&src, b"x").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let ctrl = TransferControl::new();
        let watcher = ctrl.clone();
        tokio::spawn(async move {
            // Cancel only once the copy is demonstrably under way.
            while let Some(n) = rx.recv().await {
                if n >= 256 * 1024 {
                    break;
                }
            }
            watcher.cancel();
        });

        let err = staging
            .stage(
                "id-11",
                src.to_str().unwrap(),
                StagingLimits {
                    max_bytes: u64::MAX,
                    min_free_bytes: 0,
                },
                &ctrl,
                &tx,
            )
            .await
            .expect_err("a cancelled copy must not produce a blob");
        assert!(matches!(err, StagingError::Cancelled), "{err:?}");
        assert_eq!(blobs(&tmp), 0);
    }

    /// An empty file is a real thing a user can share; it must stage cleanly
    /// rather than fall out of a loop that assumes at least one chunk.
    #[tokio::test]
    async fn an_empty_file_stages_to_an_empty_blob() {
        let (staging, tmp, src_dir) = new_staging();
        let src = src_dir.path().join("empty.txt");
        std::fs::write(&src, b"").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let staged = staging
            .stage(
                "id-12",
                src.to_str().unwrap(),
                generous(),
                &TransferControl::new(),
                &tx,
            )
            .await
            .unwrap();
        assert_eq!(staged.size, 0);
        assert_eq!(staged.name, "empty.txt");
        assert_eq!(std::fs::read(&staged.staged_path).unwrap(), b"");
        assert_eq!(blobs(&tmp), 1);
    }

    // ── A StorageProvider the test scripts, so the copy loop can be driven by
    // a reader no real filesystem produces. ─────────────────────────────────

    /// Writes through to the real filesystem, but reads from a scripted
    /// source: either one that never ends, or one that fails after N bytes.
    struct ScriptedSource {
        inner: FsStorage,
        script: Script,
    }

    #[derive(Clone, Copy)]
    enum Script {
        Endless,
        FailsAfter(usize),
    }

    impl ScriptedSource {
        fn endless() -> Self {
            ScriptedSource {
                inner: FsStorage::new(),
                script: Script::Endless,
            }
        }
        fn fails_after(n: usize) -> Self {
            ScriptedSource {
                inner: FsStorage::new(),
                script: Script::FailsAfter(n),
            }
        }
    }

    #[async_trait]
    impl StorageProvider for ScriptedSource {
        async fn open_write(&self, path: &str) -> DomainResult<Box<dyn AsyncWrite + Unpin + Send>> {
            self.inner.open_write(path).await
        }
        async fn open_append(
            &self,
            path: &str,
        ) -> DomainResult<Box<dyn AsyncWrite + Unpin + Send>> {
            self.inner.open_append(path).await
        }
        async fn open_read(
            &self,
            _path: &str,
            _offset: u64,
        ) -> DomainResult<Box<dyn AsyncRead + Unpin + Send>> {
            Ok(match self.script {
                Script::Endless => Box::new(ScriptedReader { remaining: None }),
                Script::FailsAfter(n) => Box::new(ScriptedReader { remaining: Some(n) }),
            })
        }
        async fn size(&self, path: &str) -> DomainResult<Option<u64>> {
            self.inner.size(path).await
        }
        async fn list_files(&self, root: &str) -> DomainResult<Vec<(String, u64)>> {
            self.inner.list_files(root).await
        }
        async fn finalize(&self, temp: &str, dest: &str) -> DomainResult<String> {
            self.inner.finalize(temp, dest).await
        }
    }

    /// `None` remaining: yields zeros forever. `Some(n)`: yields `n` zeros and
    /// then fails.
    struct ScriptedReader {
        remaining: Option<usize>,
    }

    impl AsyncRead for ScriptedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            let n = match self.remaining {
                None => buf.len(),
                Some(0) => {
                    return Poll::Ready(Err(io::Error::other("source stopped being readable")))
                }
                Some(left) => buf.len().min(left),
            };
            buf[..n].fill(0);
            if let Some(left) = self.remaining.as_mut() {
                *left -= n;
            }
            Poll::Ready(Ok(n))
        }
    }
}
