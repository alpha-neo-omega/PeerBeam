//! Cross-provider discovery merge.
//!
//! Multiple [`DiscoveryProvider`]s run at once — UDP broadcast, mDNS,
//! Tailscale, … — and the same physical device is often seen by more than
//! one. This module fuses their per-provider [`DiscoveryEvent`]s into one
//! deduplicated stream of [`DomainEvent`]s so the UI shows a single entry
//! per device and never learns which mechanism found it.
//!
//! Two pieces:
//!
//! - [`DiscoveryRegistry`] — the pure reducer. Tracks which providers
//!   currently see each device and unions their addresses. Fully
//!   unit-testable with no IO.
//! - [`merge_discovery`] — wraps N providers' event streams and drives the
//!   reducer, yielding a single `BoxStream<DomainEvent>`. Runtime-agnostic
//!   (built on `futures`, no tokio).
//!
//! ## Dedup rules
//!
//! - First sighting of a device (from any provider) → [`DomainEvent::PeerFound`].
//! - A later sighting that adds information (e.g. a second provider
//!   contributes a Tailscale address) → [`DomainEvent::PeerUpdated`] with the
//!   unioned addresses.
//! - A redundant sighting that adds nothing → no event (no UI churn).
//! - [`DiscoveryEvent::Lost`] from one provider removes only that provider's
//!   claim; [`DomainEvent::PeerLost`] fires only when the *last* provider
//!   that saw the device drops it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::stream::{self, BoxStream};
use futures::StreamExt;

use peerbeam_domain::entity::Device;
use peerbeam_domain::event::DomainEvent;
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryEvent, DiscoveryProvider};

/// What we know about one merged device across all providers.
struct Record {
    device: Device,
    /// Providers that currently report this device.
    providers: HashSet<ProviderId>,
    /// Provider that owns the identity fields; only it may change name/type/
    /// platform/port. Prevents cross-provider identity flapping while still
    /// surfacing a real rename. Transfers when the owner drops.
    identity_provider: ProviderId,
}

/// The pure cross-provider dedup reducer.
#[derive(Default)]
pub struct DiscoveryRegistry {
    devices: HashMap<DeviceId, Record>,
}

impl DiscoveryRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one provider event into the merged view, returning the merged
    /// [`DomainEvent`] to emit, or `None` when nothing user-visible changed.
    pub fn observe(&mut self, provider: &ProviderId, event: DiscoveryEvent) -> Option<DomainEvent> {
        match event {
            DiscoveryEvent::Found(device) | DiscoveryEvent::Updated(device) => {
                let id = device.id.clone();
                match self.devices.get_mut(&id) {
                    None => {
                        self.devices.insert(
                            id,
                            Record {
                                device: device.clone(),
                                providers: HashSet::from([provider.clone()]),
                                identity_provider: provider.clone(),
                            },
                        );
                        Some(DomainEvent::PeerFound(device))
                    }
                    Some(record) => {
                        record.providers.insert(provider.clone());
                        let owner_present = record.providers.contains(&record.identity_provider);
                        let take_identity = *provider == record.identity_provider || !owner_present;
                        if take_identity {
                            record.identity_provider = provider.clone();
                        }
                        let merged = merge_devices(&record.device, &device, take_identity);
                        let changed = !record.device.same_identity(&merged);
                        record.device = merged.clone();
                        changed.then_some(DomainEvent::PeerUpdated(merged))
                    }
                }
            }
            DiscoveryEvent::Lost(id) => {
                let remove = match self.devices.get_mut(&id) {
                    Some(record) => {
                        record.providers.remove(provider);
                        record.providers.is_empty()
                    }
                    None => false,
                };
                if remove {
                    self.devices.remove(&id);
                    Some(DomainEvent::PeerLost(id))
                } else {
                    None
                }
            }
        }
    }

    /// Number of currently-merged devices.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether no devices are currently merged.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

