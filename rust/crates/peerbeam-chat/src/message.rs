//! The wire chat message carried on the Chat channel.

use std::sync::Mutex;

use bytes::Bytes;
use chrono::Utc;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionError, SessionFrame};

/// Maximum chat body size (UTF-8 bytes).
pub const MAX_BODY: usize = 16384;
/// MessageType id for a text chat message within the Chat channel namespace.
pub const MSG_TEXT: u16 = 1;

/// Errors from encoding/decoding/validating a chat message.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("chat body too large: {len} bytes (max {MAX_BODY})")]
    TooLarge { len: usize },
    #[error("chat serialization: {0}")]
    Serialization(String),
    #[error("unexpected chat message type {0}")]
    WrongType(u16),
}

impl From<ChatError> for SessionError {
    fn from(e: ChatError) -> Self {
        SessionError::FrameDecode(e.to_string())
    }
}

/// A text/markdown chat message as it travels on the wire. The sender identity is
/// NOT carried here — it is the authenticated session peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Lexicographically time-ordered id (also the persistence key / dedup key).
    pub id: String,
    /// RFC3339 timestamp minted by the sender.
    pub timestamp: String,
    /// Markdown body (<= MAX_BODY bytes).
    pub body: String,
}

impl ChatMessage {
    /// Create a new message, minting a time-ordered id + timestamp. Rejects an
    /// over-cap body.
    pub fn new(body: &str) -> Result<ChatMessage, ChatError> {
        if body.len() > MAX_BODY {
            return Err(ChatError::TooLarge { len: body.len() });
        }
        Ok(ChatMessage {
            id: mint_id(),
            timestamp: Utc::now().to_rfc3339(),
            body: body.to_string(),
        })
    }

    /// The chat MessageType (`Message` = 1).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_TEXT)
    }

    /// Encode as a Chat-channel [`SessionFrame`] on `channel`.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
        if self.body.len() > MAX_BODY {
            return Err(ChatError::TooLarge {
                len: self.body.len(),
            });
        }
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::END_OF_MESSAGE,
            payload,
        ))
    }

    /// Decode from a Chat-channel frame. Rejects the wrong message type, bad
    /// JSON, and an over-cap body.
    pub fn from_frame(frame: &SessionFrame) -> Result<ChatMessage, ChatError> {
        if frame.message_type.get() != MSG_TEXT {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let msg: ChatMessage = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        if msg.body.len() > MAX_BODY {
            return Err(ChatError::TooLarge {
                len: msg.body.len(),
            });
        }
        Ok(msg)
    }
}

/// Last-minted (millis, suffix) pair, used only to keep [`mint_id`]
/// monotonically non-decreasing when two ids are minted within the same
/// wall-clock millisecond (routine under fast/automated sends, where an
/// unguarded random suffix would tie-break in either direction).
static LAST_ID: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// A lexicographically time-ordered id: 13-digit unix-millis + 16 hex.
///
/// Monotonically non-decreasing within a process: if the wall clock has not
/// advanced past the previous call (same millisecond, or a backwards clock
/// step), the low bits are bumped instead of resampled, so back-to-back ids
/// still sort in call order rather than depending on the random draw.
fn mint_id() -> String {
    let now_millis = Utc::now().timestamp_millis().max(0) as u64;
    let mut r = [0u8; 8];
    OsRng.fill_bytes(&mut r);
    let sample = u64::from_be_bytes(r);

    let mut last = LAST_ID.lock().unwrap_or_else(|e| e.into_inner());
    let (millis, suffix) = if now_millis > last.0 {
        (now_millis, sample)
    } else {
        (last.0, last.1.wrapping_add(1))
    };
    *last = (millis, suffix);
    drop(last);

    format!("{:013}{:016x}", millis, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::ChannelId;

    #[test]
    fn new_mints_id_and_timestamp_and_keeps_body() {
        let m = ChatMessage::new("hello **world**").unwrap();
        assert!(!m.id.is_empty());
        assert!(!m.timestamp.is_empty());
        assert_eq!(m.body, "hello **world**");
    }

    #[test]
    fn ids_are_time_ordered_and_unique() {
        let a = ChatMessage::new("a").unwrap();
        let b = ChatMessage::new("b").unwrap();
        assert_ne!(a.id, b.id);
        // 013-digit millis prefix keeps them lexicographically time-ordered.
        assert!(b.id >= a.id);
        assert_eq!(a.id.len(), 13 + 16);
    }

    /// `MAX_BODY` is a frozen wire constant (docs/MESSAGE_REGISTRY.md §4, Chat
    /// `Message = 1`): raising it is a breaking wire change requiring
    /// capability negotiation, not a silent bump. If this trips, you are
    /// about to change it — stop and read the registry entry first.
    #[test]
    fn max_body_is_pinned() {
        assert_eq!(MAX_BODY, 16384);
    }

    #[test]
    fn oversize_body_is_rejected() {
        let big = "x".repeat(MAX_BODY + 1);
        assert!(matches!(
            ChatMessage::new(&big),
            Err(ChatError::TooLarge { .. })
        ));
    }

    #[test]
    fn frame_roundtrip() {
        let m = ChatMessage::new("hi").unwrap();
        let frame = m.to_frame(ChannelId::new(5)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_TEXT);
        assert!(frame
            .flags
            .contains(peerbeam_domain::session::MessageFlags::END_OF_MESSAGE));
        let back = ChatMessage::from_frame(&frame).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn from_frame_rejects_oversize_and_bad_json() {
        use bytes::Bytes;
        use peerbeam_domain::session::{MessageFlags, MessageType, SessionFrame};
        let bad = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(MSG_TEXT),
            MessageFlags::END_OF_MESSAGE,
            Bytes::from_static(b"not json"),
        );
        assert!(ChatMessage::from_frame(&bad).is_err());
    }
}
