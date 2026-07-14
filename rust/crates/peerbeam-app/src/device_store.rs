//! The device tracking reducer — pure heart of the `DeviceManager`.
//!
//! Folds per-provider [`DiscoveryEvent`]s into a merged, deduplicated set of
//! [`ManagedDevice`]s and reports every change as a [`DeviceChange`]. No IO,
//! no runtime — the async `DeviceManager` (in the engine) just drives this.
//!
//! Responsibilities:
//! - **Merge / dedup** — one entry per [`DeviceId`] regardless of how many
//!   providers see it; addresses are unioned.
//! - **Online / offline** — a device stays tracked when its last provider
//!   drops it, flipped to offline rather than deleted (the UI can grey it
//!   out); [`prune`](DeviceStore::prune) removes long-gone devices.
//! - **Latency** — stored per device via [`record_latency`](DeviceStore::record_latency);
//!   measurement lives in the networking layer, not here.
//! - **Capabilities** — derived from the capabilities of the providers that
//!   see the device (LAN vs remote vs Tailscale-only).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};

use peerbeam_domain::entity::{Device, DeviceCapabilities, ManagedDevice};
use peerbeam_domain::event::DeviceChange;
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent};

/// Internal per-device state.
struct Entry {
    device: Device,
    online: bool,
    last_seen: DateTime<Utc>,
    latency_ms: Option<u32>,
    /// Providers currently reporting this device.
    providers: HashSet<ProviderId>,
    /// Provider that currently owns the identity fields (name/type/platform/
    /// port). Only it may change them, so a genuine rename propagates while
    /// alternating providers can't flap the identity. Ownership transfers when
    /// the owner drops off.
    identity_provider: ProviderId,
}

/// Tracks all known devices and turns provider events into [`DeviceChange`]s.
pub struct DeviceStore {
    /// Capabilities of each registered provider, for deriving reachability.
    provider_caps: HashMap<ProviderId, DiscoveryCaps>,
    devices: HashMap<DeviceId, Entry>,
}

impl DeviceStore {
    /// Create a store aware of each provider's capabilities.
    pub fn new(provider_caps: HashMap<ProviderId, DiscoveryCaps>) -> Self {
        Self {
            provider_caps,
            devices: HashMap::new(),
        }
    }

    /// Fold one provider event into the tracked set, returning the changes to
    /// notify the UI about (possibly empty).
    pub fn observe(&mut self, provider: &ProviderId, event: DiscoveryEvent) -> Vec<DeviceChange> {
        match event {
            DiscoveryEvent::Found(device) | DiscoveryEvent::Updated(device) => {
                self.upsert(provider, device)
            }
            DiscoveryEvent::Lost(id) => self.drop_provider(provider, &id),
        }
    }

    fn upsert(&mut self, provider: &ProviderId, device: Device) -> Vec<DeviceChange> {
        let id = device.id.clone();
        let last_seen = device.last_seen;

        match self.devices.get_mut(&id) {
            None => {
                let mut providers = HashSet::new();
                providers.insert(provider.clone());
                let entry = Entry {
                    device,
                    online: true,
                    last_seen,
                    latency_ms: None,
                    providers,
                    identity_provider: provider.clone(),
                };
                let managed = self.to_managed(&entry);
                self.devices.insert(id, entry);
                vec![DeviceChange::Added(managed)]
            }
            Some(entry) => {
                let was_online = entry.online;
                let new_provider = entry.providers.insert(provider.clone());
                // Only the identity owner (or a takeover when the owner has
                // dropped) may change name/type/platform/port; any other
                // provider fills blank/zero fields only. Kills cross-provider
                // flapping while still surfacing a real rename from the owner.
                let owner_present = entry.providers.contains(&entry.identity_provider);
                let take_identity = *provider == entry.identity_provider || !owner_present;
                if take_identity {
                    entry.identity_provider = provider.clone();
                }
                let merged = merge(&entry.device, &device, take_identity);
                let identity_changed = !entry.device.same_identity(&merged);
                entry.device = merged;
                entry.online = true;
                entry.last_seen = last_seen;

                let mut changes = Vec::new();
                if !was_online {
                    changes.push(DeviceChange::StatusChanged {
                        id: id.clone(),
                        online: true,
                    });
                }
                // Address/name change or a new provider (→ capabilities change)
                // is a meaningful Updated; a plain re-sighting is silent.
                if identity_changed || new_provider {
                    // Re-borrow immutably to build the managed view.
                    let managed = self.to_managed(self.devices.get(&id).unwrap());
                    changes.push(DeviceChange::Updated(managed));
                }
                changes
            }
        }
    }

