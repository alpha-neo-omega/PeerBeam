//! Filesystem [`StorageProvider`].
//!
//! Opens streamed async readers/writers backed by `tokio::fs`, bridged to the
//! `futures` IO traits the domain port speaks (via `tokio_util::compat`).
//! Nothing here buffers a whole file: callers read/write chunk by chunk, so
//! transfers stay memory-bounded regardless of file size.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, SeekFrom};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::StorageProvider;

/// A [`StorageProvider`] that reads and writes real files on disk.
#[derive(Debug, Default, Clone)]
pub struct FsStorage;

impl FsStorage {
    /// Create a filesystem storage provider.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl StorageProvider for FsStorage {
    async fn open_write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                DomainError::Storage(format!("create dir {}: {e}", parent.display()))
            })?;
        }
        let file = tokio::fs::File::create(path)
            .await
            .map_err(|e| DomainError::Storage(format!("create {path}: {e}")))?;
        Ok(Box::new(file.compat_write()))
    }

    async fn open_append(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                DomainError::Storage(format!("create dir {}: {e}", parent.display()))
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|e| DomainError::Storage(format!("append {path}: {e}")))?;
        Ok(Box::new(file.compat_write()))
    }

    async fn open_read(
        &self,
        path: &str,
        offset: u64,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| DomainError::Storage(format!("open {path}: {e}")))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| DomainError::Storage(format!("seek {path}: {e}")))?;
        }
        Ok(Box::new(file.compat()))
    }

    async fn size(&self, path: &str) -> Result<Option<u64>> {
        match tokio::fs::metadata(path).await {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DomainError::Storage(format!("stat {path}: {e}"))),
        }
    }

    async fn finalize(&self, temp: &str, dest: &str) -> Result<String> {
        // Atomically reserves the chosen name (an empty placeholder) so a
        // concurrent finalize racing for the same `dest` can never pick the
        // same candidate — see `unique_path`'s doc comment.
        let final_path = unique_path(dest).await?;

        // Restrict permissions on the completed file before it becomes visible.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(temp, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|e| DomainError::Storage(format!("set perms {temp}: {e}")))?;
        }

        // Replaces the (empty) placeholder we just reserved — this is the
        // only rename onto `final_path`, so it can't race another finalize.
        tokio::fs::rename(temp, &final_path)
            .await
            .map_err(|e| DomainError::Storage(format!("finalize {temp} -> {final_path}: {e}")))?;
        Ok(final_path)
    }

    async fn list_files(&self, root: &str) -> Result<Vec<(String, u64)>> {
        let root_path = PathBuf::from(root);
        // Canonicalized once so a symlink's resolved target can be checked
        // against it below: a "send this folder" request must never let a
        // symlink inside the tree exfiltrate a file that lives outside it.
        let root_real = tokio::fs::canonicalize(&root_path).await.map_err(|e| {
            DomainError::Storage(format!("canonicalize {}: {e}", root_path.display()))
        })?;
        let mut out = Vec::new();
        let mut stack = vec![root_path.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir)
                .await
                .map_err(|e| DomainError::Storage(format!("read dir {}: {e}", dir.display())))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| DomainError::Storage(format!("read entry: {e}")))?
            {
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| DomainError::Storage(format!("file type: {e}")))?;

                // `file_type()` does NOT follow symlinks, so a symlink to a
                // regular file is neither `is_dir()` nor `is_file()` — stat
                // through it via `metadata()` (which does follow) to classify
                // it correctly instead of silently dropping it from the list.
                // A symlinked *directory* is never traversed (cycle risk,
                // e.g. `a/link -> a`), and a symlinked *file* is only
                // followed when its resolved target stays inside `root` —
                // otherwise a folder send could be used to exfiltrate an
                // arbitrary file via a symlink (see folder_edge.rs's
                // "a symlink target must never be transferred as content").
                let meta = if file_type.is_symlink() {
                    let real = match tokio::fs::canonicalize(&path).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skipping unresolvable symlink"
                            );
                            continue;
                        }
                    };
                    if !real.starts_with(&root_real) {
                        tracing::warn!(
                            path = %path.display(),
                            target = %real.display(),
                            "skipping symlink whose target escapes the shared folder"
                        );
                        continue;
                    }
                    match tokio::fs::metadata(&path).await {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skipping unresolvable symlink"
                            );
                            continue;
                        }
                    }
                } else if file_type.is_dir() {
                    stack.push(path);
                    continue;
                } else if file_type.is_file() {
                    entry
                        .metadata()
                        .await
                        .map_err(|e| DomainError::Storage(format!("metadata: {e}")))?
                } else {
                    // fifo/socket/device/etc: skip with a log instead of a
                    // silent drop.
                    tracing::warn!(path = %path.display(), "skipping non-regular entry");
                    continue;
                };

                if meta.is_file() {
                    let size = meta.len();
                    let rel = path
                        .strip_prefix(&root_path)
                        .map_err(|e| DomainError::Storage(format!("strip prefix: {e}")))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, size));
                } else if meta.is_dir() {
                    // A symlink to a directory inside root: still not
                    // traversed (cycle risk), even though it passed the
                    // containment check above.
                    tracing::warn!(path = %path.display(), "skipping symlinked directory");
                }
            }
        }

        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// Try to atomically claim `path` by creating it as a new, empty file.
