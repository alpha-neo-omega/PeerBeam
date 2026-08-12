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
/// Maximum length of a `FileRef` name, in bytes.
pub const MAX_NAME: usize = 255;
/// MessageType id for a file reference within the Chat channel namespace.
pub const MSG_FILE_REF: u16 = 2;

/// Errors from encoding/decoding/validating a chat message.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("chat body too large: {len} bytes (max {MAX_BODY})")]
    TooLarge { len: usize },
    #[error("chat serialization: {0}")]
    Serialization(String),
    #[error("unexpected chat message type {0}")]
    WrongType(u16),
    #[error("bad file name: {0}")]
    BadName(String),
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

/// Validate a peer- or user-supplied file name: a bare filename, nothing else.
/// Rejects `../`, absolute paths, separators, empty, `.`/`..`, and over-long.
fn validate_name(name: &str) -> Result<(), ChatError> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(ChatError::BadName(format!("bad length: {}", name.len())));
    }
    let round_trips = std::path::Path::new(name)
        .file_name()
        .map(|f| f == std::ffi::OsStr::new(name))
        .unwrap_or(false);
    if !round_trips {
        return Err(ChatError::BadName(format!("not a bare filename: {name}")));
    }
    Ok(())
}

/// A reference to a file being shared in a conversation, as it travels on the
/// wire. Carries NO local path — the sender's filesystem layout is private (the
/// record-side `FileMeta` holds that). The bytes themselves travel over the
/// TRANSFER stream channel; this message only places the row in the thread and
/// correlates it, because `id` is also used as the transfer id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// Time-ordered id — the chat record key AND the transfer id.
    pub id: String,
    /// RFC3339 timestamp minted by the sender.
    pub timestamp: String,
    /// Bare file name (validated; never a path).
    pub name: String,
    /// Size in bytes, for display before the transfer starts.
    pub size: u64,
}

impl FileRef {
    /// Create a reference, minting a time-ordered id + timestamp.
    pub fn new(name: &str, size: u64) -> Result<FileRef, ChatError> {
        validate_name(name)?;
        Ok(FileRef {
            id: mint_id(),
            timestamp: Utc::now().to_rfc3339(),
            name: name.to_string(),
            size,
        })
    }

    /// The chat MessageType (`FileRef` = 2).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_FILE_REF)
    }

    /// Encode as a Chat-channel frame. Sent OPTIONAL so a peer that does not
    /// implement it skips the message instead of failing the channel
    /// (MESSAGE_REGISTRY.md §6/§7).
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
        validate_name(&self.name)?;
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Chat-channel frame. The name is re-validated: it is
    /// attacker-controlled input.
    pub fn from_frame(frame: &SessionFrame) -> Result<FileRef, ChatError> {
        if frame.message_type.get() != MSG_FILE_REF {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let r: FileRef = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        validate_name(&r.name)?;
        Ok(r)
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
pub fn mint_id() -> String {
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

    #[test]
    fn file_ref_roundtrips_through_a_frame() {
        let r = FileRef::new("report.pdf", 4096).unwrap();
        assert_eq!(r.name, "report.pdf");
        assert_eq!(r.size, 4096);
        assert_eq!(r.id.len(), 13 + 16);
        let frame = r.to_frame(ChannelId::new(3)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_FILE_REF);
        assert!(
            frame.flags.is_optional(),
            "additive types ship OPTIONAL (registry §7)"
        );
        assert!(frame.flags.contains(MessageFlags::END_OF_MESSAGE));
        assert_eq!(FileRef::from_frame(&frame).unwrap(), r);
    }

    /// The wire type must never leak the sender's local path. `FileMeta` (record
    /// side) holds it; `FileRef` (wire side) must not even have the field.
    #[test]
    fn file_ref_frame_never_contains_a_local_path() {
        let r = FileRef::new("report.pdf", 1).unwrap();
        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        assert!(
            !json.contains("local_path"),
            "wire frame leaked a local path: {json}"
        );
    }

    #[test]
    fn file_ref_rejects_unsafe_or_oversized_names() {
        for bad in ["../escape", "/etc/passwd", "a/b", "", "."] {
            assert!(
                matches!(FileRef::new(bad, 1), Err(ChatError::BadName(_))),
                "accepted {bad:?}"
            );
        }
        let long = "x".repeat(256);
        assert!(matches!(FileRef::new(&long, 1), Err(ChatError::BadName(_))));
        assert!(FileRef::new(&"x".repeat(255), 1).is_ok());
    }

    /// A hostile peer's frame must be rejected on decode too, not just on send.
    #[test]
    fn from_frame_rejects_a_hostile_name() {
        let good = FileRef::new("ok.txt", 1).unwrap();
        let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
        let hostile = r#"{"id":"x","timestamp":"t","name":"../escape","size":1}"#;
        frame.payload = bytes::Bytes::from_static(hostile.as_bytes());
        assert!(matches!(
            FileRef::from_frame(&frame),
            Err(ChatError::BadName(_))
        ));
    }

    #[test]
    fn from_frame_rejects_the_wrong_message_type() {
        let text = ChatMessage::new("hi")
            .unwrap()
            .to_frame(ChannelId::new(1))
            .unwrap();
        assert!(matches!(
            FileRef::from_frame(&text),
            Err(ChatError::WrongType(_))
        ));
    }
}
