//! Noticing that a synced folder changed.
//!
//! # Why a poll rather than a filesystem watcher
//!
//! The same reason [`peerbeam watch`] gives: a watcher needs a platform-specific
//! API per OS, and the folders people point sync at — a network share, a phone's
//! USB mount, a mounted remote — are exactly the ones where those APIs report
//! nothing. A poll is slower to notice and correct everywhere.
//!
//! # Why a file must stop changing first
//!
//! A large file being written appears at zero bytes and grows. Syncing on sight
//! would send a truncated file that looks complete on both ends, and — worse
//! here than for a one-way send — would raise the version vector once per
//! observation, so a file saved while the peer was editing could manufacture a
//! conflict out of nothing. A file counts as changed only once its size and
//! mtime have held still across two consecutive scans.
//!
//! [`peerbeam watch`]: https://example.invalid

use std::collections::HashMap;

/// One file as a scan saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    pub size: u64,
    pub modified: i64,
}

/// What the scanner remembers between passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    seen: Observed,
    /// Consecutive scans this exact (size, mtime) has held.
    holds: u32,
    /// Whether this exact state has already been reported.
    ///
    /// Kept rather than dropping the entry once it settles: forgetting it makes
    /// the next scan treat the file as new, so it settles again two scans later
    /// and every unchanged file is re-reported forever.
    reported: bool,
}

/// Tracks which files have settled, across polls.
///
/// Deliberately holds **no clock and no filesystem**: it is fed observations and
/// answers which paths have stopped moving. That keeps the rule — the part that
/// decides whether a half-written file gets synced — testable without sleeping,
/// and a rule tested only by waiting is a rule nobody re-checks.
#[derive(Debug, Default)]
pub struct Settling {
    pending: HashMap<String, Pending>,
}

/// Scans a value must hold before it counts as settled: seen once, then seen
/// again unchanged.
const REQUIRED_HOLDS: u32 = 2;

impl Settling {
    #[must_use]
    pub fn new() -> Settling {
        Settling {
            pending: HashMap::new(),
        }
    }

    /// Feed one scan's observations; get back the paths that have settled.
    ///
    /// A settled path is **reported once** and then forgotten, so a file that
    /// never changes again is not re-reported on every poll. It re-enters
    /// tracking the moment its size or mtime moves again.
    pub fn observe(&mut self, scan: &[(String, Observed)]) -> Vec<String> {
        let mut settled = Vec::new();
        let mut next: HashMap<String, Pending> = HashMap::new();

        for (path, now) in scan {
            let (holds, already) = match self.pending.get(path) {
                // Unchanged since the last scan: one more hold. `saturating_add`
                // so a file left alone for a very long time cannot wrap its
                // counter back below the threshold.
                Some(prev) if prev.seen == *now => (prev.holds.saturating_add(1), prev.reported),
                // New, or changed since last time: start counting again, and
                // this new state has not been reported. A file that is still
                // growing never accumulates holds.
                _ => (1, false),
            };
            let report = holds >= REQUIRED_HOLDS && !already;
            if report {
                settled.push(path.clone());
            }
            next.insert(
                path.clone(),
                Pending {
                    seen: *now,
                    holds,
                    reported: already || report,
                },
            );
        }

        // Paths absent from this scan are gone; forgetting them is what makes a
        // deleted-then-recreated file start counting again rather than inherit
        // the holds of a file that no longer exists.
        self.pending = next;
        settled.sort();
        settled
    }

