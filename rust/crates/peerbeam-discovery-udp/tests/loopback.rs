//! Integration tests exercising the real UDP socket over loopback.
//!
//! These bind an actual [`UdpDiscovery`] on an OS-assigned port and drive it
//! with a plain `UdpSocket` acting as a peer, so the socket setup, receive
//! loop, protocol parsing, event emission, self-filtering, query-response,
//! and liveness expiry are all covered end-to-end without depending on a
//! broadcast-capable network.

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use tokio::net::UdpSocket;
use tokio::time::timeout;

use peerbeam_discovery_udp::{Config, UdpDiscovery};
use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{DiscoveryEvent, DiscoveryProvider};

/// Config bound to an ephemeral loopback port with fast timings for tests.
fn test_config() -> Config {
    Config {
        port: 0,
        broadcast_addr: Ipv4Addr::LOCALHOST,
        interval: Duration::from_millis(100),
        peer_ttl: Duration::from_millis(250),
    }
}

fn me_device(port: u16) -> Device {
    Device {
        id: DeviceId::from("me"),
        name: "Me".to_string(),
        device_type: DeviceType::Desktop,
        platform: Platform::Linux,
        addresses: vec!["127.0.0.1".to_string()],
        port,
        last_seen: chrono::Utc::now(),
    }
}

/// A raw announce datagram as another peer would send it.
fn announce_json(id: &str, port: u16) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "kind": "announce",
        "id": id,
        "name": "PeerName",
        "device_type": "Phone",
        "platform": "android",
        "port": port,
    }))
    .unwrap()
}

#[tokio::test]
async fn discovers_peer_from_announcement() {
    let provider = UdpDiscovery::with_config(DeviceId::from("me"), test_config());
    let mut events = provider.events();
    provider.scan().await.unwrap();

    let port = provider
        .bound_port()
        .expect("socket should be bound after scan");

    // Act as a peer announcing itself to the provider.
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&announce_json("peer-1", 4321), ("127.0.0.1", port))
        .await
        .unwrap();

    let event = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("should receive an event before timeout")
        .expect("event stream should yield");

    match event {
        DiscoveryEvent::Found(d) => {
            assert_eq!(d.id, DeviceId::from("peer-1"));
            assert_eq!(d.port, 4321);
            assert_eq!(d.addresses, vec!["127.0.0.1".to_string()]);
            assert_eq!(d.platform, Platform::Android);
        }
        other => panic!("expected Found, got {other:?}"),
    }

    provider.stop().await.unwrap();
}

#[tokio::test]
async fn ignores_own_announcement_echo() {
    let provider = UdpDiscovery::with_config(DeviceId::from("me"), test_config());
    let mut events = provider.events();
    provider.scan().await.unwrap();
    let port = provider.bound_port().unwrap();

    // Send an announcement carrying OUR id — must be filtered out.
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&announce_json("me", 4321), ("127.0.0.1", port))
        .await
        .unwrap();

    let result = timeout(Duration::from_millis(500), events.next()).await;
    assert!(
        result.is_err(),
        "self-announcement must not produce an event"
    );

    provider.stop().await.unwrap();
}

#[tokio::test]
async fn responds_to_query_when_advertising() {
    let provider = UdpDiscovery::with_config(DeviceId::from("me"), test_config());
    provider.advertise(&me_device(7777)).await.unwrap();
    let port = provider.bound_port().expect("socket bound after advertise");

    // A peer asks who is here; the provider should reply directly to us.
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let query = serde_json::to_vec(&serde_json::json!({
        "v": 1, "kind": "query", "id": "peer-2",
        "name": "", "device_type": "Desktop", "platform": "linux", "port": 0,
    }))
    .unwrap();
    peer.send_to(&query, ("127.0.0.1", port)).await.unwrap();

    let mut buf = [0u8; 2048];
    let (len, _src) = timeout(Duration::from_secs(2), peer.recv_from(&mut buf))
        .await
        .expect("should receive a reply before timeout")
        .unwrap();

    let reply: serde_json::Value = serde_json::from_slice(&buf[..len]).unwrap();
    assert_eq!(reply["kind"], "announce");
    assert_eq!(reply["id"], "me");
    assert_eq!(reply["port"], 7777);

    provider.stop().await.unwrap();
}

#[tokio::test]
async fn expires_peer_after_ttl() {
    let provider = UdpDiscovery::with_config(DeviceId::from("me"), test_config());
    let mut events = provider.events();
    provider.scan().await.unwrap();
    let port = provider.bound_port().unwrap();

    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&announce_json("transient", 4321), ("127.0.0.1", port))
        .await
        .unwrap();

    // First: Found.
    let first = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("found before timeout")
        .unwrap();
    assert!(matches!(first, DiscoveryEvent::Found(_)));

    // Then, with no further announcements, the reaper should expire it.
    let lost = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("lost before timeout")
        .unwrap();
    match lost {
        DiscoveryEvent::Lost(id) => assert_eq!(id, DeviceId::from("transient")),
        other => panic!("expected Lost, got {other:?}"),
    }

    provider.stop().await.unwrap();
}
