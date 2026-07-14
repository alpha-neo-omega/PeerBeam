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
            while let Some((provider, event)) = stream.next().await {
                let emitted = store.lock().unwrap().observe(&provider, event);
                for change in emitted {
                    let _ = changes.send(change);
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
