//! In-memory peer liveness table.
//!
//! Pure logic, no IO — decides when an observation is a new `Found`, a
//! changed `Updated`, or a silent liveness refresh, and which peers have
//! aged out (`Lost`). Kept separate so the discovery event semantics are
//! unit-testable without any sockets or timers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use peerbeam_domain::entity::Device;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::DiscoveryEvent;

use crate::proto::same_identity;

struct Entry {
    device: Device,
    last_seen: Instant,
}

/// Tracks currently-visible peers and their last-seen times.
pub(crate) struct PeerTable {
    map: HashMap<DeviceId, Entry>,
}

impl PeerTable {
    /// Create an empty table.
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Record an observation of `device` at time `now`.
    ///
    /// Returns the event to emit, or `None` when the peer was already known
    /// and unchanged (a pure liveness refresh, which must not spam the UI).
    pub(crate) fn observe(&mut self, device: Device, now: Instant) -> Option<DiscoveryEvent> {
        match self.map.get_mut(&device.id) {
            None => {
                self.map.insert(
                    device.id.clone(),
                    Entry {
                        device: device.clone(),
                        last_seen: now,
                    },
                );
                Some(DiscoveryEvent::Found(device))
            }
            Some(entry) => {
                let changed = !same_identity(&entry.device, &device);
                entry.device = device.clone();
                entry.last_seen = now;
                changed.then_some(DiscoveryEvent::Updated(device))
            }
        }
    }

    /// Remove peers not seen within `ttl` of `now`, returning their ids so
    /// the caller can emit `Lost` events.
    pub(crate) fn expire(&mut self, now: Instant, ttl: Duration) -> Vec<DeviceId> {
        let stale: Vec<DeviceId> = self
            .map
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_seen) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            self.map.remove(id);
        }
        stale
    }

    /// Remove and return the ids of all known peers (used on `stop`).
    pub(crate) fn drain_ids(&mut self) -> Vec<DeviceId> {
        let ids: Vec<DeviceId> = self.map.keys().cloned().collect();
        self.map.clear();
        ids
    }

    /// Number of currently-known peers.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use peerbeam_domain::entity::{DeviceType, Platform};

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: DeviceId::from(id),
            name: name.to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: vec!["10.0.0.2".to_string()],
            port: 9000,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn first_observation_is_found() {
        let mut t = PeerTable::new();
        let ev = t.observe(device("a", "A"), Instant::now());
        assert!(matches!(ev, Some(DiscoveryEvent::Found(_))));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn unchanged_reobservation_is_silent() {
        let mut t = PeerTable::new();
        let now = Instant::now();
        t.observe(device("a", "A"), now);
        let ev = t.observe(device("a", "A"), now);
        assert!(ev.is_none());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn changed_reobservation_is_updated() {
        let mut t = PeerTable::new();
        let now = Instant::now();
        t.observe(device("a", "A"), now);
        let ev = t.observe(device("a", "A-renamed"), now);
        assert!(matches!(ev, Some(DiscoveryEvent::Updated(_))));
    }

    #[test]
    fn expire_removes_only_stale_peers() {
        let mut t = PeerTable::new();
        let t0 = Instant::now();
        t.observe(device("fresh", "F"), t0);

        // Peer observed at t0; check expiry from a point 2s later with a 1s ttl.
        let later = t0 + Duration::from_secs(2);
        let lost = t.expire(later, Duration::from_secs(1));
        assert_eq!(lost, vec![DeviceId::from("fresh")]);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn expire_keeps_recent_peers() {
        let mut t = PeerTable::new();
        let t0 = Instant::now();
        t.observe(device("recent", "R"), t0);
        let lost = t.expire(t0, Duration::from_secs(5));
        assert!(lost.is_empty());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn drain_ids_clears_and_returns_all() {
        let mut t = PeerTable::new();
        t.observe(device("a", "A"), Instant::now());
        t.observe(device("b", "B"), Instant::now());
        let mut ids = t.drain_ids();
        ids.sort_by(|x, y| x.as_str().cmp(y.as_str()));
        assert_eq!(ids, vec![DeviceId::from("a"), DeviceId::from("b")]);
        assert_eq!(t.len(), 0);
    }
}
