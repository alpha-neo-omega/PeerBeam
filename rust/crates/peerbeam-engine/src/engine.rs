//! The engine handle exposed to frontends.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use peerbeam_app::{merge_discovery, ProviderRegistry};
use peerbeam_config::EngineConfig;
use peerbeam_domain::entity::Device;
use peerbeam_domain::event::DomainEvent;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::error::EngineError;

/// Capacity of the outbound event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// The single handle every frontend holds.
///
/// Owns the resolved provider registry, the active configuration, and the
/// outbound event channel. Cloning shares the same underlying engine
/// (registry is `Arc`, the broadcast sender is cheaply cloneable), so a
/// frontend can hand copies to multiple tasks.
#[derive(Clone)]
pub struct Engine {
    config: Arc<EngineConfig>,
    registry: Arc<ProviderRegistry>,
    events: broadcast::Sender<DomainEvent>,
    /// Handle to the running discovery-merge task, if discovery is active.
    discovery_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Engine {
    /// Assemble the engine from configuration and a populated registry.
    ///
    /// Called by [`crate::EngineBuilder::build`]. Kept crate-private so the
    /// builder is the only construction path.
    pub(crate) fn new(
        config: EngineConfig,
        registry: ProviderRegistry,
    ) -> Result<Self, EngineError> {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        tracing::info!(
            device = %config.device.name,
            discovery = registry.discovery().len(),
            transfer = registry.transfer().len(),
            "engine assembled"
        );
        Ok(Self {
            config: Arc::new(config),
            registry: Arc::new(registry),
            events,
            discovery_task: Arc::new(Mutex::new(None)),
        })
    }

    /// The active configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The resolved provider registry, for use-cases to read ports from.
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// Subscribe to the engine's event stream. Frontends render these
    /// events; each subscriber receives every event published after it
    /// subscribes.
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.events.subscribe()
    }

    /// Publish an event to all subscribers.
    ///
    /// Returns the number of subscribers that received it (0 if none are
    /// listening). Use-cases call this as work progresses.
    pub fn publish(&self, event: DomainEvent) -> usize {
        self.events.send(event).unwrap_or(0)
    }

    /// Start discovery across every registered [`DiscoveryProvider`].
    ///
    /// Advertises `me` and begins scanning on each provider, then merges all
    /// their event streams into one deduplicated stream (see
    /// [`peerbeam_app::merge_discovery`]) and republishes the result on the
    /// engine event channel. Idempotent: calling it while discovery is
    /// already running restarts the merge task.
    pub async fn start_discovery(&self, me: Device) -> Result<(), EngineError> {
        let providers = self.registry.discovery();
        for provider in providers {
            provider.advertise(&me).await?;
            provider.scan().await?;
        }

        let mut merged = merge_discovery(providers);
        let engine = self.clone();
        let handle = tokio::spawn(async move {
            while let Some(event) = merged.next().await {
                engine.publish(event);
            }
        });

        // Replace any prior task, aborting it first.
        if let Some(prev) = self.discovery_task.lock().unwrap().replace(handle) {
            prev.abort();
        }
        tracing::info!(providers = providers.len(), "discovery started");
        Ok(())
    }

    /// Stop discovery: halt the merge task and every provider.
    pub async fn stop_discovery(&self) -> Result<(), EngineError> {
        if let Some(handle) = self.discovery_task.lock().unwrap().take() {
            handle.abort();
        }
        for provider in self.registry.discovery() {
            provider.stop().await?;
        }
        tracing::info!("discovery stopped");
        Ok(())
    }
}
