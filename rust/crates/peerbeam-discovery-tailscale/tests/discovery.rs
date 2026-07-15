//! Integration test for the Tailscale provider driven by a scripted status
//! source — no Tailscale install required. Verifies the poll → diff → event
//! pipeline emits Found for peers and Lost when a peer leaves a later snapshot.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::time::timeout;

use peerbeam_discovery_tailscale::{Config, StatusSource, TailscaleDiscovery};
use peerbeam_domain::error::Result;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{DiscoveryEvent, DiscoveryProvider};

/// Returns a sequence of status snapshots, then repeats the last forever.
struct ScriptedSource {
    snapshots: Vec<String>,
    idx: AtomicUsize,
}

#[async_trait]
impl StatusSource for ScriptedSource {
    async fn fetch(&self) -> Result<String> {
        let i = self.idx.fetch_add(1, Ordering::SeqCst);
        let i = i.min(self.snapshots.len() - 1);
        Ok(self.snapshots[i].clone())
    }
}

fn status_with(peers: &[(&str, &str, &str)]) -> String {
    // peers: (id, hostname, tailnet_ip)
    let entries: Vec<String> = peers
        .iter()
        .map(|(id, host, ip)| {
            format!(
                r#""nodekey:{id}": {{ "ID": "{id}", "HostName": "{host}",
                   "DNSName": "{host}.tail1234.ts.net.", "OS": "linux",
                   "TailscaleIPs": ["{ip}"], "Online": true }}"#
            )
        })
        .collect();
    format!(r#"{{ "Peer": {{ {} }} }}"#, entries.join(","))
}

#[tokio::test]
async fn discovers_then_loses_tailnet_peers() {
    let snapshots = vec![
        status_with(&[("n1", "alice", "100.64.0.1"), ("n2", "bob", "100.64.0.2")]),
        status_with(&[("n1", "alice", "100.64.0.1")]), // bob left
    ];
    let source = Box::new(ScriptedSource {
        snapshots,
        idx: AtomicUsize::new(0),
    });

    let provider = TailscaleDiscovery::with_source(
        source,
        Config {
            poll_interval: Duration::from_millis(80),
            include_offline: false,
            peer_port: 4200,
        },
    );

    let mut events = provider.events();
    provider.scan().await.unwrap();

    // Collect a handful of events over a couple of poll cycles.
    let mut founds = Vec::new();
    let mut losts = Vec::new();
    let deadline = Duration::from_secs(2);
    while founds.len() < 2 || losts.is_empty() {
        match timeout(deadline, events.next()).await {
            Ok(Some(DiscoveryEvent::Found(d))) => founds.push(d),
            Ok(Some(DiscoveryEvent::Lost(id))) => losts.push(id),
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Discovered peers must carry the configured transfer port + an address,
    // or a send can't dial them ("not reachable"). Regression: peer_port used
    // to default to 0, leaving Tailscale peers un-dialable.
    for d in &founds {
        assert_eq!(d.port, 4200, "peer {} must carry the stamped port", d.id.0);
        assert!(
            !d.addresses.is_empty(),
            "peer {} must have an address",
            d.id.0
        );
    }
    let founds: Vec<DeviceId> = founds.into_iter().map(|d| d.id).collect();

    assert!(
        founds.contains(&DeviceId::from("ts:n1")),
        "found {founds:?}"
    );
    assert!(
        founds.contains(&DeviceId::from("ts:n2")),
        "found {founds:?}"
    );
    assert!(
        losts.contains(&DeviceId::from("ts:n2")),
        "bob should be lost, got {losts:?}"
    );

    provider.stop().await.unwrap();
}
