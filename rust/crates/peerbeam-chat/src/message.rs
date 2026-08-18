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
/// Maximum length of a `FileRef` id, in bytes. Generous next to the 29 bytes
/// [`mint_id`] produces, and small enough that an id can never be a payload in
/// its own right — it is echoed into events, into persisted history, and into
/// every log line about the transfer. Deliberately the same bound the FFI's
/// `is_valid_transfer_id` applies, because this id **is** that transfer id.
pub const MAX_ID: usize = 128;
/// MessageType id for a file reference within the Chat channel namespace.
pub const MSG_FILE_REF: u16 = 2;
/// MessageType id for a file decline within the Chat channel namespace.
pub const MSG_FILE_DECLINE: u16 = 3;
/// MessageType id for a reaction within the Chat channel namespace.
pub const MSG_REACTION: u16 = 4;
/// MessageType id for a read receipt within the Chat channel namespace.
pub const MSG_RECEIPT: u16 = 5;
/// Maximum length of a reaction, in bytes.
///
/// An emoji is a handful of scalar values at most — a family emoji with skin
/// tones and zero-width joiners is around 25 bytes — so this holds every real
/// one with room to spare while refusing a body smuggled in as a "reaction".
/// This is a resource bound, not a taste test: what counts as an emoji is the
/// sending client's business, and a build that offers a different set is not
/// wrong, it is newer.
pub const MAX_REACTION: usize = 64;

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
    #[error("bad reaction: {0}")]
    BadReaction(String),
    #[error("bad receipt: {0}")]
    BadReceipt(String),
    #[error("bad file id: {0}")]
    BadId(String),
    /// An outbox entry exists but could not be decoded — most likely written
    /// by a newer schema. Deliberately distinct from
    /// [`Serialization`](Self::Serialization): that variant means the *store*
    /// failed (I/O, or a validation error at the storage layer); this one
    /// means the store answered fine and handed back bytes this build simply
    /// cannot parse. The keep rule shared by `ChatStore::delete_conversation`
    /// and `ChatStore::delete_messages` raises this when it cannot account for
    /// every queued message and must refuse rather than guess.
    ///
    /// The distinction matters one layer up: a caller across the FFI boundary
    /// needs to tell "retrying changes nothing until the queue clears" apart
    /// from an ordinary store failure — which is why this is its own variant
    /// rather than a string that caller would have to sniff.
    #[error("a queued outbox entry could not be decoded: {0}")]
    QueueUnreadable(String),
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

/// Validate a peer- or user-supplied file id: one bounded, boring token.
///
/// `FileRef.id` is the single most load-bearing field in this feature and the
/// least obviously dangerous: it is the chat-store dedup key, the persisted
/// record id, the `AppStore` key, **and** the transfer id. Until now only
/// `name` was validated on decode, so every one of those consumers was trusting
/// a string the peer chose freely.
///
/// Three concrete reasons this is not decoration:
///
/// * `StagingStore::stage` refuses any id that is not a bare file name — it
///   interpolates the id into a path and `open_write` is `File::create`, so a
///   `..` or a separator would truncate a file outside the blob root. Rejecting
///   here means no consumer has to remember that.
/// * The receiving FFI validates the transfer id with `is_valid_transfer_id`
///   and falls back to a locally minted one when it fails. An id this side
///   accepted but that side refuses would silently break the correlation the
///   whole feature rests on — the row and its bytes would stop being one thing.
/// * The id is echoed verbatim into events, history and logs.
///
/// The rule is deliberately identical to `is_valid_transfer_id`'s: non-empty,
/// at most [`MAX_ID`] bytes, `[A-Za-z0-9._-]`, and never `.` or `..`. The right
/// response to a failure is to **reject the message**, never to sanitise the id
/// — a sanitised id could collide with another blob or another row.
fn validate_id(id: &str) -> Result<(), ChatError> {
    if id.is_empty() || id.len() > MAX_ID {
        return Err(ChatError::BadId(format!("bad length: {}", id.len())));
    }
    if id == "." || id == ".." {
        return Err(ChatError::BadId(format!("reserved name: {id}")));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ChatError::BadId(format!(
            "not a bare [A-Za-z0-9._-] token: {id}"
        )));
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
        validate_id(&self.id)?;
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

    /// Decode from a Chat-channel frame. The name **and the id** are
    /// re-validated: both are attacker-controlled input, and the id is the more
    /// dangerous of the two (see [`validate_id`]).
    pub fn from_frame(frame: &SessionFrame) -> Result<FileRef, ChatError> {
        if frame.message_type.get() != MSG_FILE_REF {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let r: FileRef = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        validate_name(&r.name)?;
        validate_id(&r.id)?;
        Ok(r)
    }
}

/// "I turned down the file you offered." Carries only the id of the [`FileRef`]
/// being refused — everything else about the file is already in both threads.
///
/// It travels as an ordinary chat message on the CHAT channel rather than as a
/// transfer-protocol frame, and deliberately so: if the sender is offline at
/// the moment its receiver declines, the decline queues in the decliner's own
/// outbox and delivers later over machinery that already exists. A
/// transfer-channel signal would simply be lost with the connection, leaving
/// the sender unable to tell a refusal from a dropped network — which is the
/// whole reason a refused file would otherwise be re-offered forever,
/// re-prompting its receiver every single time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDecline {
    /// The id of the `FileRef` being refused (also the transfer id).
    pub id: String,
    /// RFC3339 timestamp minted by the decliner.
    pub timestamp: String,
}