///
/// `Ok(true)` — we reserved it (nothing existed there a moment ago).
/// `Ok(false)` — something already exists there; the caller must pick another
/// candidate name. Using `create_new` (which maps to `O_CREAT|O_EXCL` on
/// Unix) makes the existence check and the claim a single atomic syscall, so
/// two concurrent callers can never both believe they reserved the same name.
async fn try_reserve(path: &str) -> Result<bool> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(DomainError::Storage(format!("reserve {path}: {e}"))),
    }
}

/// Reserve `dest` if free, else the first free ` (n)` variant before the
/// extension, atomically claiming an empty placeholder at the winning name.
///
/// This closes a check-then-rename TOCTOU: the old implementation only
/// checked existence with a plain `stat`, so two concurrent `finalize` calls
/// racing for the same `dest` (e.g. two peers sending same-named files at
/// once) could both observe the name as free and then have the second
/// `rename` silently clobber the first's file. Reserving the name with
/// `O_CREAT|O_EXCL` means only one caller can ever win a given candidate;
/// `finalize`'s subsequent `rename` just replaces the empty placeholder it
/// already owns.
async fn unique_path(dest: &str) -> Result<String> {
    if try_reserve(dest).await? {
        return Ok(dest.to_string());
    }
    let path = Path::new(dest);
    let parent = path.parent();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());

    let mut n: u32 = 1;
    loop {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = match parent {
            Some(p) if !p.as_os_str().is_empty() => p.join(&name).to_string_lossy().to_string(),
            _ => name,
        };
        if try_reserve(&candidate).await? {
            return Ok(candidate);
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pb-fs-{}", std::process::id()));
        let path = dir.join("sub/file.bin");
        let path_str = path.to_string_lossy().to_string();

        let storage = FsStorage::new();

        // Write creates parent dirs and streams bytes.
        let mut w = storage.open_write(&path_str).await.unwrap();
        w.write_all(b"hello world").await.unwrap();
        w.flush().await.unwrap();
        w.close().await.unwrap();

        // Read back fully.
        let mut r = storage.open_read(&path_str, 0).await.unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello world");

        // Read from an offset (resume-style).
        let mut r2 = storage.open_read(&path_str, 6).await.unwrap();
        let mut rest = String::new();
        r2.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn open_missing_read_errors() {
        let storage = FsStorage::new();
        let res = storage.open_read("/no/such/pb/file", 0).await;
        assert!(matches!(res, Err(DomainError::Storage(_))));
    }

    #[tokio::test]
    async fn size_reports_len_or_none() {
        let dir = std::env::temp_dir().join(format!("pb-fs-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        std::fs::write(&path, b"12345").unwrap();
        let storage = FsStorage::new();
        assert_eq!(
            storage.size(&path.to_string_lossy()).await.unwrap(),
            Some(5)
        );
        assert_eq!(
            storage
                .size(&dir.join("missing").to_string_lossy())
                .await
                .unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn open_append_extends_file() {
        let dir = std::env::temp_dir().join(format!("pb-fs-append-{}", std::process::id()));
        let path = dir.join("a.bin");
        let path_str = path.to_string_lossy().to_string();
        let storage = FsStorage::new();

        let mut w = storage.open_write(&path_str).await.unwrap();
        w.write_all(b"hello").await.unwrap();
        w.close().await.unwrap();

        let mut a = storage.open_append(&path_str).await.unwrap();
        a.write_all(b" world").await.unwrap();
        a.close().await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn finalize_renames_and_avoids_clobber() {
        let dir = std::env::temp_dir().join(format!("pb-fs-final-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = FsStorage::new();

        let dest = dir.join("f.bin");
        let dest_str = dest.to_string_lossy().to_string();

        // First finalize: temp -> f.bin.
        let t1 = dir.join("a.part");
        std::fs::write(&t1, b"one").unwrap();
        let final1 = storage
            .finalize(&t1.to_string_lossy(), &dest_str)
            .await
            .unwrap();
        assert_eq!(final1, dest_str);
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");

        // Second finalize to the same dest must NOT clobber → "f (1).bin".
        let t2 = dir.join("b.part");
        std::fs::write(&t2, b"two").unwrap();
        let final2 = storage
            .finalize(&t2.to_string_lossy(), &dest_str)
            .await
            .unwrap();
        assert_ne!(final2, dest_str);
        assert!(final2.ends_with("f (1).bin"), "got {final2}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"one", "original untouched");
        assert_eq!(std::fs::read(&final2).unwrap(), b"two");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "finalized file is owner-only");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two finalize() calls racing for the exact same `dest` (e.g. two peers
    /// concurrently sending same-named files) must never let the second
    /// rename clobber the first's file — each temp payload must survive
    /// under a distinct final name. Before the `create_new` reservation, both
    /// calls could observe `dest` as free (plain `stat`-based check) and then
    /// both `rename` onto it, silently dropping one payload.
    #[tokio::test]
    async fn concurrent_finalize_to_same_dest_does_not_clobber() {
        let dir = std::env::temp_dir().join(format!("pb-fs-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = FsStorage::new();

        let dest = dir.join("same.bin");
        let dest_str = dest.to_string_lossy().to_string();

        let t1 = dir.join("one.part");
        let t2 = dir.join("two.part");
        std::fs::write(&t1, b"payload-one").unwrap();
        std::fs::write(&t2, b"payload-two").unwrap();
        let t1_str = t1.to_string_lossy().to_string();
        let t2_str = t2.to_string_lossy().to_string();

        let (r1, r2) = tokio::join!(
            storage.finalize(&t1_str, &dest_str),
            storage.finalize(&t2_str, &dest_str),
        );
        let final1 = r1.unwrap();
        let final2 = r2.unwrap();

        assert_ne!(
            final1, final2,
            "racing finalizes must not pick the same name"
        );
        let contents: std::collections::HashSet<Vec<u8>> = [
            std::fs::read(&final1).unwrap(),
            std::fs::read(&final2).unwrap(),
        ]
        .into_iter()
        .collect();
        assert!(
            contents.contains(b"payload-one".as_slice()),
            "first payload survived"
        );
        assert!(
            contents.contains(b"payload-two".as_slice()),
            "second payload survived"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn list_files_walks_recursively_with_relative_paths() {
        let dir = std::env::temp_dir().join(format!("pb-fs-walk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("sub/b.bin"), b"bb").unwrap();
        std::fs::write(dir.join("sub/deep/c.txt"), b"ccc").unwrap();

        let storage = FsStorage::new();
        let files = storage.list_files(&dir.to_string_lossy()).await.unwrap();

        assert_eq!(
            files,
            vec![
                ("a.txt".to_string(), 1),
                ("sub/b.bin".to_string(), 2),
                ("sub/deep/c.txt".to_string(), 3),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A symlink pointing at a regular file must be listed (following the
    /// link for size/type), not silently dropped: `DirEntry::file_type()`
    /// reports the symlink itself, which is neither `is_dir()` nor
    /// `is_file()`.
    #[cfg(unix)]
    #[tokio::test]
    async fn list_files_follows_symlink_to_regular_file() {
        let dir = std::env::temp_dir().join(format!("pb-fs-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink(dir.join("real.txt"), dir.join("link.txt")).unwrap();

        let storage = FsStorage::new();
        let files = storage.list_files(&dir.to_string_lossy()).await.unwrap();

        assert_eq!(
            files,
            vec![("link.txt".to_string(), 5), ("real.txt".to_string(), 5),],
            "the symlink must be listed alongside the file it targets"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A symlink to a directory must not be followed into the traversal
    /// (avoids cycles / escaping `root`) — it's skipped, not silently pushed
    /// as if it were a real subdirectory.
    #[cfg(unix)]
    #[tokio::test]
    async fn list_files_skips_symlinked_directory() {
        let dir = std::env::temp_dir().join(format!("pb-fs-symlink-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real_sub")).unwrap();
        std::fs::write(dir.join("real_sub/inner.txt"), b"abc").unwrap();
        std::os::unix::fs::symlink(dir.join("real_sub"), dir.join("link_sub")).unwrap();

        let storage = FsStorage::new();
        let files = storage.list_files(&dir.to_string_lossy()).await.unwrap();

        assert_eq!(
            files,
            vec![("real_sub/inner.txt".to_string(), 3)],
            "the symlinked directory must be skipped, not traversed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
