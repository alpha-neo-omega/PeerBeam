//! Route port: how candidate paths to a peer are produced and probed.

use async_trait::async_trait;

use crate::entity::{Device, Route, RouteHealth};
use crate::error::Result;
use crate::id::ProviderId;

/// Produces and probes candidate routes to a peer. The engine's route
/// selector combines candidates from all providers, probes them, and picks
/// the highest-priority healthy route automatically.
#[async_trait]
pub trait RouteProvider: Send + Sync {
    /// Stable id of this provider instance.
    fn id(&self) -> ProviderId;

    /// Enumerate candidate routes to `peer`.
    async fn candidates(&self, peer: &Device) -> Result<Vec<Route>>;

    /// Probe a candidate route for reachability and latency.
    async fn probe(&self, route: &Route) -> Result<RouteHealth>;
}
