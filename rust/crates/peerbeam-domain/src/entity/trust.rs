//! Device trust entity (persisted, TOFU-style).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// A record that a device's identity key has been trusted by the user.
///
/// Trust-on-first-use: the fingerprint is pinned on first pairing and
/// compared on every subsequent connection to detect key changes.
///
/// Pinning a key (MITM protection) is deliberately separate from being
/// *approved* for auto-accept: a device's key is pinned as soon as it is
/// first seen, regardless of whether the user accepts or declines the
/// transfer that triggered the handshake. Only an explicit accept should
/// let future connections skip the approval prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustRecord {
    /// The trusted device.
    pub device: DeviceId,
    /// Hex fingerprint of the device's long-term public key.
    pub fingerprint: String,
    /// Name the device presented when trusted.
    pub name: String,
    /// When trust was established.
    pub trusted_at: DateTime<Utc>,
    /// Whether the user has explicitly accepted a transfer from this device,
    /// making it eligible for auto-accept on future connections.
    ///
    /// `#[serde(default)]` so trust stores written before this field existed
    /// still load — those records deserialize as `approved: false`, requiring
    /// one more explicit approval after upgrading (fail-closed, not a
    /// silent auto-accept).
    #[serde(default)]
    pub approved: bool,
}
