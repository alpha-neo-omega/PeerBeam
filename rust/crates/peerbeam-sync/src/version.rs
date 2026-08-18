//! Version vectors: the difference between *newer* and *diverged*.
//!
//! # Why a timestamp is not enough
//!
//! Comparing modification times answers "which is more recent?", which is a
//! different question from "did these two devices both change this?". Two
//! devices that edited the same file while apart produce two mtimes, and the
//! later one wins — silently discarding the other edit. A version vector can
//! tell the two cases apart, and that distinction is what every other
//! bidirectional behaviour depends on: conflict files, safe deletes, knowing
//! when a push is safe.
//!
//! Each device counts **its own** edits. A vector is the set of counters a file
//! has seen, so `{alice: 3, bob: 1}` means "alice's third edit, which already
//! included bob's first".

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How two versions of the same file relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Byte-for-byte the same history. Nothing to do.
    Same,
    /// The other side has everything this one has, and more: take theirs.
    Behind,
    /// This side has everything the other has, and more: send ours.
    Ahead,
    /// **Both changed since they last agreed.** Neither is correct, and no
    /// automatic rule can pick — this is the case a timestamp comparison cannot
    /// see, and the one that loses work when it is guessed at.
    Diverged,
}

/// A per-device edit counter for one file.
///
/// `BTreeMap` rather than `HashMap` so the serialised form is stable: a vector
/// that reordered itself between writes would look changed when it was not.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionVector(BTreeMap<String, u64>);

impl VersionVector {
    #[must_use]
    pub fn new() -> Self {
        VersionVector(BTreeMap::new())
    }

    /// Record one edit made by `device`.
    ///
    /// Only ever increments this device's own counter. A device that could
    /// raise someone else's counter could claim to have seen edits it never
    /// received, and its next sync would discard them as already-applied.
    pub fn bump(&mut self, device: &str) {
        *self.0.entry(device.to_string()).or_insert(0) += 1;
    }

    /// This device's counter, or zero.
    #[must_use]
    pub fn get(&self, device: &str) -> u64 {
        self.0.get(device).copied().unwrap_or(0)
    }

    /// Whether nothing has ever been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Take the highest counter from each side.
    ///
    /// Used when a conflict is resolved: the result descends from both, so
    /// neither side later believes it is still ahead and re-sends.
    #[must_use]
    pub fn merge(&self, other: &VersionVector) -> VersionVector {
        let mut out = self.0.clone();
        for (device, &count) in &other.0 {
            let slot = out.entry(device.clone()).or_insert(0);
            *slot = (*slot).max(count);
        }
        VersionVector(out)
    }

