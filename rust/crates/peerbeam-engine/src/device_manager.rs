//! Device manager — the UI's single source of truth for peers.
//!
//! Wraps the pure [`peerbeam_app::DeviceStore`] with the async plumbing that
//! drives it: it advertises + scans every registered discovery provider,
//! merges their tagged event streams, folds them through the store, and
//! notifies subscribers with [`DeviceChange`]s. Frontends query
//! [`snapshot`](DeviceManager::snapshot) and subscribe to
//! [`changes`](DeviceManager::changes) — they never touch discovery,
//! sockets, or providers directly.
//!
//! All the merge/dedup/online/latency/capability logic lives in the store;
//! this type only owns the runtime concerns (tasks, broadcast channel).

use std::sync::{Arc, Mutex};

use futures::stream::{self, BoxStream};
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use peerbeam_app::DeviceStore;
use peerbeam_domain::entity::{Device, ManagedDevice};
use peerbeam_domain::event::DeviceChange;
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryEvent, DiscoveryProvider};

use crate::error::EngineError;

/// Capacity of the device-change broadcast channel.
const CHANGE_CHANNEL_CAPACITY: usize = 256;

/// How long an offline device is kept around before it's pruned from the
/// store. Generous enough to survive a brief Wi-Fi drop / provider restart
/// without losing the entry, short enough that a long-running daemon (a
/// headless server) doesn't accumulate every device it has ever seen.
const PRUNE_TTL: chrono::Duration = chrono::Duration::minutes(5);

/// How often the prune sweep runs.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Tracks discovered devices and notifies the UI of changes.
pub struct DeviceManager {
    providers: Vec<Arc<dyn DiscoveryProvider>>,
    store: Arc<Mutex<DeviceStore>>,
    changes: broadcast::Sender<DeviceChange>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl DeviceManager {
    /// Create a manager over the given discovery providers, seeding the store
    /// with each provider's capabilities.
    pub fn new(providers: Vec<Arc<dyn DiscoveryProvider>>) -> Self {
        let caps = providers
            .iter()
            .map(|p| (p.id(), p.capabilities()))
            .collect();
        let (changes, _) = broadcast::channel(CHANGE_CHANNEL_CAPACITY);
        Self {
            providers,
            store: Arc::new(Mutex::new(DeviceStore::new(caps))),
            changes,
            task: Mutex::new(None),
        }
    }

    /// Advertise `me`, start scanning on every provider, and begin folding
    /// their merged events into the store. Idempotent: restarts the merge
    /// task if already running.
    pub async fn start(&self, me: Device) -> Result<(), EngineError> {
        for provider in &self.providers {
            provider.advertise(&me).await?;
            provider.scan().await?;
        }

        let mut stream = self.tagged_stream();
        let store = self.store.clone();
        let changes = self.changes.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    item = stream.next() => {
                        let Some((provider, event)) = item else { break };
                        let emitted = store.lock().unwrap().observe(&provider, event);
                        for change in emitted {
                            let _ = changes.send(change);
                        }
                    }
                    _ = ticker.tick() => {
                        prune_and_notify(&store, &changes, PRUNE_TTL);
                    }
                }
            }
        });

        if let Some(prev) = self.task.lock().unwrap().replace(handle) {
            prev.abort();
        }
        Ok(())
    }

    /// Stop the merge task and every provider, then mark all devices offline
    /// (with liveness no longer observed, "online" would be a stale claim).
    /// Devices stay tracked for re-discovery; subscribers get the offline
    /// changes so UIs don't show frozen presence.
    pub async fn stop(&self) -> Result<(), EngineError> {
        if let Some(handle) = self.task.lock().unwrap().take() {
            handle.abort();
        }
        for provider in &self.providers {
            provider.stop().await?;
        }
        let emitted = self.store.lock().unwrap().offline_all();
        for change in emitted {
            let _ = self.changes.send(change);
        }
        Ok(())
    }

    /// Current view of all tracked devices (online first, then by name).
    pub fn snapshot(&self) -> Vec<ManagedDevice> {
        self.store.lock().unwrap().snapshot()
    }

    /// Subscribe to device changes. Each subscriber sees every change
    /// emitted after it subscribes.
    pub fn changes(&self) -> broadcast::Receiver<DeviceChange> {
        self.changes.subscribe()
    }

    /// Record a measured latency for a device (fed by the networking layer),
    /// notifying subscribers if it changed.
    pub fn record_latency(&self, id: &DeviceId, latency_ms: Option<u32>) {
        let emitted = self.store.lock().unwrap().record_latency(id, latency_ms);
        for change in emitted {
            let _ = self.changes.send(change);
        }
    }

    /// Remove offline devices not seen within `ttl`, notifying subscribers.
    /// Called periodically by the task spawned in [`start`](Self::start) so
    /// stale entries don't accumulate unbounded on a long-running daemon; also
    /// exposed for manual/test invocation.
    pub fn prune(&self, ttl: chrono::Duration) {
        prune_and_notify(&self.store, &self.changes, ttl);
    }

    /// Combine every provider's event stream into one, tagged with the
    /// provider id so the store can attribute capabilities.
    fn tagged_stream(&self) -> BoxStream<'static, (ProviderId, DiscoveryEvent)> {
        let tagged: Vec<BoxStream<'static, (ProviderId, DiscoveryEvent)>> = self
            .providers
            .iter()
            .map(|provider| {
                let id = provider.id();
                provider.events().map(move |ev| (id.clone(), ev)).boxed()
            })
            .collect();
        stream::select_all(tagged).boxed()
    }
}

