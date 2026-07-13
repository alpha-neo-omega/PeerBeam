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
        let final_path = unique_path(dest).await;

        // Restrict permissions on the completed file before it becomes visible.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(temp, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|e| DomainError::Storage(format!("set perms {temp}: {e}")))?;
        }

        tokio::fs::rename(temp, &final_path)
            .await
            .map_err(|e| DomainError::Storage(format!("finalize {temp} -> {final_path}: {e}")))?;
        Ok(final_path)
    }

    async fn list_files(&self, root: &str) -> Result<Vec<(String, u64)>> {
        let root_path = PathBuf::from(root);
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
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    let size = entry
                        .metadata()
                        .await
                        .map_err(|e| DomainError::Storage(format!("metadata: {e}")))?
                        .len();
                    let rel = path
                        .strip_prefix(&root_path)
                        .map_err(|e| DomainError::Storage(format!("strip prefix: {e}")))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, size));
                }
            }
        }

        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

async fn exists(path: &str) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

/// Return `dest` if free, else `dest` with a ` (n)` suffix before the
/// extension until an unused name is found.
async fn unique_path(dest: &str) -> String {
    if !exists(dest).await {
        return dest.to_string();
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
        if !exists(&candidate).await {
            return candidate;
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
}