    /// How `self` relates to `other`.
    ///
    /// The whole point of this module. `self` is ahead if every one of its
    /// counters is at least the other's and one is strictly greater; behind is
    /// the mirror; equal counters everywhere is [`Relation::Same`]; and
    /// anything else — each side holding a counter the other does not — is
    /// [`Relation::Diverged`].
    #[must_use]
    pub fn relate(&self, other: &VersionVector) -> Relation {
        let mut self_higher = false;
        let mut other_higher = false;
        // Every device named by either side. A device missing from one vector
        // counts as zero there, which is exactly what "has not seen any of its
        // edits" means.
        for device in self.0.keys().chain(other.0.keys()) {
            match self.get(device).cmp(&other.get(device)) {
                Ordering::Greater => self_higher = true,
                Ordering::Less => other_higher = true,
                Ordering::Equal => {}
            }
        }
        match (self_higher, other_higher) {
            (false, false) => Relation::Same,
            (true, false) => Relation::Ahead,
            (false, true) => Relation::Behind,
            (true, true) => Relation::Diverged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vv(pairs: &[(&str, u64)]) -> VersionVector {
        let mut v = VersionVector::new();
        for (d, n) in pairs {
            v.0.insert((*d).to_string(), *n);
        }
        v
    }

    #[test]
    fn identical_vectors_are_the_same() {
        assert_eq!(vv(&[("a", 2)]).relate(&vv(&[("a", 2)])), Relation::Same);
        assert_eq!(
            VersionVector::new().relate(&VersionVector::new()),
            Relation::Same
        );
    }

    #[test]
    fn a_strictly_higher_counter_is_ahead() {
        assert_eq!(vv(&[("a", 3)]).relate(&vv(&[("a", 2)])), Relation::Ahead);
        assert_eq!(vv(&[("a", 2)]).relate(&vv(&[("a", 3)])), Relation::Behind);
    }

    #[test]
    fn a_device_absent_from_a_vector_counts_as_zero() {
        // "Has not seen any of its edits" is exactly what absence means, and
        // treating it as anything else would make a first sync look like a
        // conflict.
        assert_eq!(
            vv(&[("a", 1), ("b", 1)]).relate(&vv(&[("a", 1)])),
            Relation::Ahead
        );
        assert_eq!(
            vv(&[("a", 1)]).relate(&vv(&[("a", 1), ("b", 1)])),
            Relation::Behind
        );
    }

    /// **The case a timestamp cannot see.** Two devices each edited once since
    /// they last agreed: neither is correct, and picking one by clock silently
    /// discards the other's work.
    #[test]
    fn concurrent_edits_are_diverged_not_newer() {
        let alice = vv(&[("alice", 2), ("bob", 1)]);
        let bob = vv(&[("alice", 1), ("bob", 2)]);
        assert_eq!(alice.relate(&bob), Relation::Diverged);
        assert_eq!(bob.relate(&alice), Relation::Diverged);
    }

    #[test]
    fn a_sequential_edit_after_receiving_is_ahead_not_diverged() {
        // Alice took bob's edit, then edited herself. That is a straight line,
        // not a divergence — reporting it as a conflict would make every
        // ordinary round trip produce a conflict file.
        let before = vv(&[("alice", 1), ("bob", 1)]);
        let after = vv(&[("alice", 2), ("bob", 1)]);
        assert_eq!(after.relate(&before), Relation::Ahead);
        assert_eq!(before.relate(&after), Relation::Behind);
    }

    #[test]
    fn bump_only_ever_raises_the_named_device() {
        let mut v = vv(&[("alice", 1), ("bob", 5)]);
        v.bump("alice");
        assert_eq!(v.get("alice"), 2);
        assert_eq!(v.get("bob"), 5, "bump touched another device's counter");
    }

    /// A merged vector descends from both, so neither side later believes it is
    /// still ahead and re-sends what the other already has.
    #[test]
    fn merging_takes_the_highest_of_each_and_ends_the_divergence() {
        let alice = vv(&[("alice", 2), ("bob", 1)]);
        let bob = vv(&[("alice", 1), ("bob", 2)]);
        assert_eq!(alice.relate(&bob), Relation::Diverged);

        let merged = alice.merge(&bob);
        assert_eq!(merged.get("alice"), 2);
        assert_eq!(merged.get("bob"), 2);
        assert_eq!(merged.relate(&alice), Relation::Ahead);
        assert_eq!(merged.relate(&bob), Relation::Ahead);
        assert_eq!(merged.relate(&merged), Relation::Same);
    }

    #[test]
    fn a_new_file_is_ahead_of_nothing() {
        assert_eq!(
            vv(&[("alice", 1)]).relate(&VersionVector::new()),
            Relation::Ahead
        );
    }

    #[test]
    fn the_serialised_form_is_stable_across_writes() {
        // A vector that reordered itself between writes would look changed when
        // it was not, and every sync would re-send every file.
        let v = vv(&[("zeta", 1), ("alpha", 2), ("mid", 3)]);
        let once = serde_json::to_string(&v).unwrap();
        let twice =
            serde_json::to_string(&serde_json::from_str::<VersionVector>(&once).unwrap()).unwrap();
        assert_eq!(once, twice);
        assert!(once.find("alpha") < once.find("mid"), "not ordered");
    }
}
