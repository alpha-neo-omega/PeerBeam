//! Lifecycle integration test for the mDNS provider.
//!
//! Real cross-device mDNS needs a multicast-capable network, which CI often
//! lacks, so this test only asserts the daemon-backed lifecycle
//! (advertise → scan → stop) runs to completion without hanging or erroring.
//! It skips gracefully when the mDNS daemon cannot start.

use std::time::Duration;

use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::DiscoveryProvider;

fn me() -> Device {
    Device {
        id: DeviceId::from("me-12345678"),
        name: "Me".to_string(),
        device_type: DeviceType::Desktop,
        platform: Platform::Linux,
        addresses: vec![],
        port: 4200,
        last_seen: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn advertise_scan_stop_completes() {
    let provider = match MdnsDiscovery::new(DeviceId::from("me-12345678")) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("mDNS daemon unavailable; skipping lifecycle test");
            return;
        }
    };

    let _events = provider.events();
    provider.scan().await.expect("scan");
    provider.advertise(&me()).await.expect("advertise");

    // Idempotency: second calls are no-ops.
    provider.scan().await.expect("scan idempotent");
    provider
        .advertise(&me())
        .await
        .expect("advertise idempotent");

    tokio::time::sleep(Duration::from_millis(50)).await;

    provider.stop().await.expect("stop");
}
