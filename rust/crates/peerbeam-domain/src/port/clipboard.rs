//! Clipboard port: reading/writing the host clipboard.

use async_trait::async_trait;

use crate::entity::ClipboardItem;
use crate::error::Result;

/// Reads and writes the host clipboard for cross-device sync.
#[async_trait]
pub trait ClipboardProvider: Send + Sync {
    /// Read the current clipboard content.
    async fn read(&self) -> Result<ClipboardItem>;

    /// Write content to the clipboard.
    async fn write(&self, item: ClipboardItem) -> Result<()>;
}
