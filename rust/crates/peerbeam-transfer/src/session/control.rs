//! Control-channel messages: the session's own protocol.
//!
//! These ride on [`ChannelId::CONTROL`]. The [`SessionFrame::message_type`]
//! selects the message; the payload is the serialized body. Ids follow the
//! message registry (`docs/MESSAGE_REGISTRY.md`).

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{
    CapabilitySet, ChannelId, ChannelType, MessageFlags, MessageType, SessionError, SessionFrame,
    SessionId, Version,
};

/// Control message type ids (within the control channel's namespace). Ids follow
/// the message registry; unassigned numbers are reserved.
pub mod id {
    /// Version + capability announcement.
    pub const HELLO: u16 = 1;
    /// Request to open a data channel `{channel, channel_type}`.
    pub const CHANNEL_OPEN: u16 = 2;
    /// Accept a channel open `{channel}`.
    pub const CHANNEL_ACCEPT: u16 = 3;
    /// Reject a channel open `{channel, reason}`.
    pub const CHANNEL_REJECT: u16 = 4;
    /// Request to close a channel `{channel}`.
    pub const CHANNEL_CLOSE: u16 = 5;
    /// Keepalive request.
    pub const PING: u16 = 6;
    /// Keepalive reply.
    pub const PONG: u16 = 7;
    /// Graceful whole-session teardown.
    pub const SHUTDOWN: u16 = 8;
    /// "I do not understand message type N."
    pub const UNSUPPORTED: u16 = 11;
    /// Acknowledge a channel close `{channel}`.
    pub const CHANNEL_CLOSED: u16 = 13;
    /// A session-level protocol violation `{detail}`.
    pub const PROTOCOL_ERROR: u16 = 14;
    /// A channel-scoped error `{channel, detail}`.
    pub const CHANNEL_ERROR: u16 = 15;
}

/// The body of a [`ControlMessage::Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHello {
    /// The protocol version this side speaks.
    pub version: Version,
    /// The capabilities this side supports.
    pub capabilities: CapabilitySet,
    /// The session id (minted by the initiator, echoed by the responder).
    pub session_id: SessionId,
}

/// A decoded control message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    /// Version + capability announcement, exchanged once at open.
    Hello(SessionHello),
    /// Keepalive request carrying an echo nonce.
    Ping(u64),
    /// Keepalive reply echoing the ping nonce.
    Pong(u64),
    /// Graceful teardown with a human-readable reason.
    Shutdown(String),
    /// Notice that a received message type was not understood.
    Unsupported(u16),
    /// Peer requests opening a data channel of the given type.
    ChannelOpen {
        /// The channel id (allocated by the opener).
        channel: ChannelId,
        /// The capability the channel will carry.
        channel_type: ChannelType,
    },
    /// Peer accepted a channel open.
    ChannelAccept {
        /// The accepted channel id.
        channel: ChannelId,
    },
    /// Peer refused a channel open.
    ChannelReject {
        /// The refused channel id.
        channel: ChannelId,
        /// Why it was refused.
        reason: String,
    },
    /// Peer requests closing a channel.
    ChannelClose {
        /// The channel to close.
        channel: ChannelId,
    },
    /// Peer confirms a channel is closed.
    ChannelClosed {
        /// The closed channel.
        channel: ChannelId,
    },
    /// A session-level protocol violation (fatal for the session).
    ProtocolError(String),
    /// A channel-scoped error (fatal only for that channel).
    ChannelError {
        /// The affected channel.
        channel: ChannelId,
        /// What went wrong.
        detail: String,
    },
}

impl ControlMessage {
    /// The message-type id this control message uses on the wire.
    #[must_use]
    pub fn message_type(&self) -> MessageType {
        MessageType::new(match self {
            ControlMessage::Hello(_) => id::HELLO,
            ControlMessage::Ping(_) => id::PING,
            ControlMessage::Pong(_) => id::PONG,
            ControlMessage::Shutdown(_) => id::SHUTDOWN,
            ControlMessage::Unsupported(_) => id::UNSUPPORTED,
            ControlMessage::ChannelOpen { .. } => id::CHANNEL_OPEN,
            ControlMessage::ChannelAccept { .. } => id::CHANNEL_ACCEPT,
            ControlMessage::ChannelReject { .. } => id::CHANNEL_REJECT,
            ControlMessage::ChannelClose { .. } => id::CHANNEL_CLOSE,
            ControlMessage::ChannelClosed { .. } => id::CHANNEL_CLOSED,
            ControlMessage::ProtocolError(_) => id::PROTOCOL_ERROR,
            ControlMessage::ChannelError { .. } => id::CHANNEL_ERROR,
        })
    }

