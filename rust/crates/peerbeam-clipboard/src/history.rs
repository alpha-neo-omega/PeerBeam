//! A bounded, local record of clips that passed through PeerBeam.
//!
//! # Why this exists at all, having said it would not
//!
//! When clipboard sync shipped, its changelog said plainly: *"Nothing is
//! persisted: there is no clipboard history, because a durable log of
//! everything you ever copied is precisely what this feature must not
//! create."* That statement was about a log created **as a side effect of
//! syncing** — automatic, unbounded, and never asked for. It remains true, and
//! nothing here changes it: with the history opt-in off, sync still persists
//! nothing.
//!
//! What this adds is a different thing: a log the user turned on, capped at
//! [`MAX_ENTRIES`], kept only on this device, and erasable in one action. The
//! objection was to the surprise and the permanence, not to the idea that
//! someone might want to see what they copied a minute ago.
//!
//! Three properties are load-bearing, and each is tested:
//!
//! * **Off by default.** An absent or unreadable setting is not consent.
//! * **Bounded.** The oldest entry is evicted, so the log cannot grow into the
//!   durable archive the original objection was about.
//! * **Local.** History is never put on the wire. Only the current clip syncs;
//!   what a device remembers is its own business.

use std::sync::Arc;

use peerbeam_domain::port::AppStore;
use serde::{Deserialize, Serialize};

use crate::message::ClipboardError;

/// The namespace clipboard history lives in.
pub const NS: &str = "clipboard-history";

/// How many clips are kept.
///
/// A bound, not a preference. Unbounded history is the thing the original
/// no-history decision was actually about: fifty entries is a scrollback, ten
/// thousand is an archive of everything you ever copied.
pub const MAX_ENTRIES: usize = 50;

/// One remembered clip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipEntry {
    /// Sort key and identity: 13-digit unix-millis, so entries order by time
    /// without a second field to keep in step.
    pub id: String,
    /// The clip itself.
    pub text: String,
    /// The device this came from, or `None` when this device copied it.
    pub from: Option<String>,
    /// RFC 3339 time it was recorded.
    pub at: String,
}

/// Clipboard history, persisted and encrypted at rest by the [`AppStore`].
#[derive(Clone)]
pub struct ClipHistory {
    store: Arc<dyn AppStore>,
}

