//! Discovery port: how peers become visible.
//!
//! Every discovery mechanism — mDNS, UDP broadcast, Tailscale status,
//! Bluetooth, manual entry, a future relay — implements the single
//! [`DiscoveryProvider`] trait. The engine registers many providers at once,
//! runs each independently, and merges their [`DiscoveryEvent`]s into one
//! deduplicated device list. Frontends never learn which mechanism found a
//! device.
//!
//! # Contract
//!
//! An implementation MUST uphold:
//!
//! 1. **Self-filtering.** Never emit a [`DiscoveryEvent`] for this device.
//!    Filtering by our own [`DeviceId`] is the provider's job, not the
//!    engine's. (v1 leaked self-discovery; v2 forbids it at the contract.)
//! 2. **Idempotent lifecycle.** Calling [`advertise`](DiscoveryProvider::advertise),
//!    [`scan`](DiscoveryProvider::scan), or [`stop`](DiscoveryProvider::stop)
//!    when already in that state is a no-op returning `Ok`.
//! 3. **Capability honesty.** [`capabilities`](DiscoveryProvider::capabilities)
//!    must accurately describe reach; the engine uses it to skip providers
//!    that cannot help on the current network (e.g. Tailscale down).
//! 4. **`stop` halts everything** — both advertising and scanning — and
//!    ends the stream(s) returned by [`events`](DiscoveryProvider::events).
//! 5. **Event ordering per peer.** For a given [`DeviceId`], a `Found` (or
//!    `Updated`) precedes its `Lost`. Cross-peer ordering is unspecified.
//! 6. **Dedup is the engine's job.** A provider may report the same peer
//!    more than once (e.g. re-announcements); it should prefer `Updated`
//!    over repeated `Found`, but the engine tolerates either.
//!
//! The engine subscribes via [`events`](DiscoveryProvider::events) *before*
//! calling [`scan`](DiscoveryProvider::scan), so no early events are lost.

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::entity::Device;
use crate::error::Result;
use crate::id::{DeviceId, ProviderId};

/// An event emitted by a single discovery provider.
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryEvent {
    /// A peer became visible for the first time.
    Found(Device),
    /// A peer already known to this provider changed (address, name, …).
    Updated(Device),
    /// A peer is no longer visible via this provider.
    Lost(DeviceId),
}

impl DiscoveryEvent {
    /// The id of the peer this event concerns, regardless of variant.
    pub fn device_id(&self) -> &DeviceId {
        match self {
            DiscoveryEvent::Found(d) | DiscoveryEvent::Updated(d) => &d.id,
            DiscoveryEvent::Lost(id) => id,
        }
    }
}

/// Declares what a discovery provider can do and reach, so the engine can
/// select providers appropriate to the current environment without any user
/// configuration.
///
/// All fields default to `false`; a provider sets the ones that apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryCaps {
    /// The provider can announce this device to peers.
    ///
    /// Some providers are scan-only (e.g. reading `tailscale status`) and
    /// leave this `false`.
    pub can_advertise: bool,

    /// The provider can find peers.
    ///
    /// Some providers are advertise-only; they leave this `false`.
    pub can_scan: bool,

    /// The provider can find peers beyond the local broadcast subnet
    /// (e.g. across Tailscale, a VPN, or the internet).
    pub crosses_subnet: bool,

    /// The provider only functions when Tailscale is up; the engine skips
    /// it otherwise instead of surfacing errors.
    pub requires_tailscale: bool,
}

/// A device-discovery mechanism.
///
/// Object-safe: the engine holds providers as `Arc<dyn DiscoveryProvider>`.
/// See the [module contract](self) for the invariants every implementation
/// must uphold.
#[async_trait]
pub trait DiscoveryProvider: Send + Sync {
    /// Stable, unique id of this provider instance (used in logs and to
    /// attribute merged events).
    fn id(&self) -> ProviderId;

    /// What this provider can do and reach. Must be accurate — the engine
    /// relies on it to choose providers.
    fn capabilities(&self) -> DiscoveryCaps;

    /// Begin advertising `me` so peers can discover this device.
    ///
    /// No-op returning `Ok` if already advertising, or if the provider
    /// cannot advertise (see [`DiscoveryCaps::can_advertise`]).
    async fn advertise(&self, me: &Device) -> Result<()>;

    /// Begin scanning for peers. Events surface via [`events`](Self::events).
    ///
    /// No-op returning `Ok` if already scanning, or if the provider cannot
    /// scan (see [`DiscoveryCaps::can_scan`]).
    async fn scan(&self) -> Result<()>;

    /// Stop both advertising and scanning and end the event stream(s).
    ///
    /// No-op returning `Ok` if already stopped.
    async fn stop(&self) -> Result<()>;

