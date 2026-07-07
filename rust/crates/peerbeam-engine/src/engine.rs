//! The engine handle exposed to frontends.

use std::sync::Arc;

use peerbeam_app::ProviderRegistry;
use peerbeam_config::EngineConfig;
use peerbeam_domain::event::DomainEvent;
use tokio::sync::broadcast;

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
}
