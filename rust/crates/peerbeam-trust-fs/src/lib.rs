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

use chrono::{DateTime, Utc};
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

    /// Mark a pinned device as approved for auto-accept, indefinitely. Called
    /// only after the user explicitly accepts an incoming transfer from it — a
    /// declined transfer must never call this. A no-op (returning `Ok`) if the
    /// device isn't pinned; a device is always pinned before it can be approved.
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
        self.approve_for(device, None)
    }

    /// [`approve`](Self::approve) with a deadline: *"trust this device for 30
    /// minutes"*.
    ///
    /// `expires_at` is an **absolute instant**, not a duration, and it is the
    /// caller's `now + window`. Storing the instant is what lets the window be
    /// enforced by a read rather than by a countdown somebody has to keep
    /// running: a store reopened tomorrow reaches the same verdict as this
    /// process would, and a machine that was asleep through the whole window
    /// wakes up with it closed.
    ///
    /// `None` means indefinite, and it **clears** any window already on the
    /// record — that is what a plain `peerbeam trust approve` asks for. The
    /// deadline is always written, even for a device that is already approved,
    /// because sliding or lifting the window is the whole point of asking
    /// again; the *permissions* still are not, so re-approving cannot undo a
    /// revoke.
    pub fn approve_for(&self, device: &DeviceId, expires_at: Option<DateTime<Utc>>) -> Result<()> {
        self.approve_gated(device, true, expires_at)
    }

    /// [`approve_for`](Self::approve_for), refusing unless PIN pairing has been
    /// satisfied for this device.
    ///
    /// `pin_satisfied` is the caller's answer to "may this device be approved?"
    /// — false when `require_pin_pairing` is on and nobody has proved knowledge
    /// of the PIN. The check lives **here**, at the single place approval is
    /// written, rather than at each surface that offers a button: a gate that
    /// every caller has to remember is a gate that one of them eventually
    /// forgets.
    ///
    /// **Refuses rather than silently doing nothing.** An approval that quietly
    /// fails leaves a surface showing a device as trusted when it is not, which
    /// is a worse outcome than an error the user can read.
    pub fn approve_gated(
        &self,
        device: &DeviceId,
        pin_satisfied: bool,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        if !pin_satisfied {
            return Err(DomainError::Encryption(format!(
                "device {} cannot be approved: PIN pairing is required and has \
                 not been completed",
                device.0
            )));
        }
        let mut cache = self.cache.lock().unwrap();
        if let Some(record) = cache.get_mut(&device.0) {
            let mut changed = false;
            // Keyed on the stored bit, not on whether the grant is *live*: an
            // expired device is still one the user approved once, so renewing it
            // must restore the permissions they actually left it rather than
            // resurrecting the frozen five they had narrowed.
            if !record.approved {
                record.approved = true;
                record.permissions = PermissionSet::granted_on_approval();
                changed = true;
            }
            if record.expires_at != expires_at {
                record.expires_at = expires_at;
                changed = true;
            }
            if changed {
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

    /// Mark a pinned device as **one of the user's own**, or drop the mark.
    ///
    /// Returns whether a record was found and changed, so a surface can tell
    /// "done" from "that device is not pinned" and from "it was already like
    /// that" without a second lookup racing the first — the same three answers
    /// [`set_permission`](Self::set_permission) gives.
    ///
    /// **Writes one bool and nothing else.** It does not approve, does not
    /// touch [`TrustRecord::permissions`], and does not extend or clear a
    /// window: marking a device the user's own is a note they made about their
    /// own list, and a note that silently granted something would be the worst
    /// kind. `marking_a_device_mine_leaves_every_permission_alone` holds the
    /// three fields to what they were.
    ///
    /// It is refused for a device that is not pinned, rather than inventing a
    /// record: "these are my devices" is a statement about devices this machine
    /// has actually met.
    pub fn set_mine(&self, device: &DeviceId, mine: bool) -> Result<bool> {
        let mut cache = self.cache.lock().unwrap();
        let Some(record) = cache.get_mut(&device.0) else {
            return Ok(false);
        };
        if record.mine == mine {
            return Ok(false);
        }
        record.mine = mine;
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

    /// The record as stored — **expired or not**. The pin is a memory of a key
    /// and must survive the grant that was built on it: `peerbeam_transfer::auth`
    /// compares a presented fingerprint against this, so hiding an expired
    /// record here would let a device whose window closed re-pin any key it
    /// liked on its next handshake.
    fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>> {
        Ok(self.cache.lock().unwrap().get(&device.0).cloned())
    }

    /// [`list`](Self::list) narrowed to the marked records, so a "My devices"
    /// screen and the full trust list can never disagree about a device's name,
    /// order or grant — there is one read and one sort, filtered.
    ///
    /// Unfiltered by approval and by expiry, as the port specifies: this
    /// answers which devices are the user's, not which they may use.
    fn my_devices(&self) -> Result<Vec<TrustRecord>> {
        Ok(self.list().into_iter().filter(|r| r.mine).collect())
    }

    // `is_trusted` is deliberately **not** overridden. It used to be a
    // `contains_key`, which is now the wrong answer: a record whose window has
    // closed is present and not trusted. Inheriting `TrustStore`'s default
    // keeps the expiry rule in exactly one place instead of giving this store
    // its own copy to fall out of step with.
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
            expires_at: None,
            mine: false,
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

    /// **The gate refuses; it does not quietly do nothing.** An approval that
    /// silently fails leaves a surface showing a device as trusted when it is
    /// not, which is worse than an error somebody can read.
    #[test]
    fn approval_is_refused_while_pin_pairing_is_unsatisfied() {
        let dir = tempfile::tempdir().unwrap();
        let trust = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let device = DeviceId::from("pb-new");
        trust.record(record("pb-new", "fp")).unwrap();

        let err = trust
            .approve_gated(&device, false, None)
            .expect_err("an unsatisfied PIN must refuse approval");
        assert!(
            format!("{err}").contains("PIN pairing"),
            "the error does not say why: {err}"
        );
        assert!(
            !trust.lookup(&device).unwrap().unwrap().approved,
            "the device was approved despite the refusal"
        );
        assert!(
            !trust.may(&device, Permission::Files),
            "a refused approval still granted a permission"
        );
    }

    #[test]
    fn approval_proceeds_once_the_pin_is_satisfied() {
        let dir = tempfile::tempdir().unwrap();
        let trust = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let device = DeviceId::from("pb-new");
        trust.record(record("pb-new", "fp")).unwrap();

        trust.approve_gated(&device, true, None).unwrap();
        assert!(trust.lookup(&device).unwrap().unwrap().approved);
        assert!(trust.may(&device, Permission::Files));
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

    // ── time-limited trust ──────────────────────────────────────────────────

    /// Approve `device` with a window that closed an hour ago.
    ///
    /// Written as a **past** instant rather than a short future one that the
    /// test then waits out: a sleep makes the assertion a statement about the
    /// scheduler, and the whole point of enforcing expiry on read is that no
    /// time has to pass for it to take effect.
    fn approve_expired(store: &FsTrust, device: &DeviceId) {
        store
            .approve_for(device, Some(Utc::now() - chrono::Duration::hours(1)))
            .unwrap();
    }

    /// **Read time, not sweep time.** Nothing runs between approving and asking
    /// — no reaper, no reopen, no reconnect — and the store already refuses.
    #[test]
    fn a_closed_window_is_refused_by_the_very_next_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-timed");
        store.record(record("dev-timed", "fp")).unwrap();

        store
            .approve_for(&id, Some(Utc::now() + chrono::Duration::hours(1)))
            .unwrap();
        assert!(store.is_trusted(&id), "the window is open");
        assert!(store.is_approved(&id));
        assert!(store.may(&id, Permission::Files));

        approve_expired(&store, &id);
        assert!(!store.is_trusted(&id), "the window closed");
        assert!(!store.is_approved(&id));
        for p in Permission::ALL {
            assert!(!store.may(&id, p), "an expired device may not {p}");
        }
    }

    /// The verdict is a property of the file, not of the process that wrote it:
    /// a store reopened after the window closed reaches the same answer with
    /// nothing having run in between.
    #[test]
    fn a_closed_window_survives_a_reopen_without_any_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let id = DeviceId::from("dev-timed");
        {
            let store = FsTrust::open(&path).unwrap();
            store.record(record("dev-timed", "fp")).unwrap();
            approve_expired(&store, &id);
        }
        let reopened = FsTrust::open(&path).unwrap();
        assert!(!reopened.is_approved(&id));
        assert!(!reopened.may(&id, Permission::Files));
    }

    /// **The pin outlives the grant.** An expired record must still be found by
    /// `lookup`, because that is what `peerbeam_transfer::auth` compares a
    /// presented fingerprint against. Dropping it would make the next handshake
    /// a fresh first contact and pin whatever key answered.
    #[test]
    fn an_expired_record_still_holds_its_pin() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-timed");
        store.record(record("dev-timed", "fp-original")).unwrap();
        approve_expired(&store, &id);

        let stored = store
            .lookup(&id)
            .unwrap()
            .expect("the record is still there");
        assert_eq!(stored.fingerprint, "fp-original");
        assert!(stored.approved, "and it remembers that it was approved");
        assert!(store.list().iter().any(|r| r.device == id), "and it lists");
    }

    /// Re-approving an expired device renews it — and gives back the
    /// permissions the user actually left it, not the frozen five it started
    /// with. Resetting the set here would silently undo a revoke every time a
    /// window lapsed.
    #[test]
    fn renewing_an_expired_device_keeps_its_narrowed_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-timed");
        store.record(record("dev-timed", "fp")).unwrap();
        store.approve(&id).unwrap();
        assert!(store
            .set_permission(&id, Permission::Clipboard, false)
            .unwrap());

        approve_expired(&store, &id);
        assert!(!store.may(&id, Permission::Files), "the window closed");

        store.approve(&id).unwrap(); // plain `approve`: indefinite again
        assert!(store.may(&id, Permission::Files), "renewed");
        assert!(
            !store.may(&id, Permission::Clipboard),
            "and the revoke the user made still stands"
        );
        assert_eq!(store.lookup(&id).unwrap().unwrap().expires_at, None);
    }

    /// A plain `approve` on a device that currently has a window **lifts** it —
    /// that is what "approve, no `--for`" asks for — and a fresh `--for` slides
    /// it. Both must be written even though the device is already approved.
    #[test]
    fn approving_again_rewrites_the_window_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-timed");
        store.record(record("dev-timed", "fp")).unwrap();

        let soon = Utc::now() + chrono::Duration::minutes(30);
        store.approve_for(&id, Some(soon)).unwrap();
        assert_eq!(store.lookup(&id).unwrap().unwrap().expires_at, Some(soon));

        let later = Utc::now() + chrono::Duration::hours(8);
        store.approve_for(&id, Some(later)).unwrap();
        assert_eq!(store.lookup(&id).unwrap().unwrap().expires_at, Some(later));

        store.approve(&id).unwrap();
        assert_eq!(
            store.lookup(&id).unwrap().unwrap().expires_at,
            None,
            "a plain approve means indefinitely"
        );
    }

    /// A window is only written by approval, never by a pin: the handshake
    /// records a stranger with nothing for a clock to end.
    #[test]
    fn a_pin_carries_no_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        assert_eq!(store.lookup(&id).unwrap().unwrap().expires_at, None);
        assert!(store.is_trusted(&id), "a pin with no window is a live pin");
    }

    /// **A `trust.json` exactly as the build before this one wrote it**: an
    /// approved device with `permissions` and no `expires_at`. It must load,
    /// and it must stay trusted — reading the absent deadline as "expired"
    /// would revoke every device on the machine the moment the user upgraded.
    #[test]
    fn a_record_written_before_expiry_existed_is_trusted_indefinitely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(
            &path,
            r#"[
  {
    "device": "pb-laptop00001",
    "fingerprint": "3f9a1b2c4d5e6f70",
    "name": "laptop",
    "trusted_at": "2026-08-17T10:30:00Z",
    "approved": true,
    "permissions": ["files", "chat", "clipboard", "presence", "pipe"]
  }
]"#,
        );
        let id = DeviceId::from("pb-laptop00001");
        assert_eq!(store.lookup(&id).unwrap().unwrap().expires_at, None);
        assert!(store.is_trusted(&id));
        assert!(store.is_approved(&id));
        for p in PermissionSet::granted_on_approval().granted() {
            assert!(store.may(&id, p), "{p} must survive the upgrade");
        }
    }

    /// A store with nothing time-limited is written exactly as it was before
    /// this field existed, so upgrading does not churn a file whose devices
    /// nobody put a clock on — and an older build reads it back unchanged.
    #[test]
    fn an_indefinite_record_writes_no_expiry_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        store.approve(&id).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("expires_at"),
            "an indefinitely-trusted store grew a key it does not need"
        );

        store
            .approve_for(&id, Some(Utc::now() + chrono::Duration::minutes(30)))
            .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("expires_at"),
            "but a window must be on disk, or a restart would forget it"
        );
    }

    // ── "one of my devices" ─────────────────────────────────────────────────

    /// The mark is written, survives a restart, is reported as a change only
    /// when it is one, and comes off again — a device sold or re-purposed
    /// stops being the user's.
    #[test]
    fn marking_a_device_mine_persists_and_unmarking_undoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        assert!(!store.is_mine(&id), "a fresh pin says nothing about whose");

        assert!(store.set_mine(&id, true).unwrap(), "marked");
        assert!(!store.set_mine(&id, true).unwrap(), "already like that");
        assert!(store.is_mine(&id));

        // Read back from disk, not from the cache that just wrote it.
        let reopened = FsTrust::open(&path).unwrap();
        assert!(reopened.is_mine(&id), "the mark must survive a restart");

        assert!(reopened.set_mine(&id, false).unwrap(), "unmarked");
        assert!(!reopened.is_mine(&id));
        assert!(!FsTrust::open(&path).unwrap().is_mine(&id));
    }

    /// A device this machine has never met cannot be one of the user's —
    /// reported as "nothing changed" rather than invented as a new record.
    #[test]
    fn marking_an_unpinned_device_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsTrust::open(dir.path().join("trust.json")).unwrap();
        let ghost = DeviceId::from("dev-never-seen");
        assert!(!store.set_mine(&ghost, true).unwrap());
        assert!(store.lookup(&ghost).unwrap().is_none());
        assert!(!store.is_mine(&ghost));
    }

    /// **The property most likely to rot, at the write path.** Marking must
    /// move one bool: not approval, not the grant, not the window — and not
    /// one `may` answer. Asserted against the record as it was, so a `set_mine`
    /// that "helpfully" approved, or a gate that started reading the label,
    /// fails here rather than in somebody's file listing.
    #[test]
    fn marking_a_device_mine_leaves_every_permission_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-laptop");
        store.record(record("dev-laptop", "fp")).unwrap();
        let window = Utc::now() + chrono::Duration::minutes(30);
        store.approve_for(&id, Some(window)).unwrap();
        store
            .set_permission(&id, Permission::Clipboard, false)
            .unwrap();

        let before = store.lookup(&id).unwrap().unwrap();
        let permitted_before: Vec<bool> =
            Permission::ALL.iter().map(|p| store.may(&id, *p)).collect();

        assert!(store.set_mine(&id, true).unwrap());

        let after = store.lookup(&id).unwrap().unwrap();
        assert!(after.mine, "precondition: the label did get written");
        assert_eq!(
            after.permissions, before.permissions,
            "marking mine rewrote the grant"
        );
        assert_eq!(after.approved, before.approved, "marking mine approved it");
        assert_eq!(
            after.expires_at, before.expires_at,
            "marking mine moved the window"
        );
        assert_eq!(
            after,
            TrustRecord {
                mine: true,
                ..before
            },
            "marking mine changed a field other than the label"
        );
        for (p, was) in Permission::ALL.into_iter().zip(permitted_before) {
            assert_eq!(store.may(&id, p), was, "marking mine changed {p}");
        }

        // ...and taking the mark off is equally inert.
        assert!(store.set_mine(&id, false).unwrap());
        assert_eq!(
            store.lookup(&id).unwrap().unwrap(),
            TrustRecord {
                mine: false,
                ..after
            },
            "unmarking must be exactly as inert as marking"
        );
    }

    /// **The upgrade rule.** The trust file every existing user has says
    /// nothing about whose device is whose, and must load saying nothing —
    /// not sweep two strangers into the list the user taps "send" on.
    #[test]
    fn a_record_written_before_the_label_existed_loads_unmarked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = store_with(&path, PRE_UPGRADE_TRUST_JSON);

        for id in ["pb-laptop00001", "pb-stranger001"] {
            let id = DeviceId::from(id);
            assert!(!store.lookup(&id).unwrap().unwrap().mine);
            assert!(!store.is_mine(&id), "{} was claimed by nobody", id.0);
        }
        assert!(
            store.my_devices().unwrap().is_empty(),
            "an upgraded store starts with an empty My devices list"
        );
        // The upgrade did not cost the laptop anything either.
        assert!(store.is_approved(&DeviceId::from("pb-laptop00001")));
    }

    /// The filtered read: only marked records, in `list`'s order, and
    /// **unfiltered by approval** — a phone the user marked but has not yet
    /// accepted still belongs on their own list, or they will conclude it is
    /// missing when it is merely waiting for a tap.
    #[test]
    fn my_devices_lists_the_marked_records_approved_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();

        // Distinct `trusted_at`s, oldest first, so the newest-first order is a
        // real assertion rather than whatever the map happened to yield.
        for (i, id) in ["dev-desktop", "dev-phone", "dev-stranger"]
            .into_iter()
            .enumerate()
        {
            let mut r = record(id, "fp");
            r.trusted_at = Utc::now() + chrono::Duration::seconds(i as i64);
            store.record(r).unwrap();
        }
        let desktop = DeviceId::from("dev-desktop");
        let phone = DeviceId::from("dev-phone");
        store.approve(&desktop).unwrap();
        store.set_mine(&desktop, true).unwrap();
        store.set_mine(&phone, true).unwrap(); // marked, never approved

        let mine: Vec<String> = store
            .my_devices()
            .unwrap()
            .into_iter()
            .map(|r| r.device.0)
            .collect();
        assert_eq!(mine, vec!["dev-phone", "dev-desktop"], "newest first");
        assert!(
            !store.is_approved(&phone),
            "and listing the phone did not approve it"
        );
        assert_eq!(store.list().len(), 3, "the full list is untouched");
    }

    /// A store where nobody has grouped anything is written exactly as it was
    /// before this field existed, so upgrading does not churn a file for a
    /// feature the user has not touched — and an older build reads it back.
    #[test]
    fn an_unmarked_store_writes_no_mine_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FsTrust::open(&path).unwrap();
        let id = DeviceId::from("dev-1");
        store.record(record("dev-1", "fp")).unwrap();
        store.approve(&id).unwrap();
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("mine"),
            "an unmarked store grew a key it does not need"
        );

        store.set_mine(&id, true).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"mine\": true"),
            "but a mark must be on disk, or a restart would forget it"
        );
    }
}
