//! Live presence state: the last status each peer sent us.
//!
//! **Nothing here is persisted** (I4). Presence is live state, so a restart
//! starts empty rather than showing stale numbers as current — a dashboard that
//! reopens to yesterday's 4% battery is worse than one that reopens to "not
//! shared". That is a deliberate design property, not a missing feature, and it
//! is why this is a plain in-memory map rather than an `AppStore` namespace
//! like `ChatStore`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use peerbeam_domain::id::DeviceId;

use crate::message::Status;

/// One peer's last status, paired with the clock reading that matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStatus {
    /// Exactly what the peer sent, after validation.
    pub status: Status,
    /// **Our** clock at the moment the frame arrived.
    ///
    /// This, not [`Status::sent_at`], is what an age or "last seen" is computed
    /// from. Peer clocks are not synchronised: a peer whose clock is an hour
    /// fast would otherwise render as "last seen in 59 minutes", and one whose
    /// clock is a day slow would render as stale while heartbeating happily.
    pub received_at: DateTime<Utc>,
}

impl PeerStatus {
    /// Whole seconds since this status arrived, floored at zero.
    ///
    /// Clamped because `now` comes from the same local clock as `received_at`,
    /// and a clock that steps backwards mid-session would otherwise produce a
    /// negative age that a surface would have to defend against.
    #[must_use]
    pub fn age_seconds(&self, now: DateTime<Utc>) -> u64 {
        (now - self.received_at).num_seconds().max(0) as u64
    }
}

/// Every peer's live status, shared between the session handlers that fill it
/// and the surfaces that read it.
///
/// `Clone` is a shallow, shared clone (like `ChatStore`): every clone sees the
/// same map, which is what lets each session's handler write into the one
/// registry the FFI and CLI read from.
#[derive(Clone, Default)]
pub struct PresenceRegistry {
    inner: Arc<Mutex<HashMap<DeviceId, PeerStatus>>>,
}

impl PresenceRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `status` from `peer`, stamped with our own receipt time,
    /// replacing whatever that peer sent before. Heartbeats are a running
    /// replacement, not a log — only the newest matters, and keeping a history
    /// would be building the persistence I4 forbids one tick at a time.
    pub fn record(&self, peer: &DeviceId, status: Status, received_at: DateTime<Utc>) {
        self.with(|m| {
            m.insert(
                peer.clone(),
                PeerStatus {
                    status,
                    received_at,
                },
            );
        });
    }

    /// One peer's last status, if it has shared one.
    #[must_use]
    pub fn get(&self, peer: &DeviceId) -> Option<PeerStatus> {
        self.with(|m| m.get(peer).cloned())
    }

    /// Every peer's last status, sorted by device id so output is stable.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(DeviceId, PeerStatus)> {
        let mut all: Vec<(DeviceId, PeerStatus)> =
            self.with(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        all.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
        all
    }

    /// Drop a peer's status — on session close, or when trust is revoked.
    ///
    /// Revocation is the case that matters: a device we no longer trust must
    /// stop appearing in the dashboard immediately, not linger until restart.
    pub fn forget(&self, peer: &DeviceId) {
        self.with(|m| {
            m.remove(peer);
        });
    }

    /// Drop everything.
    pub fn clear(&self) {
        self.with(HashMap::clear);
    }

    /// How many peers have shared a status.
    #[must_use]
    pub fn len(&self) -> usize {
        self.with(|m| m.len())
    }

    /// Whether no peer has shared a status.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run `f` under the lock, recovering from poisoning rather than panicking.
    ///
    /// A panic in one session's handler must not take down every other
    /// session's presence with it, and crate code may not `unwrap` — the same
    /// `into_inner` idiom `peerbeam_chat::mint_id` uses. The data is a plain
    /// map of last-known values with no cross-entry invariant, so a poisoned
    /// lock cannot leave it half-updated.
    fn with<T>(&self, f: impl FnOnce(&mut HashMap<DeviceId, PeerStatus>) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(battery: u8) -> Status {
        Status {
            battery_percent: Some(battery),
            sent_at: "2026-08-17T10:00:00Z".into(),
            ..Status::default()
        }
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("valid timestamp")
    }

    #[test]
    fn records_and_reads_back_a_peer_status() {
        let reg = PresenceRegistry::new();
        let bob = DeviceId::from("pb-bob");
        assert!(reg.get(&bob).is_none());
        assert!(reg.is_empty());

        reg.record(&bob, status(80), at(0));
        let got = reg.get(&bob).expect("recorded");
        assert_eq!(got.status.battery_percent, Some(80));
        assert_eq!(got.received_at, at(0));
        assert_eq!(reg.len(), 1);
    }

    /// A heartbeat replaces the previous one rather than accumulating: the
    /// registry holds *current* state, and a growing history would be exactly
    /// the persistence-by-increments I4 rules out.
    #[test]
    fn a_later_heartbeat_replaces_the_earlier_one() {
        let reg = PresenceRegistry::new();
        let bob = DeviceId::from("pb-bob");
        reg.record(&bob, status(80), at(0));
        reg.record(&bob, status(64), at(60));
        assert_eq!(reg.len(), 1, "one entry per peer, not a log");
        let got = reg.get(&bob).unwrap();
        assert_eq!(got.status.battery_percent, Some(64));
        assert_eq!(got.received_at, at(60));
    }

    #[test]
    fn peers_are_independent_and_snapshot_is_sorted() {
        let reg = PresenceRegistry::new();
        reg.record(&DeviceId::from("pb-carol"), status(10), at(0));
        reg.record(&DeviceId::from("pb-alice"), status(20), at(0));
        reg.record(&DeviceId::from("pb-bob"), status(30), at(0));
        let ids: Vec<String> = reg.snapshot().into_iter().map(|(d, _)| d.0).collect();
        assert_eq!(ids, vec!["pb-alice", "pb-bob", "pb-carol"]);
    }

    /// Age counts from OUR receipt time. A peer whose clock is an hour fast
    /// must not read as "last seen in the future".
    #[test]
    fn age_is_measured_from_our_receipt_time_not_the_peers_clock() {
        let entry = PeerStatus {
            status: Status {
                // The peer claims it sent this in the year 2099.
                sent_at: "2099-01-01T00:00:00Z".into(),
                ..status(50)
            },
            received_at: at(0),
        };
        assert_eq!(entry.age_seconds(at(90)), 90);
        // A backwards local clock step floors at zero rather than underflowing.
        assert_eq!(entry.age_seconds(at(-500)), 0);
    }

    /// Revoking trust must remove a device from the dashboard now, not at the
    /// next restart.
    #[test]
    fn forget_removes_one_peer_and_clear_removes_all() {
        let reg = PresenceRegistry::new();
        let bob = DeviceId::from("pb-bob");
        let amy = DeviceId::from("pb-amy");
        reg.record(&bob, status(1), at(0));
        reg.record(&amy, status(2), at(0));

        reg.forget(&bob);
        assert!(reg.get(&bob).is_none());
        assert!(reg.get(&amy).is_some(), "forget is not clear");

        reg.clear();
        assert!(reg.is_empty());
    }

    /// Clones share one map — that is what lets each session's handler write
    /// into the single registry every surface reads.
    #[test]
    fn clones_share_the_same_state() {
        let a = PresenceRegistry::new();
        let b = a.clone();
        b.record(&DeviceId::from("pb-bob"), status(7), at(0));
        assert_eq!(a.len(), 1, "a clone is a shared handle, not a copy");
    }
}
