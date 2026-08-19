//! RouteManager: priority ranking, failover, relay-as-last-resort, dedup, and
//! route migration on reconnect. Uses a fake transport (dial succeeds only for
//! configured route kinds, records every attempt) and a map-based classifier so
//! all seven route classes can be exercised.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;

use peerbeam_domain::entity::{
    Device, DeviceType, Direction, Platform, Route, RouteKind, TransferSession, TransferStatus,
};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, ProviderId, TransferId};
use peerbeam_domain::port::{Bind, Frame, Link, Protocol, TransferProvider};
use peerbeam_engine::{RouteClassifier, RouteManager};
use peerbeam_transfer::LinkFactory;

// ── fakes ───────────────────────────────────────────────────────

struct FakeLink;

#[async_trait]
impl Link for FakeLink {
    async fn send_frame(&mut self, _f: Frame) -> Result<()> {
        Ok(())
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        Ok(None)
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Dials succeed only for route kinds currently in `up`; every attempt is
/// recorded so tests can assert the order routes were tried in.
struct FakeTransport {
    up: Arc<Mutex<Vec<RouteKind>>>,
    attempts: Arc<Mutex<Vec<RouteKind>>>,
}

impl FakeTransport {
    fn new(up: &[RouteKind]) -> Self {
        Self {
            up: Arc::new(Mutex::new(up.to_vec())),
            attempts: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn attempts(&self) -> Vec<RouteKind> {
        self.attempts.lock().unwrap().clone()
    }
    fn clear_attempts(&self) {
        self.attempts.lock().unwrap().clear();
    }
}

#[async_trait]
impl TransferProvider for FakeTransport {
    fn id(&self) -> ProviderId {
        ProviderId::from("fake")
    }
    fn protocol(&self) -> Protocol {
        Protocol::Quic
    }
    async fn dial(&self, route: &Route, _s: &TransferSession) -> Result<Box<dyn Link>> {
        self.attempts.lock().unwrap().push(route.kind);
        if self.up.lock().unwrap().contains(&route.kind) {
            Ok(Box::new(FakeLink))
        } else {
            Err(DomainError::Connection(format!("{:?} down", route.kind)))
        }
    }
    async fn serve(&self, _b: Bind) -> Result<BoxStream<'static, Result<Box<dyn Link>>>> {
        Err(DomainError::Connection("serve unused".into()))
    }
}

/// Classifier mapping fixed address strings to route kinds (so a test can build
/// a peer whose addresses span USB/Ethernet/Wi-Fi, which IP ranges can't).
struct MapClassifier(HashMap<String, RouteKind>);

impl RouteClassifier for MapClassifier {
    fn classify(&self, address: &str) -> RouteKind {
        *self.0.get(address).unwrap_or(&RouteKind::DirectInternet)
    }
}

fn classifier(pairs: &[(&str, RouteKind)]) -> Arc<dyn RouteClassifier> {
    Arc::new(MapClassifier(
        pairs.iter().map(|(a, k)| (a.to_string(), *k)).collect(),
    ))
}

fn peer(addresses: &[&str]) -> Device {
    Device {
        id: DeviceId::from("peer"),
        name: "Peer".into(),
        device_type: DeviceType::Laptop,
        platform: Platform::Linux,
        addresses: addresses.iter().map(|s| s.to_string()).collect(),
        port: 49600,
        last_seen: Utc::now(),
    }
}

fn session() -> TransferSession {
    TransferSession {
        id: TransferId::from("t"),
        peer: DeviceId::from("peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: Vec::new(),
        total_bytes: 0,
        transferred_bytes: 0,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted: true,
    }
}

// ── tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn candidates_ranked_by_priority() {
    // Addresses given out of priority order; must come back best-first.
    let cls = classifier(&[
        ("relay", RouteKind::Relay),
        ("wifi", RouteKind::Wifi),
        ("lan", RouteKind::Lan),
        ("ts", RouteKind::TailscaleDirect),
        ("eth", RouteKind::Ethernet),
        ("usb", RouteKind::UsbTether),
    ]);
    let mgr = RouteManager::new(Arc::new(FakeTransport::new(&[]))).with_classifier(cls);
    let kinds: Vec<RouteKind> = mgr
        .candidates(&peer(&["relay", "wifi", "lan", "ts", "eth", "usb"]))
        .iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            RouteKind::Lan,
            RouteKind::UsbTether,
            RouteKind::Ethernet,
            RouteKind::Wifi,
            RouteKind::TailscaleDirect,
            RouteKind::Relay,
        ]
    );
}

#[tokio::test]
async fn selects_highest_priority_reachable() {
    let cls = classifier(&[
        ("lan", RouteKind::Lan),
        ("eth", RouteKind::Ethernet),
        ("wifi", RouteKind::Wifi),
    ]);
    let fake = Arc::new(FakeTransport::new(&[
        RouteKind::Lan,
        RouteKind::Ethernet,
        RouteKind::Wifi,
    ]));
    let mgr = RouteManager::new(fake.clone()).with_classifier(cls);
    mgr.connect(&peer(&["wifi", "lan", "eth"]), &session())
        .await
        .expect("connects");
    // LAN is highest priority and up → chosen on the first attempt.
    assert_eq!(fake.attempts(), vec![RouteKind::Lan]);
}

#[tokio::test]
async fn fails_over_past_unreachable_routes() {
    let cls = classifier(&[
        ("lan", RouteKind::Lan),
        ("usb", RouteKind::UsbTether),
        ("eth", RouteKind::Ethernet),
        ("wifi", RouteKind::Wifi),
    ]);
    // Only Ethernet + Wi-Fi are up; LAN and USB must be skipped.
    let fake = Arc::new(FakeTransport::new(&[RouteKind::Ethernet, RouteKind::Wifi]));
    let mgr = RouteManager::new(fake.clone()).with_classifier(cls);
    mgr.connect(&peer(&["lan", "usb", "eth", "wifi"]), &session())
        .await
        .expect("fails over to Ethernet");
    assert_eq!(
        fake.attempts(),
        vec![RouteKind::Lan, RouteKind::UsbTether, RouteKind::Ethernet],
        "tries in priority order and stops at the first reachable"
    );
}

#[tokio::test]
async fn all_routes_down_errors() {
    let cls = classifier(&[("lan", RouteKind::Lan), ("wifi", RouteKind::Wifi)]);
    let fake = Arc::new(FakeTransport::new(&[])); // nothing up
    let mgr = RouteManager::new(fake.clone()).with_classifier(cls);
    let r = mgr.connect(&peer(&["lan", "wifi"]), &session()).await;
    assert!(r.is_err());
    assert_eq!(fake.attempts(), vec![RouteKind::Lan, RouteKind::Wifi]);
}

#[tokio::test]
async fn no_addresses_errors_without_dialing() {
    let fake = Arc::new(FakeTransport::new(&[RouteKind::Lan]));
    let mgr = RouteManager::new(fake.clone());
    assert!(mgr.connect(&peer(&[]), &session()).await.is_err());
    assert!(fake.attempts().is_empty(), "no routes → no dial attempts");
}

#[tokio::test]
async fn relay_is_last_resort() {
    let cls = classifier(&[("lan", RouteKind::Lan)]);
    // LAN is down; only the relay is reachable.
    let fake = Arc::new(FakeTransport::new(&[RouteKind::Relay]));
    let mgr = RouteManager::new(fake.clone())
        .with_classifier(cls)
        .with_relays(vec![Route {
            kind: RouteKind::Relay,
            address: "relay.example".into(),
            port: 443,
        }]);
    mgr.connect(&peer(&["lan"]), &session())
        .await
        .expect("falls back to relay");
    assert_eq!(
        fake.attempts(),
        vec![RouteKind::Lan, RouteKind::Relay],
        "relay is only tried after every direct route"
    );
}

#[tokio::test]
async fn duplicate_addresses_are_deduped() {
    let cls = classifier(&[("lan", RouteKind::Lan)]);
    let mgr = RouteManager::new(Arc::new(FakeTransport::new(&[]))).with_classifier(cls);
    let cands = mgr.candidates(&peer(&["lan", "lan", "lan"]));
    assert_eq!(cands.len(), 1, "identical routes collapse to one");
}

#[tokio::test]
async fn migrates_to_next_route_on_reconnect() {
    let cls = classifier(&[("lan", RouteKind::Lan), ("eth", RouteKind::Ethernet)]);
    let fake = Arc::new(FakeTransport::new(&[RouteKind::Lan, RouteKind::Ethernet]));
    let mgr = RouteManager::new(fake.clone()).with_classifier(cls);
    let mut factory = mgr.link_factory(peer(&["lan", "eth"]), session());

    // First connect: LAN is best and up.
    factory.connect().await.expect("first link over LAN");
    assert_eq!(fake.attempts(), vec![RouteKind::Lan]);

    // LAN goes down; a reconnect must migrate to Ethernet automatically.
    fake.clear_attempts();
    fake.up.lock().unwrap().retain(|k| *k != RouteKind::Lan);
    factory
        .connect()
        .await
        .expect("reconnect migrates to Ethernet");
    assert_eq!(
        fake.attempts(),
        vec![RouteKind::Lan, RouteKind::Ethernet],
        "reconnect re-selects and migrates to the next best route"
    );
}

// ── link quality ────────────────────────────────────────────────

/// A discovery provider that reports one device and then goes quiet — enough
/// for the device list to have a row a measurement can land on.
struct OneDevice(Device);

#[async_trait]
impl peerbeam_domain::port::DiscoveryProvider for OneDevice {
    fn id(&self) -> ProviderId {
        ProviderId::from("fake")
    }
    fn capabilities(&self) -> peerbeam_domain::port::DiscoveryCaps {
        peerbeam_domain::port::DiscoveryCaps {
            can_advertise: true,
            can_scan: true,
            crosses_subnet: false,
            requires_tailscale: false,
        }
    }
    async fn advertise(&self, _me: &Device) -> Result<()> {
        Ok(())
    }
    async fn scan(&self) -> Result<()> {
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        Ok(())
    }
    fn events(&self) -> BoxStream<'static, peerbeam_domain::port::DiscoveryEvent> {
        futures::stream::iter(vec![peerbeam_domain::port::DiscoveryEvent::Found(
            self.0.clone(),
        )])
        .boxed()
    }
}

