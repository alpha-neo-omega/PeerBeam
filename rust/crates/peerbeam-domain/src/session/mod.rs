//! PeerSession domain foundation.
//!
//! The value types, the [`MessageHandler`] port, and the pure negotiation and
//! state-machine logic for PeerBeam's single peer-communication abstraction
//! (`Peer → PeerSession → Typed Message → Handler → Engine`). This module is
//! IO-free and runtime-free per the architectural invariants; the session
//! *runtime* (driving a `Link`, the registries, keepalive) lives in the transfer
//! layer, built on these types.
//!
//! See `docs/PEERSESSION_SPEC.md` and `docs/MESSAGE_REGISTRY.md`.

mod channel_state;
mod error;
mod frame;
mod handler;
mod ids;
mod negotiation;
mod state;

pub use channel_state::ChannelState;
pub use error::SessionError;
pub use frame::{MessageFlags, SessionFrame};
pub use handler::MessageHandler;
pub use ids::{ChannelId, ChannelType, MessageType, SessionId};
pub use negotiation::{
    negotiate_version, Capability, CapabilitySet, Version, VersionNegotiation,
    CHAT_FEAT_FILEDECLINE, CHAT_FEAT_FILEREF, CHAT_FEAT_REACTION, CHAT_FEAT_RECEIPT,
    CLIPBOARD_FEAT_CLIP, NOTES_FEAT_SYNC, PIPE_FEAT_STREAM, PRESENCE_FEAT_STATUS,
};
pub use state::SessionState;
