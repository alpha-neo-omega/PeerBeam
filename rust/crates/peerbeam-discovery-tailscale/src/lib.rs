//! Native Tailscale device discovery.
//!
//! An adapter implementing [`peerbeam_domain::port::DiscoveryProvider`] that
//! surfaces tailnet peers — reachable across subnets, NAT, and the internet
//! wherever Tailscale is up. This is the first provider with
//! `crosses_subnet = true`.
//!
//! # How it works
//!
//! Tailscale has no push API, so the provider polls `tailscale status`
//! (via [`source::StatusSource`]) on an interval and diffs successive
//! snapshots into Found/Updated/Lost events (see [`status`]). It obtains
//! status from either:
//!
//! - the **LocalAPI** Unix socket (`tailscaled.sock`) — no subprocess, or
//! - the **CLI** (`tailscale status --json`) — most portable.
//!
//! [`default_source`](source::default_source) prefers the socket when present.
//!
//! # What it surfaces
//!
//! - **Tailnet IPs** (100.64.0.0/10 and `fd7a:…`) as addresses.
//! - **MagicDNS** name as an additional address (reachable by name).
//! - Host name, OS→platform, stable node id (`ts:<id>`).
//!
//! # Contract notes
//!
//! - **Advertise is a no-op** (`can_advertise = false`): `tailscaled` already
//!   makes this node visible on the tailnet; PeerBeam adds nothing.
//! - **Self-filter** is inherent — `tailscale status` lists peers under
//!   `Peer`, never `Self`.
//! - Discovered ids are Tailscale-scoped (`ts:…`), so a device also found via
//!   mDNS/UDP appears as a separate entry until the transfer handshake
//!   reconciles the canonical app identity (future work).

mod source;
mod status;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;

use peerbeam_domain::entity::Device;
use peerbeam_domain::error::Result;
use peerbeam_domain::id::ProviderId;
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};

#[cfg(unix)]
pub use source::LocalApiStatusSource;
pub use source::{default_source, CliStatusSource, StatusSource};

use crate::status::{parse_status, SnapshotDiffer};

/// Capacity of the internal event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Tunable behaviour of [`TailscaleDiscovery`].
#[derive(Debug, Clone)]
pub struct Config {
    /// How often to poll `tailscale status`.
    pub poll_interval: Duration,
    /// Include peers Tailscale reports as offline.
    pub include_offline: bool,
    /// Transfer port stamped on discovered peers. `tailscale status` reports
    /// only tailnet IPs, not the app's port, so callers stamp their configured
    /// transfer port (peers use the same port by convention). 0 leaves peers
    /// portless — and therefore un-dialable — so set this.
    pub peer_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            include_offline: false,
            peer_port: 0,
        }
    }
}

struct Inner {
    source: Box<dyn StatusSource>,
    config: Config,
    events_tx: broadcast::Sender<DiscoveryEvent>,
    differ: Mutex<SnapshotDiffer>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    scanning: AtomicBool,
}

/// Tailscale discovery provider.
pub struct TailscaleDiscovery {
    inner: Arc<Inner>,
}

impl TailscaleDiscovery {
    /// Create a provider using the best available status source for this
    /// platform (LocalAPI socket if present, else the CLI).
    pub fn new(config: Config) -> Self {
        Self::with_source(default_source(), config)
    }

    /// Create a provider with an explicit status source. Used for tests
    /// (inject canned JSON) and custom deployments.
    pub fn with_source(source: Box<dyn StatusSource>, config: Config) -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                source,
                config,
                events_tx,
                differ: Mutex::new(SnapshotDiffer::new()),
                tasks: Mutex::new(Vec::new()),
                scanning: AtomicBool::new(false),
            }),
        }
    }
}

#[async_trait]
impl DiscoveryProvider for TailscaleDiscovery {
    fn id(&self) -> ProviderId {
        ProviderId::from("tailscale")
    }

    fn capabilities(&self) -> DiscoveryCaps {
        DiscoveryCaps {
            can_advertise: false, // tailscaled advertises the node itself
            can_scan: true,
            crosses_subnet: true,
            requires_tailscale: true,
        }
    }

    async fn advertise(&self, _me: &Device) -> Result<()> {
        // No-op: the node is already advertised on the tailnet by tailscaled.
        Ok(())
    }

    async fn scan(&self) -> Result<()> {
        if self.inner.scanning.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let handle = tokio::spawn(poll_loop(self.inner.clone()));
        self.inner.tasks.lock().unwrap().push(handle);
        tracing::info!(provider = "tailscale", "scanning started");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.inner.scanning.store(false, Ordering::SeqCst);
        for handle in self.inner.tasks.lock().unwrap().drain(..) {
            handle.abort();
        }
        let lost = self.inner.differ.lock().unwrap().drain_ids();
        for id in lost {
            let _ = self.inner.events_tx.send(DiscoveryEvent::Lost(id));
        }
        tracing::info!(provider = "tailscale", "discovery stopped");
        Ok(())
    }

    fn events(&self) -> BoxStream<'static, DiscoveryEvent> {
        BroadcastStream::new(self.inner.events_tx.subscribe())
            .filter_map(|res| async move { res.ok() })
            .boxed()
    }
}

/// Poll `tailscale status`, diff snapshots, emit events. Fetch/parse errors
/// are logged and retried on the next tick (e.g. Tailscale temporarily down).
async fn poll_loop(inner: Arc<Inner>) {
    let mut ticker = tokio::time::interval(inner.config.poll_interval);
    loop {
        ticker.tick().await; // first tick fires immediately
        if !inner.scanning.load(Ordering::SeqCst) {
            break;
        }

        let json = match inner.source.fetch().await {
            Ok(json) => json,
            Err(e) => {
                tracing::debug!("tailscale fetch failed: {e}");
                continue;
            }
        };

        let devices =
            match parse_status(&json, inner.config.include_offline, inner.config.peer_port) {
                Ok(devices) => devices,
                Err(e) => {
                    tracing::debug!("tailscale parse failed: {e}");
                    continue;
                }
            };

        let events = inner.differ.lock().unwrap().diff(devices);
        for event in events {
            let _ = inner.events_tx.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptySource;

    #[async_trait]
    impl StatusSource for EmptySource {
        async fn fetch(&self) -> Result<String> {
            Ok(r#"{"Peer":{}}"#.to_string())
        }
    }

    #[test]
    fn capabilities_declare_cross_subnet_scan_only() {
        let provider = TailscaleDiscovery::with_source(Box::new(EmptySource), Config::default());
        let caps = provider.capabilities();
        assert!(!caps.can_advertise, "advertise handled by tailscaled");
        assert!(caps.can_scan);
        assert!(caps.crosses_subnet);
        assert!(caps.requires_tailscale);
        assert_eq!(provider.id(), ProviderId::from("tailscale"));
    }

    #[tokio::test]
    async fn advertise_is_noop() {
        let provider = TailscaleDiscovery::with_source(Box::new(EmptySource), Config::default());
        let me = Device {
            id: peerbeam_domain::id::DeviceId::from("me"),
            name: "Me".to_string(),
            device_type: peerbeam_domain::entity::DeviceType::Desktop,
            platform: peerbeam_domain::entity::Platform::Linux,
            addresses: vec![],
            port: 0,
            last_seen: chrono::Utc::now(),
        };
        assert!(provider.advertise(&me).await.is_ok());
    }
}
