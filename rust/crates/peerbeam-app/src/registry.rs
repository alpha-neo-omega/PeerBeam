//! The dependency-injection container.
//!
//! Concrete adapters are registered here as trait objects. The engine's
//! builder populates a registry; use-cases read providers back out through
//! the port traits, oblivious to which crate implemented them.
//!
//! Discovery, transfer, and route are *multi*: several providers can be
//! active at once and the engine merges/selects across them. The remaining
//! ports are singletons.

use std::sync::Arc;

use peerbeam_domain::port::{
    ClipboardProvider, CompressionProvider, DiscoveryProvider, EncryptionProvider,
    NotificationSink, ReliabilityStore, RouteProvider, StorageProvider, TransferProvider,
    TrustStore,
};

/// Immutable collection of every provider wired into the engine.
///
/// Built once (via the engine builder) and shared behind an `Arc`. Cloning
/// is cheap: every field is `Arc`-backed.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    discovery: Vec<Arc<dyn DiscoveryProvider>>,
    transfer: Vec<Arc<dyn TransferProvider>>,
    route: Vec<Arc<dyn RouteProvider>>,
    encryption: Option<Arc<dyn EncryptionProvider>>,
    compression: Option<Arc<dyn CompressionProvider>>,
    reliability: Option<Arc<dyn ReliabilityStore>>,
    storage: Option<Arc<dyn StorageProvider>>,
    trust: Option<Arc<dyn TrustStore>>,
    notification: Option<Arc<dyn NotificationSink>>,
    clipboard: Option<Arc<dyn ClipboardProvider>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    // ── Registration (multi) ────────────────────────────────────

    /// Register a discovery provider.
    pub fn add_discovery(&mut self, provider: Arc<dyn DiscoveryProvider>) -> &mut Self {
        self.discovery.push(provider);
        self
    }

    /// Register a transfer provider.
    pub fn add_transfer(&mut self, provider: Arc<dyn TransferProvider>) -> &mut Self {
        self.transfer.push(provider);
        self
    }

    /// Register a route provider.
    pub fn add_route(&mut self, provider: Arc<dyn RouteProvider>) -> &mut Self {
        self.route.push(provider);
        self
    }

    // ── Registration (singletons) ───────────────────────────────

    /// Set the encryption provider.
    pub fn set_encryption(&mut self, provider: Arc<dyn EncryptionProvider>) -> &mut Self {
        self.encryption = Some(provider);
        self
    }

    /// Set the compression provider.
    pub fn set_compression(&mut self, provider: Arc<dyn CompressionProvider>) -> &mut Self {
        self.compression = Some(provider);
        self
    }

    /// Set the reliability store.
    pub fn set_reliability(&mut self, provider: Arc<dyn ReliabilityStore>) -> &mut Self {
        self.reliability = Some(provider);
        self
    }

    /// Set the storage provider.
    pub fn set_storage(&mut self, provider: Arc<dyn StorageProvider>) -> &mut Self {
        self.storage = Some(provider);
        self
    }

    /// Set the trust store.
    pub fn set_trust(&mut self, provider: Arc<dyn TrustStore>) -> &mut Self {
        self.trust = Some(provider);
        self
    }

    /// Set the notification sink.
    pub fn set_notification(&mut self, provider: Arc<dyn NotificationSink>) -> &mut Self {
        self.notification = Some(provider);
        self
    }

    /// Set the clipboard provider.
    pub fn set_clipboard(&mut self, provider: Arc<dyn ClipboardProvider>) -> &mut Self {
        self.clipboard = Some(provider);
        self
    }

    // ── Resolution ──────────────────────────────────────────────

    /// All registered discovery providers.
    pub fn discovery(&self) -> &[Arc<dyn DiscoveryProvider>] {
        &self.discovery
    }

    /// All registered transfer providers.
    pub fn transfer(&self) -> &[Arc<dyn TransferProvider>] {
        &self.transfer
    }

    /// All registered route providers.
    pub fn route(&self) -> &[Arc<dyn RouteProvider>] {
        &self.route
    }

    /// The encryption provider, if one is registered.
    pub fn encryption(&self) -> Option<&Arc<dyn EncryptionProvider>> {
        self.encryption.as_ref()
    }

    /// The compression provider, if one is registered.
    pub fn compression(&self) -> Option<&Arc<dyn CompressionProvider>> {
        self.compression.as_ref()
    }

    /// The reliability store, if one is registered.
    pub fn reliability(&self) -> Option<&Arc<dyn ReliabilityStore>> {
        self.reliability.as_ref()
    }

    /// The storage provider, if one is registered.
    pub fn storage(&self) -> Option<&Arc<dyn StorageProvider>> {
        self.storage.as_ref()
    }

    /// The trust store, if one is registered.
    pub fn trust(&self) -> Option<&Arc<dyn TrustStore>> {
        self.trust.as_ref()
    }

    /// The notification sink, if one is registered.
    pub fn notification(&self) -> Option<&Arc<dyn NotificationSink>> {
        self.notification.as_ref()
    }

    /// The clipboard provider, if one is registered.
    pub fn clipboard(&self) -> Option<&Arc<dyn ClipboardProvider>> {
        self.clipboard.as_ref()
    }
}