    fn drop_provider(&mut self, provider: &ProviderId, id: &DeviceId) -> Vec<DeviceChange> {
        let Some(entry) = self.devices.get_mut(id) else {
            return Vec::new();
        };
        entry.providers.remove(provider);

        // A LAN provider's Lost is subnet-wide liveness evidence: the peer's
        // heartbeat on this network stopped. Other LAN claims (e.g. an mDNS
        // cache entry that never got a goodbye because the peer died
        // uncleanly) are not independent proof of life — drop them with it.
        // Cross-subnet claims (Tailscale) are a different path and keep the
        // device online.
        let lost_lan = self
            .provider_caps
            .get(provider)
            .is_some_and(|c| !c.crosses_subnet);
        if lost_lan {
            let caps = &self.provider_caps;
            entry
                .providers
                .retain(|p| caps.get(p).is_some_and(|c| c.crosses_subnet));
        }

        if entry.providers.is_empty() {
            entry.online = false;
            vec![DeviceChange::StatusChanged {
                id: id.clone(),
                online: false,
            }]
        } else {
            // Fewer providers → capabilities may have shrunk → Updated.
            let managed = self.to_managed(self.devices.get(id).unwrap());
            vec![DeviceChange::Updated(managed)]
        }
    }

    /// Record a measured latency for a device. Returns a change if it moved.
    pub fn record_latency(&mut self, id: &DeviceId, latency_ms: Option<u32>) -> Vec<DeviceChange> {
        match self.devices.get_mut(id) {
            Some(entry) if entry.latency_ms != latency_ms => {
                entry.latency_ms = latency_ms;
                vec![DeviceChange::LatencyChanged {
                    id: id.clone(),
                    latency_ms,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Remove offline devices last seen longer ago than `ttl`, returning a
    /// `Removed` change for each.
    pub fn prune(&mut self, now: DateTime<Utc>, ttl: Duration) -> Vec<DeviceChange> {
        let stale: Vec<DeviceId> = self
            .devices
            .iter()
            .filter(|(_, e)| !e.online && now.signed_duration_since(e.last_seen) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        stale
            .into_iter()
            .map(|id| {
                self.devices.remove(&id);
                DeviceChange::Removed(id)
            })
            .collect()
    }

    /// Snapshot of all tracked devices, online first then by name.
    pub fn snapshot(&self) -> Vec<ManagedDevice> {
        let mut out: Vec<ManagedDevice> =
            self.devices.values().map(|e| self.to_managed(e)).collect();
        out.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| a.device.name.cmp(&b.device.name))
        });
        out
    }

    /// Number of tracked devices.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether no devices are tracked.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    fn to_managed(&self, entry: &Entry) -> ManagedDevice {
        ManagedDevice {
            device: entry.device.clone(),
            online: entry.online,
            last_seen: entry.last_seen,
            latency_ms: entry.latency_ms,
            capabilities: self.capabilities_for(&entry.providers),
        }
    }

    fn capabilities_for(&self, providers: &HashSet<ProviderId>) -> DeviceCapabilities {
        let mut provs: Vec<ProviderId> = providers.iter().cloned().collect();
        provs.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        let mut reachable_lan = false;
        let mut reachable_remote = false;
        let mut any = false;
        let mut all_require_tailscale = true;

        for provider in &provs {
            any = true;
            match self.provider_caps.get(provider) {
                Some(caps) => {
                    if caps.crosses_subnet {
                        reachable_remote = true;
                    } else {
                        reachable_lan = true;
                    }
                    if !caps.requires_tailscale {
                        all_require_tailscale = false;
                    }
                }
                // Unknown provider: assume plain LAN reach.
                None => {
                    reachable_lan = true;
                    all_require_tailscale = false;
                }
            }
        }

        DeviceCapabilities {
            reachable_lan,
            reachable_remote,
            requires_tailscale: any && all_require_tailscale,
            providers: provs,
        }
    }
}

/// Merge a re-sighting into the tracked device, always unioning addresses and
/// freshening `last_seen`. When `take_identity` is true the incoming identity
/// (name/type/platform/port) is adopted — used only for the identity owner, so
/// a genuine rename propagates. Otherwise the established identity is kept and
/// only blank/zero fields are filled, so two providers that disagree can't flap
/// the name.
fn merge(existing: &Device, incoming: &Device, take_identity: bool) -> Device {
    // Union addresses: keep the established order, append genuinely new ones.
    let mut addresses = existing.addresses.clone();
    for addr in &incoming.addresses {
        if !addresses.contains(addr) {
            addresses.push(addr.clone());
        }
    }
    if take_identity {
        // The owner decides identity — but a blank name / zero port from the
        // owner must not clobber good data another sighting already supplied.
        return Device {
            id: existing.id.clone(),
            name: if incoming.name.is_empty() {
                existing.name.clone()
            } else {
                incoming.name.clone()
            },
            device_type: incoming.device_type,
            platform: incoming.platform,
            port: if incoming.port == 0 {
                existing.port
            } else {
                incoming.port
            },
            addresses,
            last_seen: incoming.last_seen,
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

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{DeviceType, Platform};

    fn caps_map() -> HashMap<ProviderId, DiscoveryCaps> {
        let mut m = HashMap::new();
        m.insert(
            ProviderId::from("udp"),
            DiscoveryCaps {
                can_advertise: true,
                can_scan: true,
                crosses_subnet: false,
                requires_tailscale: false,
            },
        );
        m.insert(
            ProviderId::from("tailscale"),
            DiscoveryCaps {
                can_advertise: false,
                can_scan: true,
                crosses_subnet: true,
                requires_tailscale: true,
            },
        );
        m.insert(
            ProviderId::from("mdns"),
            DiscoveryCaps {
                can_advertise: true,
                can_scan: true,
                crosses_subnet: false,
                requires_tailscale: false,
            },
        );
        m
    }

    fn device(id: &str, name: &str, addr: &str) -> Device {
        Device {
            id: DeviceId::from(id),
            name: name.to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: vec![addr.to_string()],
            port: 9000,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn first_sighting_adds_online_device() {
        let mut store = DeviceStore::new(caps_map());
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        assert!(matches!(changes.as_slice(), [DeviceChange::Added(m)] if m.online));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn second_provider_dedups_and_updates_capabilities() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        let changes = store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "A", "100.64.0.1")),
        );
        assert_eq!(store.len(), 1, "same device, not duplicated");
        match changes.as_slice() {
            [DeviceChange::Updated(m)] => {
                assert!(m.capabilities.reachable_lan);
                assert!(m.capabilities.reachable_remote);
                assert!(!m.capabilities.requires_tailscale, "reachable on LAN too");
                assert_eq!(m.capabilities.providers.len(), 2);
                assert!(m.device.addresses.contains(&"10.0.0.1".to_string()));
                assert!(m.device.addresses.contains(&"100.64.0.1".to_string()));
            }
            other => panic!("expected one Updated, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_provider_names_do_not_flap() {
        let mut store = DeviceStore::new(caps_map());
        // First provider establishes the name.
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice's Laptop", "10.0.0.1")),
        );
        // A second provider reports the same device with a different name and a
        // new address. The address is unioned, but the name must NOT change.
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice", "100.64.0.1")),
        );
        // The alternate provider sighting again must still not flip the name.
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice", "100.64.0.1")),
        );
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device.name, "Alice's Laptop", "name is stable");
        assert!(snap[0].device.addresses.contains(&"10.0.0.1".to_string()));
        assert!(snap[0].device.addresses.contains(&"100.64.0.1".to_string()));
    }

    #[test]
    fn owner_provider_rename_propagates() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice", "10.0.0.1")),
        );
        // Same (owning) provider reports a genuine rename → it must propagate.
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Updated(device("a", "Alice-Renamed", "10.0.0.1")),
        );
        assert!(
            matches!(changes.as_slice(), [DeviceChange::Updated(m)] if m.device.name == "Alice-Renamed"),
            "owner rename should emit Updated with the new name, got {changes:?}"
        );
    }

    #[test]
    fn identity_ownership_transfers_when_owner_drops() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice", "10.0.0.1")),
        );
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice-TS", "100.64.0.1")),
        );
        // udp (the owner) drops; tailscale is now the only provider.
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        // Next tailscale sighting takes over identity → its name wins.
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice-TS", "100.64.0.1")),
        );
        let snap = store.snapshot();
        assert_eq!(
            snap[0].device.name, "Alice-TS",
            "ownership transferred to tailscale"
        );
    }

    #[test]
    fn owner_blank_resight_does_not_clobber() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice", "10.0.0.1")),
        );
        // The owner re-sights with a degraded packet (blank name, port 0). It
        // must NOT wipe the good identity (would otherwise flap ""/0 ↔ real).
        let mut blank = device("a", "", "10.0.0.1");
        blank.port = 0;
        store.observe(&ProviderId::from("udp"), DiscoveryEvent::Found(blank));
        let snap = store.snapshot();
        assert_eq!(
            snap[0].device.name, "Alice",
            "owner blank must not wipe name"
        );
        assert_eq!(
            snap[0].device.port, 9000,
            "owner zero port must not wipe port"
        );
    }

    #[test]
    fn merge_fills_blank_name_and_zero_port() {
        let mut store = DeviceStore::new(caps_map());
        // A provider that only knows the address (blank name, port 0).
        let mut blank = device("a", "", "10.0.0.1");
        blank.port = 0;
        store.observe(&ProviderId::from("udp"), DiscoveryEvent::Found(blank));
        // A richer sighting fills the gaps.
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice", "100.64.0.1")),
        );
        let snap = store.snapshot();
        assert_eq!(snap[0].device.name, "Alice", "blank name gets filled");
        assert_eq!(snap[0].device.port, 9000, "zero port gets filled");
    }

    #[test]
    fn identical_resighting_is_silent() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn tailscale_only_device_requires_tailscale() {
        let mut store = DeviceStore::new(caps_map());
        let changes = store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "A", "100.64.0.1")),
        );
        match changes.as_slice() {
            [DeviceChange::Added(m)] => {
                assert!(m.capabilities.reachable_remote);
                assert!(!m.capabilities.reachable_lan);
                assert!(m.capabilities.requires_tailscale);
            }
            other => panic!("expected Added, got {other:?}"),
        }
    }

    #[test]
    fn lost_from_one_of_two_providers_keeps_online() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "A", "100.64.0.1")),
        );
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        // Still online via tailscale; capabilities shrink → Updated.
        match changes.as_slice() {
            [DeviceChange::Updated(m)] => {
                assert!(m.online);
                assert!(!m.capabilities.reachable_lan);
                assert!(m.capabilities.reachable_remote);
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[test]
    fn lost_from_last_provider_goes_offline_but_stays_tracked() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        assert_eq!(
            changes,
            vec![DeviceChange::StatusChanged {
                id: DeviceId::from("a"),
                online: false
            }]
        );
        assert_eq!(store.len(), 1, "offline device still tracked");
    }

    #[test]
    fn rediscovery_brings_back_online() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        assert!(changes.contains(&DeviceChange::StatusChanged {
            id: DeviceId::from("a"),
            online: true
        }));
    }

    #[test]
    fn record_latency_reports_only_on_change() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        let first = store.record_latency(&DeviceId::from("a"), Some(12));
        assert_eq!(
            first,
            vec![DeviceChange::LatencyChanged {
                id: DeviceId::from("a"),
                latency_ms: Some(12)
            }]
        );
        // Same value → no change.
        assert!(store
            .record_latency(&DeviceId::from("a"), Some(12))
            .is_empty());
        // Unknown device → no change.
        assert!(store
            .record_latency(&DeviceId::from("ghost"), Some(1))
            .is_empty());
    }

    #[test]
    fn prune_removes_only_stale_offline_devices() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "A", "10.0.0.1")),
        );
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );

        // Not yet stale.
        let now = Utc::now();
        assert!(store.prune(now, Duration::seconds(60)).is_empty());

        // Far in the future → stale → removed.
        let later = now + Duration::seconds(120);
        let changes = store.prune(later, Duration::seconds(60));
        assert_eq!(changes, vec![DeviceChange::Removed(DeviceId::from("a"))]);
        assert!(store.is_empty());
    }

    #[test]
    fn snapshot_lists_online_before_offline() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("b", "Bravo", "10.0.0.2")),
        );
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alpha", "10.0.0.1")),
        );
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("b")),
        );

        let snap = store.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap[0].online, "online device first");
        assert_eq!(snap[0].device.name, "Alpha");
        assert!(!snap[1].online);
    }

    /// A LAN provider's Lost must also drop other LAN claims: an mDNS cache
    /// entry with no goodbye is not independent liveness, so the device goes
    /// offline instead of lingering "online" for the mDNS TTL (~minutes).
    #[test]
    fn lan_lost_collapses_other_lan_claims() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice", "192.168.1.9")),
        );
        store.observe(
            &ProviderId::from("mdns"),
            DiscoveryEvent::Found(device("a", "Alice", "192.168.1.9")),
        );

        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        assert!(
            matches!(
                changes.as_slice(),
                [DeviceChange::StatusChanged { online: false, .. }]
            ),
            "expected offline, got {changes:?}"
        );
        assert!(!store.snapshot()[0].online);
    }

    /// A cross-subnet claim (Tailscale) is a different network path — it keeps
    /// the device online when a LAN provider loses it.
    #[test]
    fn lan_lost_keeps_cross_subnet_claim() {
        let mut store = DeviceStore::new(caps_map());
        store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Found(device("a", "Alice", "192.168.1.9")),
        );
        store.observe(
            &ProviderId::from("tailscale"),
            DiscoveryEvent::Found(device("a", "Alice", "100.64.0.9")),
        );

        let changes = store.observe(
            &ProviderId::from("udp"),
            DiscoveryEvent::Lost(DeviceId::from("a")),
        );
        assert!(
            matches!(changes.as_slice(), [DeviceChange::Updated(_)]),
            "expected Updated (still online), got {changes:?}"
        );
        assert!(store.snapshot()[0].online);
    }
}