    /// How many files are still moving.
    ///
    /// Counts only what has **not** settled. Settled entries stay in the map so
    /// they are not re-reported, but they are not "still moving" — reporting
    /// them here would make this number grow with the size of the folder rather
    /// than with the work outstanding.
    #[must_use]
    pub fn unsettled(&self) -> usize {
        self.pending.values().filter(|p| !p.reported).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(path: &str, size: u64, modified: i64) -> (String, Observed) {
        (path.to_string(), Observed { size, modified })
    }

    #[test]
    fn a_file_settles_only_after_holding_still() {
        let mut s = Settling::new();
        assert!(
            s.observe(&[obs("a", 100, 5)]).is_empty(),
            "a file settled on first sight"
        );
        assert_eq!(s.observe(&[obs("a", 100, 5)]), vec!["a"]);
    }

    /// **The failure this exists to prevent.** A file still being written must
    /// never be reported, or sync would send a truncated file that looks
    /// complete on both ends.
    #[test]
    fn a_growing_file_never_settles() {
        let mut s = Settling::new();
        for size in [10u64, 200, 3000, 40_000, 500_000] {
            assert!(
                s.observe(&[obs("big", size, 5)]).is_empty(),
                "a file still growing was reported at {size} bytes"
            );
        }
        // The last scan above already recorded 500_000, so the next one that
        // sees the same value is its second consecutive hold — it settles there.
        assert_eq!(s.observe(&[obs("big", 500_000, 5)]), vec!["big"]);
    }

    #[test]
    fn a_file_rewritten_to_the_same_size_still_settles_on_its_mtime() {
        // Same length, different content: the mtime is what catches it.
        let mut s = Settling::new();
        s.observe(&[obs("a", 100, 5)]);
        assert!(
            s.observe(&[obs("a", 100, 9)]).is_empty(),
            "an mtime change did not reset the count"
        );
        assert_eq!(s.observe(&[obs("a", 100, 9)]), vec!["a"]);
    }

    #[test]
    fn a_settled_file_is_reported_once_not_every_poll() {
        let mut s = Settling::new();
        s.observe(&[obs("a", 1, 1)]);
        assert_eq!(s.observe(&[obs("a", 1, 1)]), vec!["a"]);
        assert!(
            s.observe(&[obs("a", 1, 1)]).is_empty(),
            "an unchanged file was reported again"
        );
        assert!(s.observe(&[obs("a", 1, 1)]).is_empty());
    }

    #[test]
    fn a_settled_file_that_changes_again_is_reported_again() {
        let mut s = Settling::new();
        s.observe(&[obs("a", 1, 1)]);
        assert_eq!(s.observe(&[obs("a", 1, 1)]), vec!["a"]);

        assert!(
            s.observe(&[obs("a", 2, 7)]).is_empty(),
            "reported too early"
        );
        assert_eq!(s.observe(&[obs("a", 2, 7)]), vec!["a"]);
    }

    #[test]
    fn a_vanished_file_is_forgotten_rather_than_left_pending() {
        let mut s = Settling::new();
        s.observe(&[obs("a", 1, 1)]);
        assert_eq!(s.unsettled(), 1);
        s.observe(&[]);
        assert_eq!(s.unsettled(), 0, "a deleted file stayed pending forever");
    }

    /// A file deleted and recreated must start counting again, not inherit the
    /// holds of the file that used to be there.
    #[test]
    fn a_recreated_file_starts_counting_again() {
        let mut s = Settling::new();
        s.observe(&[obs("a", 100, 5)]);
        s.observe(&[]);
        assert!(
            s.observe(&[obs("a", 100, 5)]).is_empty(),
            "a recreated file inherited its predecessor's holds"
        );
    }

    #[test]
    fn several_files_settle_independently() {
        let mut s = Settling::new();
        s.observe(&[obs("steady", 10, 1), obs("growing", 10, 1)]);
        let settled = s.observe(&[obs("steady", 10, 1), obs("growing", 99, 2)]);
        assert_eq!(settled, vec!["steady"]);
        assert_eq!(s.unsettled(), 1);
    }

    #[test]
    fn the_reported_order_is_stable() {
        // Reported paths drive index writes and network requests; an order that
        // varied per run would make every downstream trace unreproducible.
        let mut s = Settling::new();
        let scan = [obs("z", 1, 1), obs("a", 1, 1), obs("m", 1, 1)];
        s.observe(&scan);
        assert_eq!(s.observe(&scan), vec!["a", "m", "z"]);
    }
}
