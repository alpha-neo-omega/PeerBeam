//! Engine-level device management: providers registered via the builder are
//! merged, deduped, tracked (online/offline), capability-tagged, and surfaced
//! to the UI as `DeviceChange`s and a `devices()` snapshot.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::time::timeout;

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};
use peerbeam_engine::{DeviceChange, EngineBuilder};

/// Provider replaying a fixed script with declared capabilities.
struct Fake {
    id: ProviderId,
    caps: DiscoveryCaps,
    script: Vec<DiscoveryEvent>,
}

#[async_trait]
impl DiscoveryProvider for Fake {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }
    fn capabilities(&self) -> DiscoveryCaps {
        self.caps
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
        name: id.to_string(),
        device_type: DeviceType::Desktop,
        platform: Platform::Linux,
        addresses: vec![addr.to_string()],
        port: 9000,
        last_seen: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn manages_devices_across_providers() {
    let udp = Arc::new(Fake {
        id: ProviderId::from("udp"),
        caps: DiscoveryCaps {
            can_advertise: true,
            can_scan: true,
            crosses_subnet: false,
            requires_tailscale: false,
        },
        script: vec![
            DiscoveryEvent::Found(device("shared", "10.0.0.1")),
            DiscoveryEvent::Found(device("only-lan", "10.0.0.2")),
            DiscoveryEvent::Lost(DeviceId::from("only-lan")),
        ],
    });
    let tailscale = Arc::new(Fake {
        id: ProviderId::from("tailscale"),
        caps: DiscoveryCaps {
            can_advertise: false,
            can_scan: true,
            crosses_subnet: true,
            requires_tailscale: true,
        },
        script: vec![DiscoveryEvent::Found(device("shared", "100.64.0.1"))],
    });

    let engine = EngineBuilder::with_defaults()
        .with_discovery(udp)
        .with_discovery(tailscale)
        .build()
        .expect("engine builds");

    let mut changes = engine.device_changes();
    engine
        .start_discovery(device("me", "10.0.0.99"))
        .await
        .expect("discovery starts");

    // Drain changes over a short window.
    let mut added = Vec::new();
    let mut offline = Vec::new();
    while let Ok(Ok(change)) = timeout(Duration::from_millis(300), changes.recv()).await {
        match change {
            DeviceChange::Added(m) => added.push(m.device.id.to_string()),
            DeviceChange::StatusChanged { id, online: false } => offline.push(id.to_string()),
            _ => {}
        }
    }

    // Dedup: "shared" added once even though two providers report it.
    assert_eq!(added.iter().filter(|id| *id == "shared").count(), 1);
    assert!(added.contains(&"only-lan".to_string()));
    // only-lan's sole provider dropped it → offline.
    assert!(offline.contains(&"only-lan".to_string()));

    // Snapshot: merged view with capabilities and online/offline state.
    let snap = engine.devices();
    assert_eq!(snap.len(), 2, "shared + only-lan tracked");

    let shared = snap
        .iter()
        .find(|m| m.device.id == DeviceId::from("shared"))
        .unwrap();
    assert!(shared.online);
    assert!(shared.capabilities.reachable_lan, "seen via udp");
    assert!(shared.capabilities.reachable_remote, "seen via tailscale");
    assert!(!shared.capabilities.requires_tailscale, "also on LAN");
    assert_eq!(shared.capabilities.providers.len(), 2);
    assert!(shared.device.addresses.contains(&"10.0.0.1".to_string()));
    assert!(shared.device.addresses.contains(&"100.64.0.1".to_string()));

    let lan = snap
        .iter()
        .find(|m| m.device.id == DeviceId::from("only-lan"))
        .unwrap();
    assert!(!lan.online, "dropped by its only provider");

    // Latency is recorded on the managed device.
    engine.record_device_latency(&DeviceId::from("shared"), Some(23));
    let shared_latency = engine
        .devices()
        .into_iter()
        .find(|m| m.device.id == DeviceId::from("shared"))
        .unwrap()
        .latency_ms;
    assert_eq!(shared_latency, Some(23));

    engine.stop_discovery().await.expect("discovery stops");
}
