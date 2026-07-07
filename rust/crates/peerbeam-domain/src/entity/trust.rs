//! Device trust entity (persisted, TOFU-style).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// A record that a device's identity key has been trusted by the user.
///
/// Trust-on-first-use: the fingerprint is pinned on first pairing and
/// compared on every subsequent connection to detect key changes.
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
}
