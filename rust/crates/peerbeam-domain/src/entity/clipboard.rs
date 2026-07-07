//! Clipboard entity for cross-device clipboard sync.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of clipboard content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClipboardKind {
    Text,
    Link,
    RichText,
}

/// A clipboard item that can be shared between devices.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardItem {
    /// The kind of content.
    pub kind: ClipboardKind,
    /// The textual payload.
    pub content: String,
    /// When the item was captured.
    pub at: DateTime<Utc>,
}
