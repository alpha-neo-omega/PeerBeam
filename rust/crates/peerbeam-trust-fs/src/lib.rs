//! Filesystem [`TrustStore`] with trust-on-first-use (TOFU) pinning.
//!
//! Records the fingerprint a device presented the first time it was trusted
//! and persists the set as one JSON file. On later connections the auth
//! handshake compares the presented fingerprint against the pinned one: a
//! match is authenticated, a mismatch means the device's key changed (a new
//! device reusing the id, or a man-in-the-middle) and must be rejected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
        // Atomic: write to a *uniquely-named* temp file next to the target, then
        // rename over it. A crash mid-write leaves the previous store intact
        // rather than a truncated file that fails to parse (losing every pin).
        // The temp name is per-process + per-call unique so two instances
        // sharing this store can't rename the same temp out from under each
        // other (which would ENOENT one of them).
        let tmp = unique_tmp(&self.path);
        std::fs::write(&tmp, json)
            .map_err(|e| DomainError::Storage(format!("write trust store: {e}")))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DomainError::Storage(format!("commit trust store: {e}"))
        })
    }
}

impl FsTrust {
    /// All pinned records, newest trust first (for management UIs).
    pub fn list(&self) -> Vec<TrustRecord> {
        let mut records: Vec<TrustRecord> = self.cache.lock().unwrap().values().cloned().collect();
        records.sort_by_key(|r| std::cmp::Reverse(r.trusted_at));
        records
    }

    /// Revoke a pin. Returns whether the device was pinned. The next
    /// connection from it will need to be trusted again (fresh TOFU).
    pub fn remove(&self, device: &DeviceId) -> Result<bool> {
        let mut cache = self.cache.lock().unwrap();
        let existed = cache.remove(&device.0).is_some();
        if existed {
            self.persist(&cache)?;
        }
        Ok(existed)
    }
}

/// A temp path next to `path`, unique per process and per call, so concurrent
/// writers never share a temp file.
fn unique_tmp(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{}.{}.tmp", std::process::id(), n));
    PathBuf::from(s)
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
    fn concurrent_writers_on_same_path_do_not_collide() {
        // Two independent stores on the same file (as two processes would be)
        // persisting concurrently must never fail — a shared fixed temp name
        // would ENOENT one writer whose temp the other already renamed away.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let a = std::sync::Arc::new(FsTrust::open(&path).unwrap());
        let b = std::sync::Arc::new(FsTrust::open(&path).unwrap());
        let mut handles = Vec::new();
        for i in 0..25 {
            let (a, b) = (a.clone(), b.clone());
            handles.push(std::thread::spawn(move || {
                a.record(record(&format!("a-{i}"), "fp"))
                    .expect("a persist");
                b.record(record(&format!("b-{i}"), "fp"))
                    .expect("b persist");
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // The final file is valid JSON (last-writer-wins content is fine).
        FsTrust::open(&path).expect("store still parses after concurrent writes");
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

    #[test]
    fn list_and_remove_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        store.record(record("dev-a", "fp-a")).unwrap();
        store.record(record("dev-b", "fp-b")).unwrap();
        assert_eq!(store.list().len(), 2);

        assert!(store.remove(&DeviceId::from("dev-a")).unwrap());
        assert!(!store.remove(&DeviceId::from("dev-a")).unwrap(), "gone");
        assert_eq!(store.list().len(), 1);

        // Removal survives a reopen (persisted).
        let reopened = FsTrust::open(&path).unwrap();
        assert!(!reopened.is_trusted(&DeviceId::from("dev-a")));
        assert!(reopened.is_trusted(&DeviceId::from("dev-b")));
    }
}
