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

use peerbeam_domain::entity::{Permission, PermissionSet, TrustRecord};
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
        let cache = Self::read_records(&path)?;
        Ok(Self {
            path,
            cache: Mutex::new(cache),
        })
    }

    /// Parse the on-disk record list into a map keyed by device id. A missing
    /// file means nothing is pinned yet (not an error).
    fn read_records(path: &Path) -> Result<HashMap<String, TrustRecord>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice::<Vec<TrustRecord>>(&bytes)
                .map_err(|e| DomainError::Storage(format!("parse trust store: {e}")))?
                .into_iter()
                .map(|r| (r.device.0.clone(), r))
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(DomainError::Storage(format!("read trust store: {e}"))),
        }
    }

    /// Reload the current on-disk records and fold them into `cache` so a
    /// concurrent process's writes aren't silently lost (see `persist`).
    ///
    /// For each on-disk record not already reflected in `cache`, or newer
    /// (by `trusted_at`) than `cache`'s copy, adopt the disk copy — this is
    /// the "never drop a device that exists on disk but not in memory" half
    /// of the merge. `exclude` lists device ids this very call is in the
    /// middle of removing from `cache`: without excluding them, the disk's
    /// (not-yet-updated) copy would immediately resurrect the record this
    /// call is trying to delete.
    fn merge_from_disk(
        &self,
        cache: &mut HashMap<String, TrustRecord>,
        exclude: &[&str],
    ) -> Result<()> {
        let disk = Self::read_records(&self.path)?;
        for (id, disk_rec) in disk {
            if exclude.contains(&id.as_str()) {
                continue;
            }
            match cache.get(&id) {
                Some(local) if local.trusted_at >= disk_rec.trusted_at => {
                    // Our in-memory copy is at least as fresh — keep it.
                }
                _ => {
                    cache.insert(id, disk_rec);
                }
            }
        }
        Ok(())
    }

    /// Merge the latest on-disk state into `cache` (so a concurrent writer's
    /// pins survive), then atomically write the merged result.
    ///
    /// This narrows — but, without a cross-process file lock, does not fully
    /// close — the window in which two processes writing at nearly the same
    /// instant could still race (each reads disk before the other's write
    /// lands). It does fix the reported bug: a process whose cache simply
    /// doesn't yet know about another process's earlier pin no longer
    /// clobbers it on write.
    fn persist(&self, cache: &mut HashMap<String, TrustRecord>, exclude: &[&str]) -> Result<()> {
        self.merge_from_disk(cache, exclude)?;

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
            // Exclude the just-removed id from the merge, or the on-disk
            // copy (not yet aware of this removal) would resurrect it.
            self.persist(&mut cache, &[device.0.as_str()])?;
        }
        Ok(existed)
    }

    /// Mark a pinned device as approved for auto-accept. Called only after
    /// the user explicitly accepts an incoming transfer from it — a declined
    /// transfer must never call this. A no-op (returning `Ok`) if the device
    /// isn't pinned; a device is always pinned before it can be approved.
    ///
    /// Approving also writes the device's **initial permission set**
    /// ([`PermissionSet::granted_on_approval`]): the five that existed when
    /// permissions were introduced, which is exactly what approval used to
    /// grant implicitly. A permission added in a later release is therefore not
    /// granted here — it stays opt-in, via [`set_permission`].
    ///
    /// The grant is written only on the transition to approved, so re-running
    /// `peerbeam trust approve` against an already-approved device cannot undo
    /// a permission the user deliberately revoked.
    ///
    /// [`set_permission`]: FsTrust::set_permission
    pub fn approve(&self, device: &DeviceId) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(record) = cache.get_mut(&device.0) {
            if !record.approved {
                record.approved = true;
                record.permissions = PermissionSet::granted_on_approval();
                self.persist(&mut cache, &[])?;
            }
        }
        Ok(())
    }

    /// Grant or withhold one permission for a pinned device.
    ///
    /// Returns whether a record was found and changed, so a surface can tell
    /// "done" from "that device is not pinned" and from "it was already like
    /// that" without a second lookup racing the first.
    ///
    /// Writing is all this does — it never approves. A device nobody approved
    /// may nothing whatever its bits say ([`TrustStore::may`]), so permitting a
    /// merely-pinned device stages a decision rather than taking one.
    ///
    /// The change is visible to the very next [`TrustStore::may`] on this store,
    /// because the gates re-read per operation: revoking stops the next clip,
    /// heartbeat, message or accept, not the next reconnect.
    pub fn set_permission(
        &self,
        device: &DeviceId,
        permission: Permission,
        granted: bool,
    ) -> Result<bool> {
        let mut cache = self.cache.lock().unwrap();
        let Some(record) = cache.get_mut(&device.0) else {
            return Ok(false);
        };
        let updated = record.permissions.set(permission, granted);
        if updated == record.permissions {
            return Ok(false);
        }
        record.permissions = updated;
        self.persist(&mut cache, &[])?;
        Ok(true)
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
        self.persist(&mut cache, &[])
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
            approved: false,
            permissions: PermissionSet::none(),
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
        // (Content-preservation under the reload+merge fix is covered
        // separately, deterministically, by `persist_merges_a_concurrent_
        // processes_pin_instead_of_clobbering_it` below — true multi-threaded
        // races can still narrowly interleave two persist() calls without a
        // cross-process file lock, so this test only asserts the file always
        // stays valid JSON, not exact surviving content.)
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
        FsTrust::open(&path).expect("store still parses after concurrent writes");
    }

    /// The exact scenario from the reported bug: two independent `FsTrust`
    /// instances (standing in for two processes, e.g. a daemon and a GUI)
    /// share one trust.json. P1 pins a device; P2's cache never learned
    /// about it and pins a different device. P2's persist() must merge in
    /// P1's earlier pin from disk instead of clobbering it — both survive.
    #[test]
    fn persist_merges_a_concurrent_processes_pin_instead_of_clobbering_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");

        let p1 = FsTrust::open(&path).unwrap(); // cache: {}
        let p2 = FsTrust::open(&path).unwrap(); // cache: {} (opened before p1 writes)

        p1.record(record("dev-x", "fp-x")).unwrap(); // disk: [x]; p2 still doesn't know
        p2.record(record("dev-y", "fp-y")).unwrap(); // p2 merges disk's `x` in before writing

        let reopened = FsTrust::open(&path).unwrap();
        assert!(
            reopened.is_trusted(&DeviceId::from("dev-x")),
            "P1's pin must survive P2's persist"
        );
        assert!(reopened.is_trusted(&DeviceId::from("dev-y")));

        // P2's own in-memory view was updated by the merge too, not just the
        // file — a subsequent read from the same instance sees both.
        assert!(p2.is_trusted(&DeviceId::from("dev-x")));
        assert!(p2.is_trusted(&DeviceId::from("dev-y")));
    }

    /// A device removed via `remove()` must not be resurrected by the disk
    /// copy that same persist() call reloads — the removal excludes its own
    /// target from the merge.
    #[test]
    fn remove_is_not_undone_by_its_own_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();

        store.record(record("dev-r", "fp-r")).unwrap();
        assert!(store.is_trusted(&DeviceId::from("dev-r")));

        assert!(store.remove(&DeviceId::from("dev-r")).unwrap());
        assert!(!store.is_trusted(&DeviceId::from("dev-r")));

        let reopened = FsTrust::open(&path).unwrap();
        assert!(
            !reopened.is_trusted(&DeviceId::from("dev-r")),
            "removal must persist"
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

    #[test]
    fn approve_marks_pinned_device_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-approve");

        store.record(record("dev-approve", "fp")).unwrap();
        assert!(!store.lookup(&id).unwrap().unwrap().approved);

        store.approve(&id).unwrap();
        assert!(store.lookup(&id).unwrap().unwrap().approved);

        // Persisted across reopen.
        let reopened = FsTrust::open(&path).unwrap();
        assert!(reopened.lookup(&id).unwrap().unwrap().approved);
    }

    #[test]
    fn approve_unknown_device_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        assert!(store.approve(&DeviceId::from("ghost")).is_ok());
        assert!(store.lookup(&DeviceId::from("ghost")).unwrap().is_none());
    }

    #[test]
    fn records_without_approved_field_deserialize_as_not_approved() {
        // Simulates a trust.json written before `approved` existed: old
        // records must still load, defaulting to `approved: false` (a
        // pinned-but-unapproved device requires one more explicit accept
        // after upgrading, rather than silently becoming auto-acceptable).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let legacy = serde_json::json!([{
            "device": "dev-legacy",
            "fingerprint": "fp-legacy",
            "name": "Old Peer",
            "trusted_at": Utc::now().to_rfc3339(),
        }]);
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let store = FsTrust::open(&path).unwrap();
        let rec = store
            .lookup(&DeviceId::from("dev-legacy"))
            .unwrap()
            .unwrap();
        assert!(!rec.approved);
        assert_eq!(rec.fingerprint, "fp-legacy");
    }

    // ── permissions ─────────────────────────────────────────────────────────

    /// **A `trust.json` exactly as a build before permissions wrote it**, byte
    /// for byte: `serde_json::to_vec_pretty` over two records with no
    /// `permissions` key. Written as a literal rather than built from
    /// `TrustRecord`s on purpose — a constructed record would be serialized by
    /// *this* build, which always emits the field, so it could never exercise
    /// the missing-field path this whole rule is about.
    const PRE_UPGRADE_TRUST_JSON: &str = r#"[
  {
    "device": "pb-laptop00001",
    "fingerprint": "3f9a1b2c4d5e6f70a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718",
    "name": "laptop",
    "trusted_at": "2026-08-17T10:30:00Z",
    "approved": true
  },
  {
    "device": "pb-stranger001",
    "fingerprint": "77b2ccddeeff00112233445566778899aabbccddeeff00112233445566778899",
    "name": "Unknown Peer",
    "trusted_at": "2026-08-18T02:11:00Z",
    "approved": false
  }
]"#;

    fn store_with(path: &Path, json: &str) -> FsTrust {
        std::fs::write(path, json).unwrap();
        FsTrust::open(path).unwrap()
    }

    /// **The upgrade rule.** A device that worked before the upgrade must work
    /// after it. Reading the absent field as "no permissions" would silently
    /// stop this laptop's chat and transfers with no reason given; reading it
    /// as "everything" would hand it permissions added in later releases. It
    /// means *the permissions that existed when the record was written* —
    /// exactly the five — and slot 5, where a sixth would land, stays clear.
    #[test]
    fn a_pre_upgrade_record_keeps_the_five_and_gains_nothing_added_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(&path, PRE_UPGRADE_TRUST_JSON);

        let laptop = DeviceId::from("pb-laptop00001");
        for p in Permission::ALL
            .into_iter()
            .filter(|p| PermissionSet::granted_on_approval().grants(*p))
        {
            assert!(
                store.may(&laptop, p),
                "an approved pre-upgrade device must keep {p} across the upgrade"
            );
        }
        // ...and gains nothing assigned after the freeze. `Notes` is slot 5,
        // the first such permission, so this is the real case rather than the
        // hypothetical one the loop below covers.
        assert!(
            !store.may(&laptop, Permission::Notes),
            "a device nobody reviewed was handed a permission added later"
        );

        let permissions = store.lookup(&laptop).unwrap().unwrap().permissions;
        for slot in 5..32u8 {
            assert!(
                !permissions.grants_slot(slot),
                "slot {slot} — a permission introduced after this record was \
                 written — must not be granted to a device nobody reviewed"
            );
        }
        assert_eq!(
            permissions,
            PermissionSet::granted_on_approval(),
            "the legacy reading is the frozen default, not `all bits`"
        );
    }

    /// The same legacy file's *unapproved* record gains nothing: permissions
    /// narrow a standing, they never create one.
    #[test]
    fn a_pre_upgrade_stranger_is_still_permitted_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(&path, PRE_UPGRADE_TRUST_JSON);

        let stranger = DeviceId::from("pb-stranger001");
        assert!(store.is_trusted(&stranger), "it is pinned");
        for p in Permission::ALL {
            assert!(!store.may(&stranger, p), "but it may not {p}");
        }
    }

    /// A pin grants nothing on disk; approving is what writes the five.
    #[test]
    fn approving_grants_exactly_the_frozen_five() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-1");

        store.record(record("dev-1", "fp")).unwrap();
        assert_eq!(
            store.lookup(&id).unwrap().unwrap().permissions,
            PermissionSet::none(),
            "a pinned stranger's record must not read as a grant"
        );

        store.approve(&id).unwrap();
        assert_eq!(
            store.lookup(&id).unwrap().unwrap().permissions,
            PermissionSet::granted_on_approval()
        );
        for p in Permission::ALL {
            assert_eq!(
                store.may(&id, p),
                PermissionSet::granted_on_approval().grants(p),
                "{p}: approval granted something other than the frozen set"
            );
        }
    }

    /// Re-running `trust approve` on an already-approved device must not
    /// resurrect a permission the user deliberately revoked — a provisioning
    /// script is expected to be re-run.
    #[test]
    fn re_approving_does_not_undo_a_revoked_permission() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        store.approve(&id).unwrap();

        assert!(store
            .set_permission(&id, Permission::Clipboard, false)
            .unwrap());
        store.approve(&id).unwrap();
        assert!(!store.may(&id, Permission::Clipboard), "still revoked");
    }

    /// **Revocation applies to the next operation, not the next reconnect.**
    /// The gates re-read the store per operation, so the same store instance a
    /// running session holds must answer differently the instant the bit
    /// changes — no restart, no reconnect, no cache to invalidate.
    #[test]
    fn a_revoked_permission_is_refused_by_the_very_next_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        store.approve(&id).unwrap();
        assert!(store.may(&id, Permission::Clipboard));

        assert!(store
            .set_permission(&id, Permission::Clipboard, false)
            .unwrap());
        assert!(
            !store.may(&id, Permission::Clipboard),
            "the next query must already refuse"
        );
        for p in Permission::ALL
            .into_iter()
            .filter(|p| *p != Permission::Clipboard)
        {
            // Against what approval actually granted, not against every
            // permission this build knows: one assigned after the freeze is
            // absent for its own reason, and blaming the revocation for it
            // would make this test fail every time a permission is added.
            assert_eq!(
                store.may(&id, p),
                PermissionSet::granted_on_approval().grants(p),
                "revoking clipboard disturbed {p}"
            );
        }

        // And it survives a reopen, so the decision is durable rather than a
        // property of this process.
        let reopened = FsTrust::open(&path).unwrap();
        assert!(!reopened.may(&id, Permission::Clipboard));
        assert!(reopened.may(&id, Permission::Files));
    }

    /// Granting back is symmetric, and the return value distinguishes "changed"
    /// from "already like that" so a surface never reports a write it did not
    /// make.
    #[test]
    fn set_permission_reports_whether_it_changed_anything() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        store.approve(&id).unwrap();

        assert!(store.set_permission(&id, Permission::Pipe, false).unwrap());
        assert!(!store.set_permission(&id, Permission::Pipe, false).unwrap());
        assert!(store.set_permission(&id, Permission::Pipe, true).unwrap());
        assert!(store.may(&id, Permission::Pipe));
    }

    /// A device that is not pinned cannot be permitted — reported, not
    /// silently invented as a new record.
    #[test]
    fn permitting_an_unpinned_device_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let ghost = DeviceId::from("ghost");
        assert!(!store
            .set_permission(&ghost, Permission::Files, true)
            .unwrap());
        assert!(store.lookup(&ghost).unwrap().is_none());
    }

    /// Permitting is not approving. The store writes the bit; `may` still says
    /// no, because it implies approval.
    #[test]
    fn permitting_a_merely_pinned_device_does_not_approve_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();

        assert!(store.set_permission(&id, Permission::Files, true).unwrap());
        assert!(store
            .lookup(&id)
            .unwrap()
            .unwrap()
            .permissions
            .grants(Permission::Files));
        assert!(!store.is_approved(&id));
        assert!(
            !store.may(&id, Permission::Files),
            "the bit is not a standing"
        );
    }

    /// A store written by a **newer** build carries a permission name this one
    /// cannot enforce. It must still load — refusing to parse would take every
    /// pin on the machine with it — and the unknown grant is not honoured.
    #[test]
    fn a_record_from_a_newer_build_loads_and_its_unknown_grant_is_ignored() {
        // **Proved unknown, not assumed.** The placeholder here was `browse`
        // until shared folders shipped, at which point this test started
        // honouring the grant it exists to reject — and said nothing. A string
        // cannot notice that; this assertion does.
        const UNKNOWN: &str = "xyzzy-not-a-permission";
        assert!(
            Permission::parse(UNKNOWN).is_none(),
            "{UNKNOWN} is a real permission now — pick another placeholder"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(
            &path,
            r#"[{"device":"pb-future0001","fingerprint":"ab","name":"Future",
                 "trusted_at":"2026-08-18T02:11:00Z","approved":true,
                 "permissions":["files","xyzzy-not-a-permission","chat"]}]"#,
        );
        let id = DeviceId::from("pb-future0001");
        assert!(store.may(&id, Permission::Files));
        assert!(store.may(&id, Permission::Chat));
        assert!(!store.may(&id, Permission::Clipboard));
        assert_eq!(
            store.lookup(&id).unwrap().unwrap().permissions.bits(),
            Permission::Files.mask() | Permission::Chat.mask()
        );
    }

    /// An explicitly empty array is an empty set, not the legacy default: only
    /// an **absent** field means "the five that existed then".
    #[test]
    fn an_explicit_empty_permission_array_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(
            &path,
            r#"[{"device":"pb-locked00001","fingerprint":"ab","name":"Locked",
                 "trusted_at":"2026-08-18T02:11:00Z","approved":true,
                 "permissions":[]}]"#,
        );
        let id = DeviceId::from("pb-locked00001");
        assert!(store.is_approved(&id));
        for p in Permission::ALL {
            assert!(!store.may(&id, p));
        }
    }

    /// Permissions survive the write path, so what a gate reads after a restart
    /// is what the user chose.
    #[test]
    fn permissions_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let id = DeviceId::from("dev-1");
        {
            let store = FsTrust::open(&path).unwrap();
            store.record(record("dev-1", "fp")).unwrap();
            store.approve(&id).unwrap();
            store
                .set_permission(&id, Permission::Presence, false)
                .unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"permissions\""), "stored by name: {raw}");
        assert!(!raw.contains("\"presence\""), "and the revoked one is gone");

        let store = FsTrust::open(&path).unwrap();
        assert!(!store.may(&id, Permission::Presence));
        assert!(store.may(&id, Permission::Clipboard));
    }
}
