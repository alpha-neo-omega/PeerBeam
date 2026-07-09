//! Filesystem [`StorageProvider`].
//!
//! Opens streamed async readers/writers backed by `tokio::fs`, bridged to the
//! `futures` IO traits the domain port speaks (via `tokio_util::compat`).
//! Nothing here buffers a whole file: callers read/write chunk by chunk, so
//! transfers stay memory-bounded regardless of file size.

use std::path::Path;

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
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
}
