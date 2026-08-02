//! Control-channel messages: the session's own protocol.
//!
//! These ride on [`ChannelId::CONTROL`]. The [`SessionFrame::message_type`]
//! selects the message; the payload is the serialized body. Ids follow the
//! message registry (`docs/MESSAGE_REGISTRY.md`).

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{
    CapabilitySet, ChannelId, MessageFlags, MessageType, SessionError, SessionFrame, SessionId,
    Version,
};

/// Control message type ids (within the control channel's namespace).
pub mod id {
    /// Version + capability announcement.
    pub const HELLO: u16 = 1;
    /// Keepalive request.
    pub const PING: u16 = 6;
    /// Keepalive reply.
    pub const PONG: u16 = 7;
    /// Graceful teardown.
    pub const SHUTDOWN: u16 = 8;
    /// "I do not understand message type N."
    pub const UNSUPPORTED: u16 = 11;
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
        })
    }

    /// Encode as a control-channel [`SessionFrame`].
    pub fn to_frame(&self) -> Result<SessionFrame, SessionError> {
        let payload = match self {
            ControlMessage::Hello(hello) => encode(hello)?,
            ControlMessage::Ping(nonce) | ControlMessage::Pong(nonce) => encode(nonce)?,
            ControlMessage::Shutdown(reason) => encode(reason)?,
            ControlMessage::Unsupported(mt) => encode(mt)?,
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