/// The whole point of the feature, end to end: a round trip the transport
/// measured lands on the peer's row in the device list *and* is announced, so
/// a surface re-renders instead of polling.
///
/// The rounding is asserted on the way through rather than in isolation —
/// 2.7 ms must arrive as 3 ms, because a floor would quietly turn every fast
/// LAN link into the same number.
#[tokio::test]
async fn a_measured_round_trip_reaches_the_device_list_and_is_announced() {
    let bob = peer(&["10.0.0.5"]);
    let engine = std::sync::Arc::new(
        peerbeam_engine::EngineBuilder::with_defaults()
            .with_discovery(Arc::new(OneDevice(bob.clone())))
            .build()
            .expect("engine builds"),
    );
    let mut changes = engine.device_changes();
    engine
        .start_discovery(peer(&["10.0.0.99"]))
        .await
        .expect("discovery starts");
    // Wait for the row itself to exist; a measurement against an unknown
    // device is a no-op, so asserting before it lands would pass for the
    // wrong reason.
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), changes.recv()).await {
            Ok(Ok(peerbeam_engine::DeviceChange::Added(_))) => break,
            Ok(Ok(_)) => continue,
            other => panic!("device never appeared: {other:?}"),
        }
    }

    let transport = Arc::new(FakeTransport::new(&[]));
    let rm = RouteManager::new(transport).reporting_to(&engine);
    rm.record_link_rtt(&bob.id, Some(std::time::Duration::from_micros(2_700)));

    assert_eq!(
        engine
            .devices()
            .into_iter()
            .find(|m| m.device.id == bob.id)
            .expect("the peer is tracked")
            .latency_ms,
        Some(3),
        "2.7 ms must arrive as 3 ms, not floored to 2"
    );

    let announced = tokio::time::timeout(std::time::Duration::from_secs(2), changes.recv())
        .await
        .expect("a change was announced")
        .expect("the change stream is live");
    assert!(
        matches!(
            announced,
            peerbeam_engine::DeviceChange::LatencyChanged { ref id, latency_ms: Some(3) }
                if *id == bob.id
        ),
        "expected a LatencyChanged for {:?}, got {announced:?}",
        bob.id
    );

    // Nothing measurable clears the row rather than leaving a figure that is
    // no longer about any link we hold.
    rm.record_link_rtt(&bob.id, None);
    assert_eq!(
        engine
            .devices()
            .into_iter()
            .find(|m| m.device.id == bob.id)
            .unwrap()
            .latency_ms,
        None
    );

    engine.stop_discovery().await.expect("discovery stops");
}
