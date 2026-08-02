//! Errors raised by the session layer.

use super::negotiation::Version;
use super::state::SessionState;
use crate::error::DomainError;

/// A session-layer failure. Distinct from [`DomainError`] so callers can match on
/// session-specific conditions; a transport failure surfaced from a [`Link`] is
/// wrapped as [`SessionError::Link`].
///
/// [`Link`]: crate::port::Link
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// The peers do not share a protocol major version.
    #[error("protocol version incompatible: local {local}, peer {peer}")]
    VersionIncompatible {
        /// This side's version.
        local: Version,
        /// The peer's version.
        peer: Version,
    },

    /// An attempt was made to move the session between states illegally.
    #[error("illegal session state transition: {from:?} -> {to:?}")]
    InvalidTransition {
        /// The current state.
        from: SessionState,
        /// The rejected target state.
        to: SessionState,
    },

    /// A frame could not be decoded.
    #[error("frame decode failed: {0}")]
    FrameDecode(String),

    /// A control message arrived that is not valid in the current state.
    #[error("unexpected message in state {state:?}: {detail}")]
    UnexpectedMessage {
        /// The state the session was in.
        state: SessionState,
        /// What was unexpected.
        detail: String,
    },

    /// The session is closed and cannot service the request.
    #[error("session is closed")]
    Closed,

    /// The peer failed to respond within the keepalive/idle window.
    #[error("keepalive timeout")]
    Timeout,

    /// The underlying transport link failed.
    #[error("link error: {0}")]
    Link(String),

    /// A control payload could not be (de)serialized.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A channel-layer error (capability not negotiated, channel limit reached,
    /// unknown channel, …).
    #[error("channel error: {0}")]
    Channel(String),
}

impl From<DomainError> for SessionError {
    /// A transport/link failure surfaced by the [`Link`](crate::port::Link) port
    /// becomes [`SessionError::Link`], preserving the message.
    fn from(err: DomainError) -> Self {
        SessionError::Link(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_error_maps_to_link() {
        let e: SessionError = DomainError::Connection("peer closed".into()).into();
        assert!(matches!(e, SessionError::Link(_)));
        assert!(e.to_string().contains("peer closed"));
    }

    #[test]
    fn errors_are_comparable() {
        let a = SessionError::VersionIncompatible {
            local: Version::new(1, 0),
            peer: Version::new(2, 0),
        };
        let b = SessionError::VersionIncompatible {
            local: Version::new(1, 0),
            peer: Version::new(2, 0),
        };
        assert_eq!(a, b);
        assert_ne!(a, SessionError::Closed);
    }
}
