//! Integration test for cross-provider discovery merge.
//!
//! Drives [`merge_discovery`] with two in-memory fake providers whose event
//! streams overlap, asserting the dedup/union semantics at the stream level
//! — no sockets, no timing, fully deterministic.

use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::StreamExt;

use peerbeam_app::merge_discovery;
use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::event::DomainEvent;
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};

/// A provider that replays a fixed script of events.
struct Fake {
    id: ProviderId,
    script: Vec<DiscoveryEvent>,
}

#[async_trait]
impl DiscoveryProvider for Fake {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }
    fn capabilities(&self) -> DiscoveryCaps {
        DiscoveryCaps {
            can_advertise: true,
            can_scan: true,
            ..DiscoveryCaps::default()
        }
    }
    async fn advertise(&self, _me: &Device) -> peerbeam_domain::Result<()> {
        Ok(())
    }
    async fn scan(&self) -> peerbeam_domain::Result<()> {
        Ok(())
    }
    async fn stop(&self) -> peerbeam_domain::Result<()> {
        Ok(())
    }
    fn events(&self) -> BoxStream<'static, DiscoveryEvent> {
        futures::stream::iter(self.script.clone()).boxed()
    }
}

fn device(id: &str, addr: &str) -> Device {
    Device {
        id: DeviceId::from(id),
        name: "Peer".to_string(),
        device_type: DeviceType::Desktop,
        platform: Platform::Linux,
        addresses: vec![addr.to_string()],
        port: 9000,
        last_seen: chrono::Utc::now(),
    }
}

#[test]
fn merges_and_dedups_across_providers() {
    // "shared" is seen by both providers at the same address; each provider
    // also sees one unique device.
    let udp = Arc::new(Fake {
        id: ProviderId::from("udp"),
        script: vec![
            DiscoveryEvent::Found(device("shared", "10.0.0.1")),
            DiscoveryEvent::Found(device("only-udp", "10.0.0.2")),
        ],
    });
    let mdns = Arc::new(Fake {
        id: ProviderId::from("mdns"),
        script: vec![
            DiscoveryEvent::Found(device("shared", "10.0.0.1")),
            DiscoveryEvent::Found(device("only-mdns", "10.0.0.3")),
        ],
    });

    let providers: Vec<Arc<dyn DiscoveryProvider>> = vec![udp, mdns];
    let events: Vec<DomainEvent> = block_on(merge_discovery(&providers).collect());

    let founds: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            DomainEvent::PeerFound(d) => Some(d.id.to_string()),
            _ => None,
        })
        .collect();

    // Every distinct device appears exactly once as PeerFound.
    assert_eq!(founds.len(), 3, "got {founds:?}");
    assert_eq!(founds.iter().filter(|id| *id == "shared").count(), 1);
    assert!(founds.contains(&"only-udp".to_string()));
    assert!(founds.contains(&"only-mdns".to_string()));

    // The redundant "shared" sighting adds no new address, so no PeerUpdated.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DomainEvent::PeerUpdated(_))),
        "no update expected for identical re-sighting"
    );
}
