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
//!
//! **Link quality.** [`RouteManager::reporting_to`] points the manager at the
//! engine's device list, and [`RouteManager::record_link_rtt`] puts a measured
//! round trip on a peer's row. The measurement itself is taken by the transport
//! (`QuicChannels::rtt` — quinn's own estimate for the live connection) and
//! reported here because this is the component that chose the route it was
//! taken on. Nothing probes, and nothing goes on the wire.

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;

use peerbeam_domain::entity::{Device, Route, RouteKind, TransferSession};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{Link, TransferProvider};
use peerbeam_transfer::LinkFactory;

use crate::engine::Engine;
use crate::route_classifier::{AddressClassifier, RouteClassifier};

/// Selects and dials the best available route to a peer, with failover.
pub struct RouteManager {
    transport: Arc<dyn TransferProvider>,
    classifier: Arc<dyn RouteClassifier>,
    /// Configured last-resort relay routes (always `RouteKind::Relay`).
    relays: Vec<Route>,
    /// Where a measured link round-trip time is reported, if anywhere.
    ///
    /// A `Weak`, and deliberately: the frontend that wires this holds the
    /// `RouteManager` for the life of the process, so a strong handle here
    /// would keep the whole engine graph — discovery sockets, stores, tasks —
    /// alive past the shutdown that is supposed to release it. A stale `Weak`
    /// makes [`record_link_rtt`](Self::record_link_rtt) a no-op, which is the
    /// right behaviour for a measurement nobody is left to receive.
    devices: Option<Weak<Engine>>,
}

impl RouteManager {
    /// Build with the default address-range classifier.
    pub fn new(transport: Arc<dyn TransferProvider>) -> Self {
        Self {
            transport,
            classifier: Arc::new(AddressClassifier),
            relays: Vec::new(),
            devices: None,
        }
    }

    /// Override the route classifier (e.g. an interface-aware one).
    pub fn with_classifier(mut self, classifier: Arc<dyn RouteClassifier>) -> Self {
        self.classifier = classifier;
        self
    }

    /// Classify one address into its route class, using this manager's
    /// injected classifier.
    ///
    /// Exposed so callers that already hold an address — an accepted
    /// connection's remote, say — can ask *the same* classifier
    /// [`candidates`](Self::candidates) uses, instead of standing up a second
    /// one that would drift from it the first time either changed. Swapping in
    /// an interface-aware classifier via
    /// [`with_classifier`](Self::with_classifier) therefore changes both
    /// answers together, which is the point.
    #[must_use]
    pub fn classify(&self, address: &str) -> RouteKind {
        self.classifier.classify(address)
    }

    /// Add last-resort relay routes, tried only after every direct route.
    pub fn with_relays(mut self, relays: Vec<Route>) -> Self {
        self.relays = relays;
        self
    }

    /// Report measured link round-trip times into `engine`'s device list.
    ///
    /// The seam `docs/DEVICES.md` describes and nothing was filling:
    /// "`record_device_latency` stores a per-device RTT fed by the networking
    /// layer (measurement is not the manager's job)". This *is* the networking
    /// layer — it is the component that chose the route the measurement was
    /// taken on — so the number reaches the device list from here rather than
    /// from a surface that would have to re-derive which link it was about.
    ///
    /// Optional: a frontend that only dials (the CLI's one-shot commands) can
    /// skip it and nothing changes.
    pub fn reporting_to(mut self, engine: &Arc<Engine>) -> Self {
        self.devices = Some(Arc::downgrade(engine));
        self
    }