/// Prune stale offline devices from `store` and broadcast the resulting
/// `Removed` changes on `changes`. Shared by the periodic sweep in
/// [`DeviceManager::start`] and the manual [`DeviceManager::prune`].
fn prune_and_notify(
    store: &Mutex<DeviceStore>,
    changes: &broadcast::Sender<DeviceChange>,
    ttl: chrono::Duration,
) {
    let emitted = store.lock().unwrap().prune(chrono::Utc::now(), ttl);
    for change in emitted {
        let _ = changes.send(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    use async_trait::async_trait;
    use tokio::time::timeout;

    use peerbeam_domain::entity::{DeviceType, Platform};
    use peerbeam_domain::port::DiscoveryCaps;

    /// Provider that replays a fixed script and never advertises/scans again.
    struct Scripted {
        id: ProviderId,
        script: Vec<DiscoveryEvent>,
    }

    #[async_trait]
    impl DiscoveryProvider for Scripted {
        fn id(&self) -> ProviderId {
            self.id.clone()
        }
        fn capabilities(&self) -> DiscoveryCaps {
            DiscoveryCaps {
                can_advertise: true,
                can_scan: true,
                crosses_subnet: false,
                requires_tailscale: false,
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

    fn device(id: &str) -> Device {
        Device {
            id: DeviceId::from(id),
            name: id.to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: vec!["10.0.0.1".to_string()],
            port: 9000,
            last_seen: chrono::Utc::now(),
        }
    }

    /// `prune` (the manual/testable entry point that the periodic sweep in
    /// `start` also drives) removes an offline device once it is older than
    /// the TTL, and broadcasts the `Removed` change — closing the "offline
    /// devices accumulate unbounded" gap.
    #[tokio::test]
    async fn prune_removes_stale_offline_device_and_notifies() {
        let provider = Arc::new(Scripted {
            id: ProviderId::from("udp"),
            script: vec![
                DiscoveryEvent::Found(device("a")),
                DiscoveryEvent::Lost(DeviceId::from("a")),
            ],
        });
        let manager = DeviceManager::new(vec![provider]);
        let mut changes = manager.changes();
        manager.start(device("me")).await.expect("starts");

        // Drain Added + offline StatusChanged emitted by the scripted events.
        let _added = timeout(StdDuration::from_millis(500), changes.recv())
            .await
            .expect("added change")
            .unwrap();
        let offline = timeout(StdDuration::from_millis(500), changes.recv())
            .await
            .expect("status change")
            .unwrap();
        assert!(matches!(
            offline,
            DeviceChange::StatusChanged { online: false, .. }
        ));
        assert_eq!(manager.snapshot().len(), 1, "still tracked while offline");

        // Not yet stale under a generous TTL.
        manager.prune(chrono::Duration::hours(1));
        assert_eq!(manager.snapshot().len(), 1, "not stale, kept");

        // A zero TTL makes the offline device immediately stale.
        manager.prune(chrono::Duration::zero());
        let removed = timeout(StdDuration::from_millis(500), changes.recv())
            .await
            .expect("removed change")
            .unwrap();
        assert_eq!(removed, DeviceChange::Removed(DeviceId::from("a")));
        assert!(manager.snapshot().is_empty(), "pruned");

        manager.stop().await.expect("stops");
    }
}
