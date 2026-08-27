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
    /// Open a streamed writer at `path`, creating/truncating as needed and
    /// creating parent directories.
    async fn open_write(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>>;

    /// Open a streamed writer that appends to `path`, creating it (and parent
    /// directories) if missing. Used to resume a partially-received file.
    async fn open_append(&self, path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>>;

    /// Open a streamed reader at `path`, seeking to `offset` (for resume).
    async fn open_read(&self, path: &str, offset: u64)
        -> Result<Box<dyn AsyncRead + Unpin + Send>>;

    /// Size of a file in bytes, or `None` if it does not exist. Used to
    /// compute resume offsets.
    async fn size(&self, path: &str) -> Result<Option<u64>>;

    /// Recursively list files under `root`, returning each file's path
    /// relative to `root` (with `/` separators) and its size, sorted by path.
    /// Directories themselves are not returned.
    async fn list_files(&self, root: &str) -> Result<Vec<(String, u64)>>;

    /// Atomically promote a fully-written temporary file to its final
    /// destination. If `dest` already exists, a non-colliding name is chosen
    /// (e.g. `file (1).ext`) so existing files are never overwritten.
    /// Restrictive permissions are applied. Returns the actual final path.
    async fn finalize(&self, temp: &str, dest: &str) -> Result<String>;

    /// [`finalize`](Self::finalize), but **replacing** anything already at
    /// `dest` instead of choosing a free name.
    ///
    /// For callers whose contract is *overwrite*, which a folder receive's is:
    /// a folder is delivered as a tree, and re-receiving one must reproduce it
    /// rather than accumulate `f (1).bin`, `f (2).bin` beside the files it is
    /// supposed to be replacing. Single-file receives keep using [`finalize`] —
    /// there, silently overwriting a file the user already had would be the
    /// wrong answer, and the two contracts are genuinely different rather than
    /// one being a special case of the other.
    ///
    /// The default delegates to [`finalize`], so an implementation that has no
    /// notion of replacement keeps its existing non-clobbering behaviour rather
    /// than being forced into one it cannot honour.
    async fn finalize_replacing(&self, temp: &str, dest: &str) -> Result<String> {
        self.finalize(temp, dest).await
    }
}
