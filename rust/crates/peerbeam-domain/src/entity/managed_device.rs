//! The UI-facing device model.
//!
//! A [`Device`] is what a discovery provider reports. A [`ManagedDevice`] is
//! what the application layer maintains *about* a device after merging all
//! providers: liveness, measured latency, and which transports can reach it.
//! Frontends render `ManagedDevice`s and never see raw provider events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::ProviderId;

use super::Device;

/// How a device can be reached, derived from the discovery providers that
/// currently see it. Lets the UI badge devices (LAN vs remote/Tailscale)
/// and lets route selection prefer local paths — all without the UI knowing
/// any networking detail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Reachable on the local subnet (seen by a non-cross-subnet provider).
    pub reachable_lan: bool,
    /// Reachable beyond the local subnet (seen by a cross-subnet provider).
    pub reachable_remote: bool,
    /// Reachable only while Tailscale is up (every provider seeing it
    /// requires Tailscale).
    pub requires_tailscale: bool,
    /// Discovery providers currently reporting this device (sorted).
    pub providers: Vec<ProviderId>,
}

/// A device as tracked by the application layer across all providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedDevice {
    /// The merged device record (identity + unioned addresses).
    pub device: Device,
    /// Whether at least one provider currently sees it.
    pub online: bool,
    /// When it was most recently observed.
    pub last_seen: DateTime<Utc>,
    /// Most recent measured round-trip latency, if any.
    pub latency_ms: Option<u32>,
    /// How it can be reached.
    pub capabilities: DeviceCapabilities,
}