impl ClipHistory {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>) -> Self {
        ClipHistory { store }
    }

    /// Remember a clip, evicting the oldest once [`MAX_ENTRIES`] is reached.
    ///
    /// **The caller decides whether history is on**; this records what it is
    /// given. Keeping the opt-in out of here means one place decides it, rather
    /// than one per call site — and a test can drive the store directly without
    /// standing up settings.
    ///
    /// A clip identical to the newest entry is not recorded twice: copying the
    /// same thing again is one fact, and duplicate rows would push real history
    /// out of a bounded list.
    pub fn record(&self, text: &str, from: Option<&str>) -> Result<bool, ClipboardError> {
        if text.is_empty() {
            return Ok(false);
        }
        let mut entries = self.list()?;
        if entries.first().is_some_and(|e| e.text == text) {
            return Ok(false);
        }
        let now = chrono::Utc::now();
        let entry = ClipEntry {
            id: format!("{:013}", now.timestamp_millis().max(0)),
            text: text.to_string(),
            from: from.map(str::to_string),
            at: now.to_rfc3339(),
        };
        self.put(&entry)?;

        // Evict from the oldest end until the bound holds. A loop rather than a
        // single removal: a build that lowered MAX_ENTRIES would otherwise
        // shrink the log one entry per copy instead of at once.
        entries.insert(0, entry);
        for old in entries.iter().skip(MAX_ENTRIES) {
            self.store
                .delete(NS, &old.id)
                .map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        }
        Ok(true)
    }

    fn put(&self, entry: &ClipEntry) -> Result<(), ClipboardError> {
        let bytes =
            serde_json::to_vec(entry).map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        self.store
            .put(NS, &entry.id, &bytes)
            .map_err(|e| ClipboardError::Serialization(e.to_string()))
    }

    /// Every remembered clip, newest first.
    ///
    /// An undecodable row is skipped rather than fatal — the same containment
    /// chat history applies, so one bad record cannot make the whole log
    /// unreadable.
    pub fn list(&self) -> Result<Vec<ClipEntry>, ClipboardError> {
        let rows = self
            .store
            .list(NS)
            .map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        let mut entries: Vec<ClipEntry> = rows
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice::<ClipEntry>(&v).ok())
            .collect();
        entries.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(entries)
    }

    /// Forget everything, and report how many entries were removed.
    ///
    /// One action, not one per entry: a user clearing clipboard history is
    /// trying to be rid of it, and making them delete fifty rows individually
    /// would be a worse answer than never having stored them.
    pub fn clear(&self) -> Result<usize, ClipboardError> {
        let n = self.list()?.len();
        self.store
            .clear(NS)
            .map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;

    fn history() -> (ClipHistory, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[5u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (ClipHistory::new(app), dir)
    }

    /// Distinct ids without sleeping: `record` stamps by millisecond, and a
    /// test writing fifty entries would otherwise collide and overwrite.
    fn put(h: &ClipHistory, i: u64, text: &str) {
        h.put(&ClipEntry {
            id: format!("{i:013}"),
            text: text.to_string(),
            from: None,
            at: "2026-01-01T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    #[test]
    fn history_is_newest_first() {
        let (h, _dir) = history();
        put(&h, 1, "first");
        put(&h, 2, "second");
        let listed = h.list().unwrap();
        assert_eq!(listed[0].text, "second");
        assert_eq!(listed[1].text, "first");
    }

    #[test]
    fn the_log_is_bounded_and_evicts_the_oldest() {
        // The bound is the whole answer to the original objection: fifty
        // entries is a scrollback, ten thousand is an archive of everything you
        // ever copied.
        let (h, _dir) = history();
        for i in 1..=(MAX_ENTRIES as u64 + 10) {
            put(&h, i, &format!("clip {i}"));
        }
        // One more through the real path, which is where eviction happens.
        h.record("newest", None).unwrap();

        let listed = h.list().unwrap();
        assert_eq!(listed.len(), MAX_ENTRIES, "the log grew past its bound");
        assert_eq!(listed[0].text, "newest");
        assert!(
            !listed.iter().any(|e| e.text == "clip 1"),
            "the oldest entry survived eviction"
        );
    }

    #[test]
    fn copying_the_same_thing_twice_records_it_once() {
        // Duplicates would push real history out of a bounded list, and copying
        // the same text again is one fact rather than two.
        let (h, _dir) = history();
        assert!(h.record("same", None).unwrap());
        assert!(!h.record("same", None).unwrap());
        assert_eq!(h.list().unwrap().len(), 1);
    }

    #[test]
    fn an_empty_clip_is_never_recorded() {
        let (h, _dir) = history();
        assert!(!h.record("", None).unwrap());
        assert!(h.list().unwrap().is_empty());
    }

    #[test]
    fn clear_forgets_everything_in_one_action() {
        let (h, _dir) = history();
        put(&h, 1, "a");
        put(&h, 2, "b");
        assert_eq!(h.clear().unwrap(), 2);
        assert!(h.list().unwrap().is_empty());
    }

    #[test]
    fn a_received_clip_remembers_who_sent_it() {
        let (h, _dir) = history();
        h.record("from bob", Some("pb-bob")).unwrap();
        assert_eq!(h.list().unwrap()[0].from.as_deref(), Some("pb-bob"));
    }

    #[test]
    fn an_undecodable_row_is_skipped_rather_than_hiding_the_log() {
        let (h, dir) = history();
        put(&h, 1, "readable");

        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[5u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        app.put(NS, "junk", b"not json").unwrap();

        let listed = ClipHistory::new(app).list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].text, "readable");
    }
}