    /// Encode as a control-channel [`SessionFrame`].
    pub fn to_frame(&self) -> Result<SessionFrame, SessionError> {
        let payload = match self {
            ControlMessage::Hello(hello) => encode(hello)?,
            ControlMessage::Ping(nonce) | ControlMessage::Pong(nonce) => encode(nonce)?,
            ControlMessage::Shutdown(reason) => encode(reason)?,
            ControlMessage::Unsupported(mt) => encode(mt)?,
            ControlMessage::ChannelOpen {
                channel,
                channel_type,
            } => encode(&(channel, channel_type))?,
            ControlMessage::ChannelAccept { channel }
            | ControlMessage::ChannelClose { channel }
            | ControlMessage::ChannelClosed { channel } => encode(channel)?,
            ControlMessage::ChannelReject { channel, reason } => encode(&(channel, reason))?,
            ControlMessage::ProtocolError(detail) => encode(detail)?,
            ControlMessage::ChannelError { channel, detail } => encode(&(channel, detail))?,
        };
        Ok(SessionFrame::new(
            ChannelId::CONTROL,
            self.message_type(),
            MessageFlags::END_OF_MESSAGE,
            payload,
        ))
    }

    /// Decode from a control-channel frame. An unrecognized control message type
    /// is a [`SessionError::FrameDecode`], which the session turns into an
    /// `Unsupported` reply.
    pub fn from_frame(frame: &SessionFrame) -> Result<ControlMessage, SessionError> {
        match frame.message_type.get() {
            id::HELLO => Ok(ControlMessage::Hello(decode(&frame.payload)?)),
            id::PING => Ok(ControlMessage::Ping(decode(&frame.payload)?)),
            id::PONG => Ok(ControlMessage::Pong(decode(&frame.payload)?)),
            id::SHUTDOWN => Ok(ControlMessage::Shutdown(decode(&frame.payload)?)),
            id::UNSUPPORTED => Ok(ControlMessage::Unsupported(decode(&frame.payload)?)),
            id::CHANNEL_OPEN => {
                let (channel, channel_type) = decode(&frame.payload)?;
                Ok(ControlMessage::ChannelOpen {
                    channel,
                    channel_type,
                })
            }
            id::CHANNEL_ACCEPT => Ok(ControlMessage::ChannelAccept {
                channel: decode(&frame.payload)?,
            }),
            id::CHANNEL_REJECT => {
                let (channel, reason) = decode(&frame.payload)?;
                Ok(ControlMessage::ChannelReject { channel, reason })
            }
            id::CHANNEL_CLOSE => Ok(ControlMessage::ChannelClose {
                channel: decode(&frame.payload)?,
            }),
            id::CHANNEL_CLOSED => Ok(ControlMessage::ChannelClosed {
                channel: decode(&frame.payload)?,
            }),
            id::PROTOCOL_ERROR => Ok(ControlMessage::ProtocolError(decode(&frame.payload)?)),
            id::CHANNEL_ERROR => {
                let (channel, detail) = decode(&frame.payload)?;
                Ok(ControlMessage::ChannelError { channel, detail })
            }
            other => Err(SessionError::FrameDecode(format!(
                "unknown control message type {other}"
            ))),
        }
    }
}

/// Serialize a control body to JSON bytes.
fn encode<T: Serialize>(value: &T) -> Result<Bytes, SessionError> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|e| SessionError::Serialization(e.to_string()))
}

/// Deserialize a control body from JSON bytes.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, SessionError> {
    serde_json::from_slice(bytes).map_err(|e| SessionError::Serialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::{Capability, ChannelType};

    fn hello() -> ControlMessage {
        ControlMessage::Hello(SessionHello {
            version: Version::CURRENT,
            capabilities: CapabilitySet::new().with(Capability::new(ChannelType::CONTROL)),
            session_id: SessionId::from_u128(7),
        })
    }

    #[test]
    fn control_messages_roundtrip_through_frames() {
        for msg in [
            hello(),
            ControlMessage::Ping(42),
            ControlMessage::Pong(42),
            ControlMessage::Shutdown("bye".into()),
            ControlMessage::Unsupported(99),
        ] {
            let frame = msg.to_frame().expect("encode");
            assert!(frame.channel.is_control());
            let decoded = ControlMessage::from_frame(&frame).expect("decode");
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn unknown_control_type_is_rejected() {
        let frame = SessionFrame::new(
            ChannelId::CONTROL,
            MessageType::new(4321),
            MessageFlags::END_OF_MESSAGE,
            Bytes::from_static(b"null"),
        );
        assert!(matches!(
            ControlMessage::from_frame(&frame),
            Err(SessionError::FrameDecode(_))
        ));
    }

    #[test]
    fn corrupt_body_is_a_serialization_error() {
        let frame = SessionFrame::new(
            ChannelId::CONTROL,
            MessageType::new(id::PING),
            MessageFlags::END_OF_MESSAGE,
            Bytes::from_static(b"not-a-number"),
        );
        assert!(matches!(
            ControlMessage::from_frame(&frame),
            Err(SessionError::Serialization(_))
        ));
    }
}
