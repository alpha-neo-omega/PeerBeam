//! In-memory [`ClipboardProvider`].
//!
//! Holds one clipboard item in memory. Useful on headless servers (no OS
//! clipboard) and as a deterministic double in tests. Real desktop/mobile
//! adapters (e.g. an `arboard`-backed one) implement the same port and swap
//! in via the engine builder — the transfer layer is unaffected either way.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use peerbeam_domain::entity::ClipboardItem;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::ClipboardProvider;

/// A clipboard that lives entirely in memory.
#[derive(Clone, Default)]
pub struct MemoryClipboard {
    item: Arc<Mutex<Option<ClipboardItem>>>,
}

impl MemoryClipboard {
    /// Create an empty in-memory clipboard.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ClipboardProvider for MemoryClipboard {
    async fn read(&self) -> Result<ClipboardItem> {
        self.item
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| DomainError::NotFound("clipboard is empty".into()))
    }

    async fn write(&self, item: ClipboardItem) -> Result<()> {
        *self.item.lock().unwrap() = Some(item);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[tokio::test]
    async fn empty_read_is_not_found() {
        let cb = MemoryClipboard::new();
        assert!(matches!(cb.read().await, Err(DomainError::NotFound(_))));
    }

    #[tokio::test]
    async fn write_then_read() {
        let cb = MemoryClipboard::new();
        let item = ClipboardItem::text("hello".into(), t0());
        cb.write(item.clone()).await.unwrap();
        assert_eq!(cb.read().await.unwrap(), item);
    }

    #[tokio::test]
    async fn write_overwrites() {
        let cb = MemoryClipboard::new();
        cb.write(ClipboardItem::text("a".into(), t0()))
            .await
            .unwrap();
        cb.write(ClipboardItem::text("b".into(), t0()))
            .await
            .unwrap();
        assert_eq!(cb.read().await.unwrap().as_text(), Some("b"));
    }
}