    /// Record a transport-measured round trip against `peer`'s row in the
    /// device list, or clear it when the transport measured nothing.
    ///
    /// `None` clears rather than leaving the previous figure standing: it means
    /// we hold a live connection and still cannot characterise it, and an
    /// older number is not an answer about this link.
    ///
    /// Recording against an id the device list does not know is a no-op
    /// (`DeviceStore::record_latency` ignores an unknown device), which is what
    /// makes it safe for a caller to pass whichever id it actually holds — a
    /// discovery id when it dialled one, an authenticated peer id when it
    /// accepted.
    pub fn record_link_rtt(&self, peer: &DeviceId, rtt: Option<Duration>) {
        let Some(engine) = self.devices.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        engine.record_device_latency(peer, rtt.map(rtt_millis));
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

/// Whole milliseconds, rounded to nearest, saturating.
///
/// **Not floored at 1.** A link fast enough to round to zero is a real
/// measurement of a very fast link, and rounding it up would invent a
/// millisecond that was not measured — the same substitution the presence
/// collectors refuse when they cannot read a battery. A surface renders the
/// zero as "<1 ms", which is what it means.
fn rtt_millis(rtt: Duration) -> u32 {
    u32::try_from((rtt.as_micros() + 500) / 1000).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::BoxStream;
    use peerbeam_config::EngineConfig;
    use peerbeam_domain::id::ProviderId;
    use peerbeam_domain::port::{Bind, Protocol};

    /// A transport that is never dialled. These tests are about what the
    /// manager *reports*, not what it connects to, so a provider that refuses
    /// everything keeps them honest about that.
    struct NullTransport;

    #[async_trait]
    impl TransferProvider for NullTransport {
        fn id(&self) -> ProviderId {
            ProviderId::from("null")
        }
        fn protocol(&self) -> Protocol {
            Protocol::Quic
        }
        async fn dial(&self, _r: &Route, _s: &TransferSession) -> Result<Box<dyn Link>> {
            Err(DomainError::Connection("null transport".into()))
        }
        async fn serve(&self, _b: Bind) -> Result<BoxStream<'static, Result<Box<dyn Link>>>> {
            Err(DomainError::Connection("null transport".into()))
        }
    }

    /// A link fast enough to round to zero is a real reading of a very fast
    /// link, not a missing one.
    #[test]
    fn a_sub_millisecond_link_rounds_to_zero_rather_than_up_to_one() {
        assert_eq!(rtt_millis(Duration::ZERO), 0);
        assert_eq!(rtt_millis(Duration::from_micros(1)), 0);
        assert_eq!(rtt_millis(Duration::from_micros(499)), 0);
    }

    #[test]
    fn a_round_trip_rounds_to_the_nearest_millisecond() {
        assert_eq!(rtt_millis(Duration::from_micros(500)), 1);
        assert_eq!(rtt_millis(Duration::from_micros(1_499)), 1);
        assert_eq!(rtt_millis(Duration::from_micros(1_500)), 2);
        assert_eq!(rtt_millis(Duration::from_millis(37)), 37);
    }

    /// Nothing should ever produce one — QUIC closes the connection at its
    /// 30-second idle timeout — but a truncating cast would turn an impossible
    /// reading into a small, plausible one, and a plausible wrong number is the
    /// only kind a surface cannot catch.
    #[test]
    fn an_impossibly_long_round_trip_saturates_rather_than_wrapping() {
        assert_eq!(rtt_millis(Duration::MAX), u32::MAX);
    }

    /// An unwired manager is the CLI's case and every test's case: recording
    /// must be a silent no-op, never a panic.
    #[tokio::test]
    async fn recording_without_a_device_list_does_nothing() {
        let rm = RouteManager::new(Arc::new(NullTransport));
        rm.record_link_rtt(&DeviceId::from("pb-bob"), Some(Duration::from_millis(7)));
        rm.record_link_rtt(&DeviceId::from("pb-bob"), None);
    }

    /// The reporting link must never be what keeps the engine alive: a
    /// frontend holds its `RouteManager` for the whole process, and a strong
    /// handle here would hold the engine graph open long past the shutdown
    /// meant to release it.
    #[tokio::test]
    async fn reporting_does_not_keep_the_engine_alive() {
        let engine = Arc::new(
            crate::EngineBuilder::new(EngineConfig::default())
                .build()
                .expect("an engine with no providers still builds"),
        );
        let weak = Arc::downgrade(&engine);
        let rm = RouteManager::new(Arc::new(NullTransport)).reporting_to(&engine);

        drop(engine);
        assert!(
            weak.upgrade().is_none(),
            "the reporting link kept the engine alive after its last owner dropped it"
        );
        // And a stale link is inert rather than fatal, which is why nothing has
        // to remember to unwire on the way down.
        rm.record_link_rtt(&DeviceId::from("pb-bob"), Some(Duration::from_millis(7)));
    }
}
