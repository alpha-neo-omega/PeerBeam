//! The engine builder — dependency injection entry point.
//!
//! A frontend selects which plugins to compile/register (often behind
//! cargo features) and wires them here. `build` produces an immutable
//! [`Engine`]. This is the only place concrete providers are named.

use std::sync::Arc;

use peerbeam_app::ProviderRegistry;
use peerbeam_config::EngineConfig;
use peerbeam_domain::port::{
    ClipboardProvider, CompressionProvider, DiscoveryProvider, EncryptionProvider,
    NotificationSink, ReliabilityStore, RouteProvider, StorageProvider, TransferProvider,
    TrustStore,
};

use crate::engine::Engine;
use crate::error::EngineError;

/// Fluent builder that assembles the provider registry and configuration
/// into an [`Engine`].
pub struct EngineBuilder {
    config: EngineConfig,
    registry: ProviderRegistry,
}

impl EngineBuilder {
    /// Start a builder from the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            registry: ProviderRegistry::new(),
        }
    }

    /// Start a builder from default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EngineConfig::default())
    }

    // ── Provider registration (fluent) ──────────────────────────

    /// Add a discovery provider.
    pub fn with_discovery(mut self, provider: Arc<dyn DiscoveryProvider>) -> Self {
        self.registry.add_discovery(provider);
        self
    }

    /// Add a transfer provider.
    pub fn with_transfer(mut self, provider: Arc<dyn TransferProvider>) -> Self {
        self.registry.add_transfer(provider);
        self
    }

    /// Add a route provider.
    pub fn with_route(mut self, provider: Arc<dyn RouteProvider>) -> Self {
        self.registry.add_route(provider);
        self
    }

    /// Set the encryption provider.
    pub fn with_encryption(mut self, provider: Arc<dyn EncryptionProvider>) -> Self {
        self.registry.set_encryption(provider);
        self
    }

    /// Set the compression provider.
    pub fn with_compression(mut self, provider: Arc<dyn CompressionProvider>) -> Self {
        self.registry.set_compression(provider);
        self
    }

    /// Set the reliability store.
    pub fn with_reliability(mut self, provider: Arc<dyn ReliabilityStore>) -> Self {
        self.registry.set_reliability(provider);
        self
    }

    /// Set the storage provider.
    pub fn with_storage(mut self, provider: Arc<dyn StorageProvider>) -> Self {
        self.registry.set_storage(provider);
        self
    }

    /// Set the trust store.
    pub fn with_trust(mut self, provider: Arc<dyn TrustStore>) -> Self {
        self.registry.set_trust(provider);
        self
    }

    /// Set the notification sink.
    pub fn with_notification(mut self, provider: Arc<dyn NotificationSink>) -> Self {
        self.registry.set_notification(provider);
        self
    }

    /// Set the clipboard provider.
    pub fn with_clipboard(mut self, provider: Arc<dyn ClipboardProvider>) -> Self {
        self.registry.set_clipboard(provider);
        self
    }

    /// Finalize wiring and produce an [`Engine`].
    pub fn build(self) -> Result<Engine, EngineError> {
        Engine::new(self.config, self.registry)
    }
}
