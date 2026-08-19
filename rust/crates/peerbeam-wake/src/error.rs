//! The one error surface for waking a device.

use thiserror::Error;

use crate::mac::MacError;

/// Why a wake did not go out.
///
/// Note what is **not** here: any variant meaning "the device did not wake up".
/// Nothing in this crate can produce one — see [`crate::WakeAttempt`]. Every
/// variant below is about a packet that never left this machine.
#[derive(Debug, Error)]
pub enum WakeError {
    /// The text given was not a hardware address.
    #[error(transparent)]
    Mac(#[from] MacError),

    /// The [`AppStore`](peerbeam_domain::port::AppStore) beneath could not be
    /// read or written.
    #[error("wake store: {0}")]
    Storage(String),

    /// No hardware address has been recorded for this device, so there is
    /// nothing to address a packet to. Distinct from
    /// [`NotPermitted`](WakeError::NotPermitted) because the remedies are
    /// different: record the address, versus approve the device.
    #[error("no hardware address is recorded for {device}, so it cannot be woken")]
    NotRecorded { device: String },

    /// The user has not approved this device — see [`crate::may_wake`].
    #[error("{device} is not an approved device, so PeerBeam will not wake it")]
    NotPermitted { device: String },

    /// Not one of the broadcast targets accepted the packet. Carries the last
    /// operating-system error, which on a desktop is almost always "permission
    /// denied" (broadcast not enabled on the socket) or "network unreachable"
    /// (no interface up).
    #[error("no magic packet could be sent: {0}")]
    Send(String),
}
