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
/// Answers whether a device must survive pruning however long it has been gone.
///
/// The store knows liveness and nothing else; whether a device is *yours*, or
/// has a hardware address recorded so it can be woken, lives in the trust and
/// wake stores. So the question is injected rather than answered here.
pub type KeepDevice = Arc<dyn Fn(&DeviceId) -> bool + Send + Sync>;

pub struct DeviceManager {
    providers: Vec<Arc<dyn DiscoveryProvider>>,
    store: Arc<Mutex<DeviceStore>>,
    changes: broadcast::Sender<DeviceChange>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Devices this answers `true` for are never pruned. See [`KeepDevice`].
    keep: Mutex<Option<KeepDevice>>,
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
            keep: Mutex::new(None),
        }
    }

    /// Protect devices from pruning — those marked as the user's own, and those
    /// with a wake address recorded.
    ///
    /// **Waking a device requires it to still be listed, and a sleeping device
    /// is an offline one.** Pruning after five minutes is what stops the list
    /// filling with machines that are long gone, but applied to these it deletes
    /// the very devices Wake exists for: recording a MAC is a deliberate
    /// statement that you intend to wake that machine, and marking one as yours
    /// is the same claim in weaker form. Forgetting either is the app discarding
    /// something it was told.
    pub fn keep_devices(&self, keep: KeepDevice) {
        *self.keep.lock().unwrap() = Some(keep);
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
        let keep = self.keep.lock().unwrap().clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // **The sweep outlives the stream.** These share one task, and the
            // stream ending used to `break` out of the loop — taking the ticker
            // with it. Every device already in the store then stayed exactly as
            // it was, offline and un-prunable, for as long as the app ran: a
            // list slowly filling with machines that are no longer there and
            // will never age out. The stream ends whenever the providers stop
            // yielding, which needs no user action and says nothing on screen.
            //
            // So a finished stream retires that arm and leaves the sweep
            // running. The task still ends when it is aborted — by `stop`, or
            // by a `start` replacing it — which is the only place ending it is
            // actually meant to happen.
            let mut live = true;
            loop {
                tokio::select! {
                    item = stream.next(), if live => {
                        match item {
                            Some((provider, event)) => {
                                let emitted = store.lock().unwrap().observe(&provider, event);
                                for change in emitted {
                                    let _ = changes.send(change);
                                }
                            }
                            None => live = false,
                        }
                    }
                    _ = ticker.tick() => {
                        prune_and_notify(&store, &changes, PRUNE_TTL, keep.as_ref());
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
        let keep = self.keep.lock().unwrap().clone();
        prune_and_notify(&self.store, &self.changes, ttl, keep.as_ref());
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
    keep: Option<&KeepDevice>,
) {
    let emitted = store
        .lock()
        .unwrap()
        .prune(chrono::Utc::now(), ttl, |id| keep.is_some_and(|k| k(id)));
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

    /// **The periodic sweep must outlive the provider stream.** Both run in one
    /// task, and the stream finishing used to break the loop — taking the ticker
    /// down with it. Nothing announces that: the app's scan button is its own
    /// state, so it still reads "Stop" while the engine has quietly stopped
    /// pruning, and every device already seen stays listed, offline, for as long
    /// as the app runs.
    ///
    /// `Scripted` ends as soon as its script is replayed, which is exactly that
    /// condition. Asserted on the task rather than on a removal, because the
    /// sweep's own interval is a minute and its staleness test reads the wall
    /// clock — what regressed is whether the task is still there to tick at all.
    #[tokio::test]
    async fn the_prune_sweep_survives_a_finished_provider_stream() {
        let provider = Arc::new(Scripted {
            id: ProviderId::from("udp"),
            script: vec![DiscoveryEvent::Found(device("a"))],
        });
        let manager = DeviceManager::new(vec![provider]);
        let mut changes = manager.changes();
        manager.start(device("me")).await.expect("starts");

        // Wait for the one scripted event, after which the stream is finished.
        let _added = timeout(StdDuration::from_millis(500), changes.recv())
            .await
            .expect("the scripted event arrives")
            .expect("a change");
        // Give the task a chance to observe the end of the stream.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let task = manager.task.lock().unwrap();
        let handle = task.as_ref().expect("the merge task is running");
        assert!(
            !handle.is_finished(),
            "the task exited when the provider stream ended, so nothing prunes \
             offline devices any more"
        );
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
