//! Identifier newtypes for the PeerSession layer.
//!
//! These are pure value types with no IO. Random [`SessionId`] generation lives
//! in the session layer (which owns entropy); the domain stays IO-free per the
//! architectural invariants.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A logical session between two trusted devices.
///
/// A 128-bit value minted by the session initiator and echoed by the responder.
/// It is stable across reconnects within one session's lifetime, so a dropped
/// connection can re-attach rather than starting over (see the session spec).
/// Generation from entropy is performed by the session layer; the domain only
/// carries and compares the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// The all-zero id. Useful as a placeholder and in tests; never a valid
    /// negotiated session id.
    pub const NIL: SessionId = SessionId([0u8; 16]);

    /// Construct from raw bytes (e.g. from entropy produced by the session
    /// layer, or decoded from the wire).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        SessionId(bytes)
    }

    /// Construct from a `u128` (convenient for deterministic tests).
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        SessionId(value.to_be_bytes())
    }

    /// The raw 16 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether this is the [`NIL`](SessionId::NIL) id.
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0 == [0u8; 16]
    }
}

impl fmt::Display for SessionId {
    /// Lower-case hex, no separators.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// A channel within a session. Maps to a transport stream id at the transport
/// layer; `0` is reserved for the control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelId(u64);

impl ChannelId {
    /// The reserved control channel that carries session-level messages
    /// (negotiation, keepalive, shutdown).
    pub const CONTROL: ChannelId = ChannelId(0);

    /// Construct from a raw stream id.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        ChannelId(id)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(&self) -> u64 {
        self.0
    }

    /// Whether this is the control channel.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.0 == 0
    }
}

/// The capability a channel carries (registry id). See the message registry for
/// the range assignments; only [`CONTROL`](ChannelType::CONTROL) and
/// [`TRANSFER`](ChannelType::TRANSFER) are near-term.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelType(u16);

impl ChannelType {
    /// Session's own control protocol.
    pub const CONTROL: ChannelType = ChannelType(0x0000);
    /// File/folder transfer (today's transfer, reframed as a channel).
    pub const TRANSFER: ChannelType = ChannelType(0x0100);
    /// Text/markdown chat messages (Phase B). See MESSAGE_REGISTRY.md §2.
    pub const CHAT: ChannelType = ChannelType(0x0101);
    /// Clipboard sync (Phase B). See MESSAGE_REGISTRY.md §2.
    pub const CLIPBOARD: ChannelType = ChannelType(0x0102);
    /// Device status heartbeats (Phase B). See MESSAGE_REGISTRY.md §2.
    pub const PRESENCE: ChannelType = ChannelType(0x0103);
    /// Note sync (Phase C). See MESSAGE_REGISTRY.md §2.
    ///
    /// The id the registry reserved for Notes. It was pencilled in as "rides
    /// Sync; may not need its own channel", but Sync (`0x0104`) reconciles
    /// *folders* — a different shape of problem, with bytes carried over
    /// Transfer. Notes are small, self-contained records with their own
    /// conflict rule, so they get the id already set aside for them rather than
    /// waiting on a channel built for something else.
    pub const NOTES: ChannelType = ChannelType(0x0105);
    /// Encrypted byte pipe — `peerbeam pipe` (Phase B). See MESSAGE_REGISTRY.md
    /// §2/§4.
    ///
    /// A **stream** capability, like [`TRANSFER`](ChannelType::TRANSFER) and
    /// unlike Chat/Clipboard/Presence: its channel carries an unbounded,
    /// length-unknown byte stream that the caller drives over the sealed link
    /// itself, rather than typed messages dispatched to a
    /// [`MessageHandler`](super::MessageHandler).
    pub const PIPE: ChannelType = ChannelType(0x0107);

    /// Construct from a raw registry id.
    #[must_use]
    pub const fn new(id: u16) -> Self {
        ChannelType(id)
    }

    /// The raw registry id.
    #[must_use]
    pub const fn get(&self) -> u16 {
        self.0
    }

    /// Whether this is the control channel type.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.0 == 0x0000
    }

    /// Whether this id is in the first-party capability range (`0x0100..=0x0FFF`).
    #[must_use]
    pub fn is_first_party(&self) -> bool {
        (0x0100..=0x0FFF).contains(&self.0)
    }

    /// Whether this id is in the plugin range (`0x8000..=0xBFFF`).
    #[must_use]
    pub fn is_plugin(&self) -> bool {
        (0x8000..=0xBFFF).contains(&self.0)
    }
}

