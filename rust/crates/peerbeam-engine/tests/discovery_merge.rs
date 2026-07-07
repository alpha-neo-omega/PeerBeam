//! Engine-level discovery integration: two providers registered via the
//! builder, merged through `start_discovery`, surfacing deduplicated
//! `DomainEvent`s on the engine's event stream.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::time::timeout;

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};
use peerbeam_engine::{DomainEvent, EngineBuilder};

/// Provider replaying a fixed script.
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

#[tokio::test]
async fn engine_merges_registered_providers() {
    let udp = Arc::new(Fake {
        id: ProviderId::from("udp"),
        script: vec![
            DiscoveryEvent::Found(device("shared", "10.0.0.1")),
            DiscoveryEvent::Found(device("only-udp", "10.0.0.2")),
        ],
    });
    let mdns = Arc::new(Fake {
        id: ProviderId::from("mdns"),
        script: vec![DiscoveryEvent::Found(device("shared", "10.0.0.1"))],
    });

    let engine = EngineBuilder::with_defaults()
        .with_discovery(udp)
        .with_discovery(mdns)
        .build()
        .expect("engine builds");

    // Subscribe before starting so no events are missed.
    let mut rx = engine.subscribe();

    let me = device("me", "10.0.0.99");
    engine.start_discovery(me).await.expect("discovery starts");

    // Collect what arrives within a short window.
    let mut events = Vec::new();
    while let Ok(Ok(ev)) = timeout(Duration::from_millis(300), rx.recv()).await {
        events.push(ev);
    }

    let founds: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            DomainEvent::PeerFound(d) => Some(d.id.to_string()),
            _ => None,
        })
        .collect();

    // "shared" (seen by both) deduped to one; plus the unique "only-udp".
    assert_eq!(founds.iter().filter(|id| *id == "shared").count(), 1);
    assert!(founds.contains(&"only-udp".to_string()));
    assert_eq!(founds.len(), 2, "got {founds:?}");

    engine.stop_discovery().await.expect("discovery stops");
}