/// Merge a re-sighting into an already-known device, always unioning addresses
/// and freshening `last_seen`. When `take_identity` is true (the identity owner,
/// or a takeover after the owner dropped) the incoming name/type/platform/port
/// is adopted, so a genuine rename propagates; otherwise the established
/// identity is kept and only blank/zero fields are filled, so alternating
/// providers can't flap it. Mirrors [`crate::device_store`]'s merge.
fn merge_devices(existing: &Device, incoming: &Device, take_identity: bool) -> Device {
    let mut addresses = existing.addresses.clone();
    for addr in &incoming.addresses {
        if !addresses.contains(addr) {
            addresses.push(addr.clone());
        }
    }
    if take_identity {
        return Device {
            addresses,
            ..incoming.clone()
        };
    }
    Device {
        id: existing.id.clone(),
        name: if existing.name.is_empty() {
            incoming.name.clone()
        } else {
            existing.name.clone()
        },
        device_type: existing.device_type,
        platform: existing.platform,
        port: if existing.port == 0 {
            incoming.port
        } else {
            existing.port
        },
        addresses,
        last_seen: incoming.last_seen,
    }
}

/// Merge the event streams of every provider into one deduplicated
/// [`DomainEvent`] stream.
///
/// Each provider's `events()` is tagged with its id and combined with
/// [`stream::select_all`]; a [`DiscoveryRegistry`] carried through
/// [`StreamExt::scan`] performs the dedup. The returned stream is `'static`
/// and owns everything it needs (the input slice is only borrowed while the
/// per-provider streams are taken).
pub fn merge_discovery(
    providers: &[Arc<dyn DiscoveryProvider>],
) -> BoxStream<'static, DomainEvent> {
    let tagged: Vec<BoxStream<'static, (ProviderId, DiscoveryEvent)>> = providers
        .iter()
        .map(|provider| {
            let id = provider.id();
            provider
                .events()
                .map(move |event| (id.clone(), event))
                .boxed()
        })
        .collect();

    stream::select_all(tagged)
        // `scan` yields `Some(_)` on every item so the stream never ends
        // early; the inner Option is the (possibly absent) merged event.
        .scan(DiscoveryRegistry::new(), |registry, (provider, event)| {
            let out = registry.observe(&provider, event);
            futures::future::ready(Some(out))
        })
        .filter_map(futures::future::ready)
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use peerbeam_domain::entity::{DeviceType, Platform};

    fn device_with_addrs(id: &str, addrs: &[&str]) -> Device {
        Device {
            id: DeviceId::from(id),
            name: "Peer".to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: addrs.iter().map(|s| s.to_string()).collect(),
            port: 9000,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn first_sighting_is_peer_found() {
        let mut reg = DiscoveryRegistry::new();
        let ev = reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        assert!(matches!(ev, Some(DomainEvent::PeerFound(_))));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn same_device_from_second_provider_dedups_silently() {
        let mut reg = DiscoveryRegistry::new();
        reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        // mDNS sees the same device at the same address — no new info.
        let ev = reg.observe(
            &ProviderId::from("mdns"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        assert!(ev.is_none(), "redundant sighting must not emit");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn second_provider_new_address_unions_and_updates() {
        let mut reg = DiscoveryRegistry::new();
        reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        let ev = reg.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device_with_addrs("a", &["100.64.0.5"])),
        );
        match ev {
            Some(DomainEvent::PeerUpdated(d)) => {
                assert!(d.addresses.contains(&"10.0.0.1".to_string()));
                assert!(d.addresses.contains(&"100.64.0.5".to_string()));
            }
            other => panic!("expected PeerUpdated, got {other:?}"),
        }
    }

    #[test]
    fn lost_from_one_provider_keeps_device_if_another_sees_it() {
        let mut reg = DiscoveryRegistry::new();
        reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        reg.observe(
            &ProviderId::from("mdns"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );

        let ev = reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        assert!(ev.is_none(), "still visible via mdns");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn lost_from_last_provider_emits_peer_lost() {
        let mut reg = DiscoveryRegistry::new();
        reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device_with_addrs("a", &["10.0.0.1"])),
        );
        let ev = reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        match ev {
            Some(DomainEvent::PeerLost(id)) => assert_eq!(id, DeviceId::from("a")),
            other => panic!("expected PeerLost, got {other:?}"),
        }
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn lost_unknown_device_is_noop() {
        let mut reg = DiscoveryRegistry::new();
        let ev = reg.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("ghost")),
        );
        assert!(ev.is_none());
    }
}