/// A message kind within a channel's own namespace. Each [`ChannelType`] owns a
/// separate `MessageType` space starting at `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MessageType(u16);

impl MessageType {
    /// Construct from a raw id.
    #[must_use]
    pub const fn new(id: u16) -> Self {
        MessageType(id)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(&self) -> u16 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_hex_display_is_32_chars() {
        let id = SessionId::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let s = id.to_string();
        assert_eq!(s.len(), 32);
        assert_eq!(s, "0123456789abcdef0011223344556677");
    }

    #[test]
    fn session_id_nil_roundtrip() {
        assert!(SessionId::NIL.is_nil());
        assert!(!SessionId::from_u128(1).is_nil());
        assert_eq!(SessionId::from_bytes([0u8; 16]), SessionId::NIL);
    }

    #[test]
    fn channel_id_control_is_zero() {
        assert!(ChannelId::CONTROL.is_control());
        assert_eq!(ChannelId::CONTROL.get(), 0);
        assert!(!ChannelId::new(1).is_control());
    }

    #[test]
    fn channel_type_range_classification() {
        assert!(ChannelType::CONTROL.is_control());
        assert!(ChannelType::TRANSFER.is_first_party());
        assert!(!ChannelType::TRANSFER.is_control());
        assert!(ChannelType::new(0x8001).is_plugin());
        assert!(!ChannelType::new(0x0100).is_plugin());
    }

    #[test]
    fn chat_channel_type_is_0x0101_and_first_party() {
        assert_eq!(ChannelType::CHAT.get(), 0x0101);
        assert!(ChannelType::CHAT.is_first_party());
        assert!(!ChannelType::CHAT.is_control());
    }

    /// `0x0103` is the id `docs/MESSAGE_REGISTRY.md` §2 reserved for Presence.
    /// Pinned because the registry ids are a long-term wire contract: they are
    /// never renumbered, so a change here is a wire break, not a refactor.
    #[test]
    fn presence_channel_type_is_0x0103_and_first_party() {
        assert_eq!(ChannelType::PRESENCE.get(), 0x0103);
        assert!(ChannelType::PRESENCE.is_first_party());
        assert!(!ChannelType::PRESENCE.is_control());
        assert_ne!(ChannelType::PRESENCE, ChannelType::CHAT);
    }

    /// `0x0102` is the id `docs/MESSAGE_REGISTRY.md` §2 reserved for Clipboard,
    /// pinned for the same reason Presence's is: a renumbering here is a wire
    /// break. It sits *between* Chat and Presence, which is exactly why it is
    /// worth asserting the three are distinct — an off-by-one in this table
    /// would route clips into the presence handler.
    #[test]
    fn clipboard_channel_type_is_0x0102_and_first_party() {
        assert_eq!(ChannelType::CLIPBOARD.get(), 0x0102);
        assert!(ChannelType::CLIPBOARD.is_first_party());
        assert!(!ChannelType::CLIPBOARD.is_control());
        assert_ne!(ChannelType::CLIPBOARD, ChannelType::CHAT);
        assert_ne!(ChannelType::CLIPBOARD, ChannelType::PRESENCE);
    }

    /// `0x0107` is the id `docs/MESSAGE_REGISTRY.md` §2 assigns to Pipe — the
    /// next free first-party slot after the Sync/Notes/Command reservations.
    /// Pinned for the same reason the others are: these ids are a long-term
    /// wire contract and are never renumbered, so a change here is a wire
    /// break rather than a refactor.
    ///
    /// The `0x0104..=0x0106` assertions are not padding: those three ids are
    /// *reserved but unimplemented*, so nothing else in the tree would notice
    /// if Pipe were quietly moved onto one of them.
    #[test]
    fn pipe_channel_type_is_0x0107_and_first_party() {
        assert_eq!(ChannelType::PIPE.get(), 0x0107);
        assert!(ChannelType::PIPE.is_first_party());
        assert!(!ChannelType::PIPE.is_control());
        for reserved in [0x0104u16, 0x0105, 0x0106] {
            assert_ne!(
                ChannelType::PIPE,
                ChannelType::new(reserved),
                "Pipe must not squat a reserved id"
            );
        }
        for taken in [
            ChannelType::TRANSFER,
            ChannelType::CHAT,
            ChannelType::CLIPBOARD,
            ChannelType::PRESENCE,
        ] {
            assert_ne!(ChannelType::PIPE, taken);
        }
    }
}
