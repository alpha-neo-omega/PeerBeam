//! The session-layer frame and its wire codec.
//!
//! A [`SessionFrame`] is the protocol data unit carried on a channel. It is
//! distinct from [`crate::port::Frame`] (the transport frame): a `SessionFrame`
//! rides *inside* a transport frame's payload, adding the channel/message-type
//! header that makes multiplexing and typed messages possible.
//!
//! The encoding is a fixed-width header so decoding is simple and total (no
//! panics, every malformed input yields an error). Exact field widths are an
//! internal detail governed by the protocol version; the field set is the
//! stable contract.

use bytes::Bytes;

use super::error::SessionError;
use super::ids::{ChannelId, MessageType};

/// Per-message flags (a bit set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageFlags(u8);

impl MessageFlags {
    /// No flags set.
    pub const NONE: MessageFlags = MessageFlags(0);
    /// The receiver may safely ignore this message if it does not understand
    /// its [`MessageType`]. Cleared means the message is required.
    pub const OPTIONAL: MessageFlags = MessageFlags(0b0000_0001);
    /// This frame carries the final bytes of a logical message.
    pub const END_OF_MESSAGE: MessageFlags = MessageFlags(0b0000_0010);

    /// The raw bits.
    #[must_use]
    pub const fn bits(&self) -> u8 {
        self.0
    }

    /// Construct from raw bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        MessageFlags(bits)
    }

    /// Whether every flag in `other` is set here.
    #[must_use]
    pub fn contains(&self, other: MessageFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// A copy with the bits in `other` also set.
    #[must_use]
    pub fn with(self, other: MessageFlags) -> Self {
        MessageFlags(self.0 | other.0)
    }

    /// Whether the [`OPTIONAL`](MessageFlags::OPTIONAL) flag is set.
    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.contains(MessageFlags::OPTIONAL)
    }
}

/// Fixed header size: `channel(8) + message_type(2) + flags(1) + length(4)`.
const HEADER_LEN: usize = 8 + 2 + 1 + 4;

/// One typed message on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFrame {
    /// The channel this message belongs to.
    pub channel: ChannelId,
    /// The message kind within the channel's namespace.
    pub message_type: MessageType,
    /// Per-message flags.
    pub flags: MessageFlags,
    /// The message body (opaque at this layer).
    pub payload: Bytes,
}

impl SessionFrame {
    /// Build a frame.
    #[must_use]
    pub fn new(
        channel: ChannelId,
        message_type: MessageType,
        flags: MessageFlags,
        payload: Bytes,
    ) -> Self {
        SessionFrame {
            channel,
            message_type,
            flags,
            payload,
        }
    }

    /// Encode to bytes: fixed header followed by the raw payload.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut buf = Vec::with_capacity(HEADER_LEN + self.payload.len());
        buf.extend_from_slice(&self.channel.get().to_be_bytes());
        buf.extend_from_slice(&self.message_type.get().to_be_bytes());
        buf.push(self.flags.bits());
        // Payload length is bounded to u32 on the wire; longer payloads are a
        // programming error caught here rather than silently truncated.
        let len = u32::try_from(self.payload.len()).unwrap_or(u32::MAX);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        Bytes::from(buf)
    }

    /// Decode from bytes. Total: every malformed input yields
    /// [`SessionError::FrameDecode`] rather than panicking.
    pub fn decode(buf: &[u8]) -> Result<SessionFrame, SessionError> {
        if buf.len() < HEADER_LEN {
            return Err(SessionError::FrameDecode(format!(
                "frame too short: {} < {HEADER_LEN} header bytes",
                buf.len()
            )));
        }
        // Fixed offsets; every slice below is within `HEADER_LEN`, already
        // checked, so the array conversions cannot fail.
        let channel = u64::from_be_bytes(read_array::<8>(&buf[0..8])?);
        let message_type = u16::from_be_bytes(read_array::<2>(&buf[8..10])?);
        let flags = buf[10];
        let len = u32::from_be_bytes(read_array::<4>(&buf[11..15])?) as usize;

        let body = &buf[HEADER_LEN..];
        if body.len() != len {
            return Err(SessionError::FrameDecode(format!(
                "payload length mismatch: header says {len}, got {}",
                body.len()
            )));
        }
        Ok(SessionFrame {
            channel: ChannelId::new(channel),
            message_type: MessageType::new(message_type),
            flags: MessageFlags::from_bits(flags),
            payload: Bytes::copy_from_slice(body),
        })
    }
}

/// Copy a fixed-size slice into an array without panicking.
fn read_array<const N: usize>(slice: &[u8]) -> Result<[u8; N], SessionError> {
    slice
        .try_into()
        .map_err(|_| SessionError::FrameDecode(format!("expected {N} bytes, got {}", slice.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8], flags: MessageFlags) -> SessionFrame {
        SessionFrame::new(
            ChannelId::CONTROL,
            MessageType::new(1),
            flags,
            Bytes::copy_from_slice(payload),
        )
    }

    #[test]
    fn flags_contains_and_with() {
        let f = MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE);
        assert!(f.contains(MessageFlags::OPTIONAL));
        assert!(f.contains(MessageFlags::END_OF_MESSAGE));
        assert!(f.is_optional());
        assert!(!MessageFlags::END_OF_MESSAGE.is_optional());
        assert!(!MessageFlags::NONE.contains(MessageFlags::OPTIONAL));
    }

    #[test]
    fn frame_roundtrip() {
        let original = frame(b"hello session", MessageFlags::OPTIONAL);
        let bytes = original.encode();
        let decoded = SessionFrame::decode(&bytes).expect("decode");
        assert_eq!(original, decoded);
    }

    #[test]
    fn empty_payload_roundtrip() {
        let original = frame(b"", MessageFlags::NONE);
        let decoded = SessionFrame::decode(&original.encode()).expect("decode");
        assert_eq!(original, decoded);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn decode_rejects_short_buffer() {
        assert!(SessionFrame::decode(&[0u8; 3]).is_err());
        assert!(SessionFrame::decode(&[]).is_err());
    }

    #[test]
    fn decode_rejects_length_mismatch() {
        let mut bytes = frame(b"abcd", MessageFlags::NONE).encode().to_vec();
        bytes.push(0xff); // trailing byte the header didn't account for
        assert!(SessionFrame::decode(&bytes).is_err());
    }

    #[test]
    fn decode_never_panics_on_arbitrary_input() {
        // Fuzz-lite: a spread of lengths and byte patterns must all return a
        // Result, never panic.
        for len in 0..64usize {
            let buf: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let _ = SessionFrame::decode(&buf);
        }
    }
}
