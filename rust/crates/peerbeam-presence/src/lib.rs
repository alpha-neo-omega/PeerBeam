//! Device presence: the `Presence` capability on PeerSession (ChannelType
//! `0x0103`, `docs/MESSAGE_REGISTRY.md` §2).
//!
//! One message type — [`Status`] — carrying what a device can honestly say
//! about itself: battery, free storage, how it reaches the network, and its app
//! version. It rides an ordinary message channel, exactly like Chat, so it
//! inherits the session's authentication, sealing and channel semantics and
//! adds no transport of its own (I2).
//!
//! # The privacy story
//!
//! Two gates gate every outbound status, and they are the whole feature:
//!
//! * **Opt-in, default off.** One setting, *"Share device status with trusted
//!   devices"*. While it is off this device sends nothing at all.
//! * **Trusted-only, not configurable.** A status never leaves for a peer that
//!   is not trusted, whatever the setting says.
//!
//! Both live in one function, [`may_share_status`], which
//! [`PresenceSender::beat`] consults before it opens a channel or sends a
//! frame. Receiving is unconditional: a device that shares nothing still shows
//! everyone else's status.
//!
//! Nothing is persisted (I4). Presence is live state, so a restart starts empty
//! rather than presenting stale numbers as current.

mod collect;
mod gate;
mod handler;
mod message;
mod registry;
mod send;

pub use collect::{battery, collect, network, storage_free};
pub use gate::{caps_support_status, may_share_status};
pub use handler::{PresenceHandler, PresenceSink, RingSink};
pub use message::{
    is_known_network, PresenceError, Ring, Status, MAX_BATTERY_PERCENT, MAX_RING_SECONDS, MSG_RING,
    MSG_STATUS, NETWORK_KINDS,
};
pub use registry::{PeerStatus, PresenceRegistry};
pub use send::{Beat, PresenceSender, SendError, SharingSetting, StatusSource, HEARTBEAT_INTERVAL};
