//! Storage port: streamed file IO.
//!
//! Returns `futures`-based async readers/writers so the domain stays
//! runtime-agnostic (no direct `tokio::fs` dependency). Streaming only —
//! no API here loads a whole file into memory.

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};

use crate::error::Result;

/// Opens streamed readers and writers for transfer payloads.
#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Open a streamed writer at `path`, creating/truncating as needed.
    async fn open_write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>>;

    /// Open a streamed reader at `path`, seeking to `offset` (for resume).
    async fn open_read(&self, path: &str, offset: u64)
        -> Result<Box<dyn AsyncRead + Unpin + Send>>;
}