impl FileDecline {
    /// Refuse the file offered under `id`, minting the timestamp.
    #[must_use]
    pub fn new(id: &str) -> FileDecline {
        FileDecline {
            id: id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    /// The chat MessageType (`FileDecline` = 3).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_FILE_DECLINE)
    }

    /// Encode as a Chat-channel frame. Sent OPTIONAL so a peer that does not
    /// implement it skips the message instead of failing the channel
    /// (MESSAGE_REGISTRY.md §6/§7) — a 2a-era sender must keep its conversation
    /// even if one of these ever reaches it.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
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

    /// Decode from a Chat-channel frame. `id` needs no validation here: it is
    /// never a path and is never trusted as a key on its own — every write it
    /// drives goes through `ChatStore::settle_file_row`, which authorizes
    /// against the stored record rather than the id.
    pub fn from_frame(frame: &SessionFrame) -> Result<FileDecline, ChatError> {
        if frame.message_type.get() != MSG_FILE_DECLINE {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        serde_json::from_slice(&frame.payload).map_err(|e| ChatError::Serialization(e.to_string()))
    }
}

/// A reaction to one message in a conversation: an emoji attached to, or
/// withdrawn from, the message named by `target_id`.
///
/// **Add and remove are the same message with a flag, not a toggle.** A toggle
/// derives the new state from what the receiver believes the old one was, so a
/// single dropped or duplicated frame leaves the two devices permanently
/// disagreeing about whether the reaction is there. Stating the intended end
/// state makes the message idempotent: applying it twice is applying it once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    /// The id of the message being reacted to, in this conversation.
    pub target_id: String,
    /// The reaction itself, as the sender's client chose to express it.
    pub emoji: String,
    /// `true` withdraws this reaction, `false` adds it.
    #[serde(default)]
    pub remove: bool,
    /// RFC 3339 send time, for ordering only.
    pub timestamp: String,
}

impl Reaction {
    /// A reaction adding `emoji` to `target_id`.
    #[must_use]
    pub fn add(target_id: &str, emoji: &str) -> Reaction {
        Reaction {
            target_id: target_id.to_string(),
            emoji: emoji.to_string(),
            remove: false,
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    /// A reaction withdrawing `emoji` from `target_id`.
    #[must_use]
    pub fn remove(target_id: &str, emoji: &str) -> Reaction {
        Reaction {
            target_id: target_id.to_string(),
            emoji: emoji.to_string(),
            remove: true,
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    /// The chat MessageType (`Reaction` = 4).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_REACTION)
    }

    /// Encode as a Chat-channel frame. OPTIONAL, like `FileDecline`: a peer
    /// that predates reactions skips the message rather than failing the
    /// channel, so reacting to an older build costs the reaction and nothing
    /// else (MESSAGE_REGISTRY.md §6/§7).
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
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

    /// Decode from a Chat-channel frame, bounding both fields.
    ///
    /// `emoji` is length-checked here but **not** character-checked: like a
    /// file name, it is peer-supplied text that ends up rendered, and the one
    /// display policy for that is [`display_name`](crate::display_name), which
    /// substitutes rather than deletes. Applying it at the point of render
    /// keeps a single authority for the rule instead of a second, stricter
    /// one that would reject a message the rest of the app would have shown
    /// safely.
    ///
    /// `target_id` is bounded by [`MAX_ID`] because it is a message id and is
    /// echoed into events and history exactly as `FileRef`'s is. It is not
    /// otherwise validated: it authorizes nothing on its own — every write it
    /// drives goes through `ChatStore::apply_reaction`, which looks the record
    /// up in the peer's own namespace and does nothing when it is absent.
    pub fn from_frame(frame: &SessionFrame) -> Result<Reaction, ChatError> {
        if frame.message_type.get() != MSG_REACTION {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let r: Reaction = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        if r.target_id.is_empty() || r.target_id.len() > MAX_ID {
            return Err(ChatError::BadReaction(format!(
                "target id length {} (max {MAX_ID})",
                r.target_id.len()
            )));
        }
        if r.emoji.is_empty() || r.emoji.len() > MAX_REACTION {
            return Err(ChatError::BadReaction(format!(
                "reaction length {} (max {MAX_REACTION})",
                r.emoji.len()
            )));
        }
        Ok(r)
    }
}

/// "I have read your messages up to and including `read_through`."
///
/// **A watermark, not a per-message acknowledgement.** Message ids are
/// lexicographically time-ordered ([`mint_id`]), so one id names a prefix of
/// the conversation, and one receipt covers a whole thread-read instead of one
/// frame per message. That makes it naturally idempotent — re-applying a
/// watermark marks nothing new — and monotonic, since a watermark only ever
/// moves forward. A per-message scheme would need dedup, ordering, and a
/// decision about what a receipt for a message you never sent means.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    /// The newest message of ours the peer has read, inclusive.
    pub read_through: String,
    /// RFC 3339 time the peer read it.
    pub timestamp: String,
}

impl Receipt {
    /// A receipt saying everything up to `read_through` has been read.
    #[must_use]
    pub fn read_through(id: &str) -> Receipt {
        Receipt {
            read_through: id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    /// The chat MessageType (`Receipt` = 5).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_RECEIPT)
    }

    /// Encode as a Chat-channel frame. OPTIONAL, like every chat message added
    /// after 2a: a peer that predates receipts skips it rather than failing the
    /// channel.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ChatError> {
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

    /// Decode from a Chat-channel frame, bounding the watermark id.
    ///
    /// `read_through` authorizes nothing on its own: `ChatStore::apply_receipt`
    /// marks only **our own outgoing** rows, inside this peer's own namespace,
    /// so a receipt can neither reach another conversation nor rewrite a row
    /// the peer itself sent.
    pub fn from_frame(frame: &SessionFrame) -> Result<Receipt, ChatError> {
        if frame.message_type.get() != MSG_RECEIPT {
            return Err(ChatError::WrongType(frame.message_type.get()));
        }
        let r: Receipt = serde_json::from_slice(&frame.payload)
            .map_err(|e| ChatError::Serialization(e.to_string()))?;
        if r.read_through.is_empty() || r.read_through.len() > MAX_ID {
            return Err(ChatError::BadReceipt(format!(
                "read_through length {} (max {MAX_ID})",
                r.read_through.len()
            )));
        }
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
    #[test]
    fn receipt_round_trips_and_ships_optional() {
        let r = Receipt::read_through("m-9");
        let frame = r.to_frame(ChannelId::new(2)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_RECEIPT);
        assert!(
            frame.flags.contains(MessageFlags::OPTIONAL),
            "a peer predating receipts must skip, not fail the channel"
        );
        assert_eq!(Receipt::from_frame(&frame).unwrap(), r);
    }

    #[test]
    fn an_empty_or_oversized_watermark_is_refused() {
        let mut r = Receipt::read_through("m-9");
        r.read_through = String::new();
        assert!(matches!(
            Receipt::from_frame(&r.to_frame(ChannelId::new(1)).unwrap()),
            Err(ChatError::BadReceipt(_))
        ));

        let mut r = Receipt::read_through("m-9");
        r.read_through = "i".repeat(MAX_ID + 1);
        assert!(matches!(
            Receipt::from_frame(&r.to_frame(ChannelId::new(1)).unwrap()),
            Err(ChatError::BadReceipt(_))
        ));
    }

    #[test]
    fn reaction_round_trips_and_states_its_intent() {
        let r = Reaction::add("m-1", "\u{1F44D}");
        let frame = r.to_frame(ChannelId::new(7)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_REACTION);
        let back = Reaction::from_frame(&frame).unwrap();
        assert_eq!(back, r);
        assert!(!back.remove);

        let w = Reaction::remove("m-1", "\u{1F44D}");
        let back = Reaction::from_frame(&w.to_frame(ChannelId::new(7)).unwrap()).unwrap();
        assert!(back.remove);
    }

    #[test]
    fn a_reaction_frame_is_optional_so_an_older_peer_skips_it() {
        // A peer that predates reactions must drop the message, not fail the
        // channel — reacting at an old build costs the reaction, nothing more.
        let frame = Reaction::add("m-1", "\u{2764}")
            .to_frame(ChannelId::new(3))
            .unwrap();
        assert!(
            frame.flags.contains(MessageFlags::OPTIONAL),
            "reaction frame was not OPTIONAL"
        );
    }

    #[test]
    fn an_oversized_or_empty_reaction_is_refused() {
        let mut r = Reaction::add("m-1", "x");
        r.emoji = "e".repeat(MAX_REACTION + 1);
        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(matches!(
            Reaction::from_frame(&frame),
            Err(ChatError::BadReaction(_))
        ));

        let mut r = Reaction::add("m-1", "x");
        r.emoji = String::new();
        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(matches!(
            Reaction::from_frame(&frame),
            Err(ChatError::BadReaction(_))
        ));
    }

    #[test]
    fn an_oversized_or_empty_target_id_is_refused() {
        let mut r = Reaction::add("m-1", "\u{1F44D}");
        r.target_id = "i".repeat(MAX_ID + 1);
        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(matches!(
            Reaction::from_frame(&frame),
            Err(ChatError::BadReaction(_))
        ));

        let mut r = Reaction::add("m-1", "\u{1F44D}");
        r.target_id = String::new();
        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(matches!(
            Reaction::from_frame(&frame),
            Err(ChatError::BadReaction(_))
        ));
    }

    #[test]
    fn a_reaction_does_not_decode_from_another_message_type() {
        let frame = ChatMessage::new("hello")
            .unwrap()
            .to_frame(ChannelId::new(1))
            .unwrap();
        assert!(matches!(
            Reaction::from_frame(&frame),
            Err(ChatError::WrongType(MSG_TEXT))
        ));
    }

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
    ///
    /// Asserting only that the *key* is absent is weak: a future
    /// `local_path: Option<String>` carrying
    /// `#[serde(skip_serializing_if = "Option::is_none")]` would keep such an
    /// assertion green. So this pins two stronger properties as well:
    ///
    /// 1. the frame does not contain the **populated value** the record-side
    ///    twin really holds — the exact `(FileRef, FileMeta)` pairing
    ///    `prepare_file_send` produces, where the path is definitely present on
    ///    one side and must never appear on the other;
    /// 2. the serialized object's key set is **exactly** the four wire fields,
    ///    so any added field is caught the moment it serializes at all.
    ///
    /// Between them, a new field could only slip through by being both
    /// skip-serialized *and* never populated — in which case it leaks nothing.
    #[test]
    fn file_ref_frame_never_contains_a_local_path() {
        const SECRET_PATH: &str = "/home/alice/Private/taxes/report.pdf";
        let r = FileRef::new("report.pdf", 1).unwrap();
        // The record-side twin the sender persists beside this very FileRef,
        // with the path POPULATED — not a hypothetical.
        let meta = crate::record::FileMeta::new(&r.name, r.size, Some(SECRET_PATH.to_string()));
        assert_eq!(
            meta.local_path.as_deref(),
            Some(SECRET_PATH),
            "the record side must really hold the path, or this proves nothing"
        );

        let frame = r.to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        assert!(
            !json.contains("local_path"),
            "wire frame leaked the local-path key: {json}"
        );
        assert!(
            !json.contains(SECRET_PATH) && !json.contains("/home/alice"),
            "wire frame leaked a populated local path: {json}"
        );

        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<String> = object
            .as_object()
            .expect("a FileRef frame is a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "id".to_string(),
                "name".to_string(),
                "size".to_string(),
                "timestamp".to_string()
            ],
            "the FileRef wire shape gained or lost a field: {json}"
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

    /// `FileRef.id` is the chat-store key, the persisted record id, the
    /// `AppStore` key AND the transfer id, and it arrives from the peer. A
    /// traversal id reaching `StagingStore::stage` is an arbitrary write; one
    /// the receiving FFI's `is_valid_transfer_id` refuses silently breaks the
    /// id correlation the feature rests on. So a hostile id must be rejected on
    /// decode, exactly like a hostile name — never sanitised, since a sanitised
    /// id could collide with a different blob or a different row.
    #[test]
    fn from_frame_rejects_a_hostile_id() {
        let good = FileRef::new("ok.txt", 1).unwrap();
        for bad in [
            "../escape",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "",
            ".",
            "..",
            "has space",
            "nul\u{0}byte",
            "emoji🎉",
        ] {
            let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
            let payload = serde_json::json!({
                "id": bad, "timestamp": "t", "name": "ok.txt", "size": 1u64,
            });
            frame.payload = bytes::Bytes::from(serde_json::to_vec(&payload).unwrap());
            assert!(
                matches!(FileRef::from_frame(&frame), Err(ChatError::BadId(_))),
                "accepted a hostile id: {bad:?}"
            );
        }
        // Over-long is refused; exactly at the bound is fine.
        let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
        let long = "x".repeat(MAX_ID + 1);
        frame.payload = bytes::Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "id": long, "timestamp": "t", "name": "ok.txt", "size": 1u64,
            }))
            .unwrap(),
        );
        assert!(matches!(
            FileRef::from_frame(&frame),
            Err(ChatError::BadId(_))
        ));

        // The guard cannot pass by refusing everything: a real minted id, and
        // the boring tokens another implementation might plausibly mint, all
        // survive the round trip.
        for ok in [good.id.as_str(), "tx-1234-7", "a.b_c-D9"] {
            let mut frame = good.to_frame(ChannelId::new(1)).unwrap();
            frame.payload = bytes::Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "id": ok, "timestamp": "t", "name": "ok.txt", "size": 1u64,
                }))
                .unwrap(),
            );
            assert_eq!(
                FileRef::from_frame(&frame).expect("a bare id decodes").id,
                ok
            );
        }
    }

    /// The same rule on the way out, so a locally-built `FileRef` cannot put an
    /// id on the wire that our own receive path would refuse.
    #[test]
    fn to_frame_refuses_a_hostile_id() {
        let mut r = FileRef::new("ok.txt", 1).unwrap();
        r.id = "../escape".into();
        assert!(matches!(
            r.to_frame(ChannelId::new(1)),
            Err(ChatError::BadId(_))
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

    #[test]
    fn file_decline_round_trips_and_ships_optional() {
        let d = FileDecline::new("0000000000001");
        let frame = d.to_frame(ChannelId::new(7)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_FILE_DECLINE);
        // OPTIONAL so a peer that does not know type 3 ignores it and keeps
        // the channel (MESSAGE_REGISTRY.md section 6) rather than tearing the
        // conversation down.
        assert!(frame.flags.is_optional());
        assert!(frame.flags.contains(MessageFlags::END_OF_MESSAGE));
        assert_eq!(FileDecline::from_frame(&frame).unwrap(), d);
    }

    #[test]
    fn file_decline_rejects_a_frame_of_the_wrong_type() {
        let r = FileRef::new("a.bin", 1).unwrap();
        let frame = r.to_frame(ChannelId::new(7)).unwrap();
        assert!(matches!(
            FileDecline::from_frame(&frame),
            Err(ChatError::WrongType(MSG_FILE_REF))
        ));
    }

    /// A decline names a file by id and nothing else. Pinning the exact key set
    /// keeps a future field from quietly joining it — the id is already known to
    /// both sides, so anything added here would be new information travelling
    /// out of a refusal, which is the one moment the user chose to share less.
    #[test]
    fn file_decline_carries_nothing_but_the_id_and_its_timestamp() {
        let d = FileDecline::new("0000000000001");
        let frame = d.to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<String> = object
            .as_object()
            .expect("a FileDecline frame is a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["id".to_string(), "timestamp".to_string()],
            "the FileDecline wire shape gained or lost a field: {json}"
        );
    }
}
