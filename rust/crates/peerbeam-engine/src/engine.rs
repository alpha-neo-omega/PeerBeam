//! The engine handle exposed to frontends.

use std::sync::Arc;

use peerbeam_app::ProviderRegistry;
use peerbeam_config::EngineConfig;
use peerbeam_domain::entity::{Device, ManagedDevice};
use peerbeam_domain::event::{DeviceChange, DomainEvent};
use peerbeam_domain::id::DeviceId;
use tokio::sync::broadcast;

use crate::device_manager::DeviceManager;
use crate::error::EngineError;

/// Capacity of the outbound event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// The single handle every frontend holds.
///
/// Owns the resolved provider registry, configuration, the general event
/// channel, and the [`DeviceManager`]. Cloning shares the same underlying
/// engine (all fields are `Arc`/cloneable), so a frontend can hand copies to
/// multiple tasks.
#[derive(Clone)]
pub struct Engine {
    config: Arc<EngineConfig>,
    registry: Arc<ProviderRegistry>,
    events: broadcast::Sender<DomainEvent>,
    devices: Arc<DeviceManager>,
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
        let devices = Arc::new(DeviceManager::new(registry.discovery().to_vec()));
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
            devices,
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

    /// Subscribe to the engine's general event stream (transfers, clipboard,
    /// errors). Device changes have their own stream — see
    /// [`device_changes`](Self::device_changes).
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

    // ── Devices ─────────────────────────────────────────────────

    /// Start discovery: advertise `me` and scan on every registered provider,
    /// merging results into the device manager.
    pub async fn start_discovery(&self, me: Device) -> Result<(), EngineError> {
        self.devices.start(me).await
    }

    /// Stop discovery on every provider.
    pub async fn stop_discovery(&self) -> Result<(), EngineError> {
        self.devices.stop().await
    }

    /// Current merged, deduplicated device list (online first).
    pub fn devices(&self) -> Vec<ManagedDevice> {
        self.devices.snapshot()
    }

    /// Subscribe to device changes for the UI to render.
    pub fn device_changes(&self) -> broadcast::Receiver<DeviceChange> {
        self.devices.changes()
    }

    /// Record a measured latency for a device (from the networking layer).
    pub fn record_device_latency(&self, id: &DeviceId, latency_ms: Option<u32>) {
        self.devices.record_latency(id, latency_ms);
    }
}
