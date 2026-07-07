//! Discovery port: how peers become visible.

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::entity::Device;
use crate::error::Result;
use crate::id::{DeviceId, ProviderId};

/// An event from a single discovery provider.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A peer appeared.
    Found(Device),
    /// A peer's details changed.
    Updated(Device),
    /// A peer disappeared.
    Lost(DeviceId),
}

/// Declares what a discovery provider can reach, so the engine can pick
/// providers appropriate to the current network without user config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryCaps {
    /// True if this provider can find peers outside the local subnet.
    pub crosses_subnet: bool,
    /// True if this provider requires Tailscale to be up.
    pub requires_tailscale: bool,
}

/// A device-discovery mechanism (mDNS, UDP broadcast, Tailscale, …).
#[async_trait]
pub trait DiscoveryProvider: Send + Sync {
    /// Stable id of this provider instance.
    fn id(&self) -> ProviderId;

    /// What networks this provider can reach.
    fn capabilities(&self) -> DiscoveryCaps;

    /// Begin advertising this device so peers can find us.
    async fn advertise(&self, me: &Device) -> Result<()>;

    /// Begin scanning for peers.
    async fn scan(&self) -> Result<()>;

    /// Stop advertising and scanning.
    async fn stop(&self) -> Result<()>;

    /// Stream of discovery events from this provider.
    fn events(&self) -> BoxStream<'static, DiscoveryEvent>;
}
