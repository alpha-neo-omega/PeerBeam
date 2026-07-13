//! Filesystem [`TrustStore`] with trust-on-first-use (TOFU) pinning.
//!
//! Records the fingerprint a device presented the first time it was trusted
//! and persists the set as one JSON file. On later connections the auth
//! handshake compares the presented fingerprint against the pinned one: a
//! match is authenticated, a mismatch means the device's key changed (a new
//! device reusing the id, or a man-in-the-middle) and must be rejected.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use peerbeam_domain::entity::TrustRecord;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// A [`TrustStore`] backed by a single JSON file.
pub struct FsTrust {
    path: PathBuf,
    /// In-memory cache of pinned records, keyed by device id.
    cache: Mutex<HashMap<String, TrustRecord>>,
}

impl FsTrust {
    /// Open (or start) a trust store at `path`, loading any existing pins.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let cache = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Vec<TrustRecord>>(&bytes)
                .map_err(|e| DomainError::Storage(format!("parse trust store: {e}")))?
                .into_iter()
                .map(|r| (r.device.0.clone(), r))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(DomainError::Storage(format!("read trust store: {e}"))),
        };
        Ok(Self {
            path,
            cache: Mutex::new(cache),
        })
    }

    fn persist(&self, cache: &HashMap<String, TrustRecord>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DomainError::Storage(format!("trust dir: {e}")))?;
        }
        let records: Vec<&TrustRecord> = cache.values().collect();
        let json = serde_json::to_vec_pretty(&records)
            .map_err(|e| DomainError::Storage(format!("serialize trust store: {e}")))?;
        // Atomic: write to a temp file next to the target, then rename over it.
        // A crash mid-write leaves the previous store intact rather than a
        // truncated file that fails to parse (losing every pin).
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| DomainError::Storage(format!("write trust store: {e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| DomainError::Storage(format!("commit trust store: {e}")))
    }
}

impl TrustStore for FsTrust {
    fn record(&self, record: TrustRecord) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        cache.insert(record.device.0.clone(), record);
        self.persist(&cache)
    }

    fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>> {
        Ok(self.cache.lock().unwrap().get(&device.0).cloned())
    }

    fn is_trusted(&self, device: &DeviceId) -> bool {
        self.cache.lock().unwrap().contains_key(&device.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn record(device: &str, fp: &str) -> TrustRecord {
        TrustRecord {
            device: DeviceId::from(device),
            fingerprint: fp.to_string(),
            name: "Peer".to_string(),
            trusted_at: Utc::now(),
        }
    }

    #[test]
    fn pin_lookup_and_trust() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();

        let id = DeviceId::from("dev-1");
        assert!(!store.is_trusted(&id));
        assert!(store.lookup(&id).unwrap().is_none());

        store.record(record("dev-1", "fp-abc")).unwrap();
        assert!(store.is_trusted(&id));
        assert_eq!(store.lookup(&id).unwrap().unwrap().fingerprint, "fp-abc");
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        {
            let store = FsTrust::open(&path).unwrap();
            store.record(record("dev-2", "fp-xyz")).unwrap();
        }
        let store = FsTrust::open(&path).unwrap();
        assert_eq!(
            store
                .lookup(&DeviceId::from("dev-2"))
                .unwrap()
                .unwrap()
                .fingerprint,
            "fp-xyz"
        );
    }

    #[test]
    fn record_overwrites_same_device() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        store.record(record("dev-3", "old")).unwrap();
        store.record(record("dev-3", "new")).unwrap();
        assert_eq!(
            store
                .lookup(&DeviceId::from("dev-3"))
                .unwrap()
                .unwrap()
                .fingerprint,
            "new"
        );
    }
}