    /// Subscribe to this provider's discovery events.
    ///
    /// Each call returns an independent stream. The stream ends when
    /// [`stop`](Self::stop) is called or the provider is dropped.
    fn events(&self) -> BoxStream<'static, DiscoveryEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Device, DeviceType, Platform};
    use chrono::Utc;
    use futures::executor::block_on;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A minimal in-memory test double. NOT a real provider — it exists only
    /// to prove the interface is object-safe and drivable, and to pin the
    /// contract's shape. Real mechanisms live in separate infra crates.
    struct FakeDiscovery {
        id: ProviderId,
        caps: DiscoveryCaps,
        scripted: Vec<DiscoveryEvent>,
        advertising: AtomicBool,
        scanning: AtomicBool,
        stopped: AtomicBool,
    }

    impl FakeDiscovery {
        fn new(scripted: Vec<DiscoveryEvent>) -> Self {
            Self {
                id: ProviderId::from("fake"),
                caps: DiscoveryCaps {
                    can_advertise: true,
                    can_scan: true,
                    crosses_subnet: false,
                    requires_tailscale: false,
                },
                scripted,
                advertising: AtomicBool::new(false),
                scanning: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl DiscoveryProvider for FakeDiscovery {
        fn id(&self) -> ProviderId {
            self.id.clone()
        }

        fn capabilities(&self) -> DiscoveryCaps {
            self.caps
        }

        async fn advertise(&self, _me: &Device) -> Result<()> {
            self.advertising.store(true, Ordering::Relaxed);
            Ok(())
        }

        async fn scan(&self) -> Result<()> {
            self.scanning.store(true, Ordering::Relaxed);
            Ok(())
        }

        async fn stop(&self) -> Result<()> {
            self.advertising.store(false, Ordering::Relaxed);
            self.scanning.store(false, Ordering::Relaxed);
            self.stopped.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn events(&self) -> BoxStream<'static, DiscoveryEvent> {
            futures::stream::iter(self.scripted.clone()).boxed()
        }
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: DeviceId::from(id),
            name: name.to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: vec!["10.0.0.2".to_string()],
            port: 0,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn is_object_safe() {
        // Must be storable as a trait object — the engine holds Arc<dyn _>.
        let provider: Arc<dyn DiscoveryProvider> = Arc::new(FakeDiscovery::new(vec![]));
        assert_eq!(provider.id(), ProviderId::from("fake"));
    }

    #[test]
    fn reports_capabilities() {
        let provider = FakeDiscovery::new(vec![]);
        let caps = provider.capabilities();
        assert!(caps.can_advertise);
        assert!(caps.can_scan);
        assert!(!caps.crosses_subnet);
        assert!(!caps.requires_tailscale);
    }

    #[test]
    fn caps_default_is_all_false() {
        let caps = DiscoveryCaps::default();
        assert_eq!(
            caps,
            DiscoveryCaps {
                can_advertise: false,
                can_scan: false,
                crosses_subnet: false,
                requires_tailscale: false
            }
        );
    }

    #[test]
    fn lifecycle_transitions() {
        let provider = FakeDiscovery::new(vec![]);
        let me = device("self", "Me");

        block_on(async {
            provider.advertise(&me).await.unwrap();
            provider.scan().await.unwrap();
        });
        assert!(provider.advertising.load(Ordering::Relaxed));
        assert!(provider.scanning.load(Ordering::Relaxed));

        block_on(provider.stop()).unwrap();
        assert!(provider.stopped.load(Ordering::Relaxed));
        assert!(!provider.advertising.load(Ordering::Relaxed));
        assert!(!provider.scanning.load(Ordering::Relaxed));
    }

    #[test]
    fn event_stream_yields_scripted_events_in_order() {
        let events = vec![
            DiscoveryEvent::Found(device("a", "Alice")),
            DiscoveryEvent::Updated(device("a", "Alice-2")),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        ];
        let provider = FakeDiscovery::new(events.clone());

        let collected: Vec<DiscoveryEvent> = block_on(provider.events().collect());
        assert_eq!(collected, events);
    }

    #[test]
    fn events_returns_independent_streams() {
        let provider = FakeDiscovery::new(vec![DiscoveryEvent::Lost(DeviceId::from("x"))]);
        let a: Vec<_> = block_on(provider.events().collect());
        let b: Vec<_> = block_on(provider.events().collect());
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn device_id_accessor_covers_all_variants() {
        assert_eq!(
            DiscoveryEvent::Found(device("a", "A")).device_id(),
            &DeviceId::from("a")
        );
        assert_eq!(
            DiscoveryEvent::Updated(device("b", "B")).device_id(),
            &DeviceId::from("b")
        );
        assert_eq!(
            DiscoveryEvent::Lost(DeviceId::from("c")).device_id(),
            &DeviceId::from("c")
        );
    }
}
