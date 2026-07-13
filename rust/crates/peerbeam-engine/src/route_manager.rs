//! Automatic route selection with failover and migration.
//!
//! A peer is often reachable several ways at once — a LAN address *and* a
//! Tailscale address, say. `RouteManager` picks the best one automatically and
//! hides the choice from everything above it. Its priority, best first:
//!
//! ```text
//! LAN → USB tethering → Ethernet → Wi-Fi → Tailscale → (direct internet) → Relay
//! ```
//!
//! (This is exactly `RouteKind`'s ordering, so ranking is a sort.)
//!
//! **One API.** [`RouteManager::connect`] takes a peer and returns a live
//! `Link`. It tries candidates in priority order and *fails over* to the next
//! on error, so the returned link is always the highest-priority route that is
//! actually reachable. The caller (UI, transfer engine) receives only a `Link`
//! and never learns which route was used — the choice is logged, not exposed.
//!
//! **Migration.** [`RouteManager::link_factory`] yields a
//! [`LinkFactory`](peerbeam_transfer::LinkFactory): each reconnect re-evaluates
//! the candidates, so a transfer that loses its LAN link resumes over the next
//! best route automatically (via the recovery driver).

use std::sync::Arc;

use async_trait::async_trait;

use peerbeam_domain::entity::{Device, Route, TransferSession};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Link, TransferProvider};
use peerbeam_transfer::LinkFactory;

use crate::route_classifier::{AddressClassifier, RouteClassifier};

/// Selects and dials the best available route to a peer, with failover.
pub struct RouteManager {
    transport: Arc<dyn TransferProvider>,
    classifier: Arc<dyn RouteClassifier>,
    /// Configured last-resort relay routes (always `RouteKind::Relay`).
    relays: Vec<Route>,
}

impl RouteManager {
    /// Build with the default address-range classifier.
    pub fn new(transport: Arc<dyn TransferProvider>) -> Self {
        Self {
            transport,
            classifier: Arc::new(AddressClassifier),
            relays: Vec::new(),
        }
    }

    /// Override the route classifier (e.g. an interface-aware one).
    pub fn with_classifier(mut self, classifier: Arc<dyn RouteClassifier>) -> Self {
        self.classifier = classifier;
        self
    }

    /// Add last-resort relay routes, tried only after every direct route.
    pub fn with_relays(mut self, relays: Vec<Route>) -> Self {
        self.relays = relays;
        self
    }

    /// Ranked candidate routes to `peer`, best (highest priority) first,
    /// deduplicated. Each of the peer's addresses becomes a route classified
    /// into its priority class; configured relays are appended.
    pub fn candidates(&self, peer: &Device) -> Vec<Route> {
        let mut routes: Vec<Route> = peer
            .addresses
            .iter()
            .filter(|a| !a.is_empty())
            .map(|address| Route {
                kind: self.classifier.classify(address),
                address: address.clone(),
                port: peer.port,
            })
            .collect();
        routes.extend(self.relays.iter().cloned());

        // Highest priority first (RouteKind::Lan is the smallest, so ascending).
        routes.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.address.cmp(&b.address)));
        routes.dedup();
        routes
    }

    /// **The single API.** Connect to `peer` over the best reachable route,
    /// failing over to the next candidate on error. Returns a live `Link`; the
    /// route used is logged, never returned.
    pub async fn connect(&self, peer: &Device, session: &TransferSession) -> Result<Box<dyn Link>> {
        let candidates = self.candidates(peer);
        if candidates.is_empty() {
            return Err(DomainError::Connection(format!(
                "no routes to {}",
                peer.name
            )));
        }

        let mut last_err: Option<DomainError> = None;
        for route in candidates {
            match self.transport.dial(&route, session).await {
                Ok(link) => {
                    tracing::info!(
                        peer = %peer.name,
                        kind = ?route.kind,
                        address = %route.address,
                        "route selected"
                    );
                    return Ok(link);
                }
                Err(e) => {
                    tracing::warn!(
                        peer = %peer.name,
                        kind = ?route.kind,
                        address = %route.address,
                        error = %e,
                        "route unavailable, failing over"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            DomainError::Connection(format!("all routes to {} failed", peer.name))
        }))
    }

    /// A [`LinkFactory`] bound to `peer`/`session`. Each `connect()` re-selects
    /// the best route, so a dropped transfer resumes over whatever route is
    /// currently best (route migration) when driven by the recovery loop.
    pub fn link_factory(&self, peer: Device, session: TransferSession) -> RouteLinkFactory<'_> {
        RouteLinkFactory {
            manager: self,
            peer,
            session,
        }
    }
}

/// [`LinkFactory`] over a [`RouteManager`] — re-selects the best route on every
/// reconnect (route migration).
pub struct RouteLinkFactory<'a> {
    manager: &'a RouteManager,
    peer: Device,
    session: TransferSession,
}

#[async_trait]
impl LinkFactory for RouteLinkFactory<'_> {
    async fn connect(&mut self) -> Result<Box<dyn Link>> {
        self.manager.connect(&self.peer, &self.session).await
    }
}
