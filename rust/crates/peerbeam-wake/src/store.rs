//! Hardware addresses on disk, in the encrypted [`AppStore`].

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::AppStore;

use crate::error::WakeError;
use crate::mac::MacAddress;

/// The single namespace every recorded address lives in, keyed by device id.
///
/// One namespace rather than one per device, because a wake record is a single
/// small fact about a device and there is nothing to list *within* one. Keying
/// by device id is what ties the address to a peer the user has a trust
/// relationship with, which is the whole basis of [`may_wake`](crate::may_wake).
///
/// A device id is peer-supplied — the handshake in `peerbeam_transfer::auth`
/// takes it verbatim from the peer's own Hello — so it is used here as a
/// **key**, never as a namespace name. That is deliberate: `peerbeam-chat`
/// composes its namespaces from peer ids and had to reserve a separator to stop
/// a peer named `outbox` from colliding with its queue (see
/// `peerbeam_chat::OUTBOX_NS`). A constant namespace has no such surface. The
/// key itself is the store's problem, and `FsAppStore` already contains a
/// hostile key to its own root.
pub const NS: &str = "wake";

/// What this device knows about waking one other device.
///
/// Deliberately small. It holds the address and when the user recorded it, and
/// nothing that could be mistaken for a claim about the device's state — there
/// is no `last_woken_at`, because a magic packet produces no event to stamp one
/// from, and a field that looked like the time of a successful wake would be
/// recording the time of a *send*. See [`WakeAttempt`](crate::WakeAttempt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRecord {
    /// The device this address belongs to.
    pub device: DeviceId,
    /// Its hardware address, stored as canonical text — a `wake` record stays
    /// legible to the person whose machine it is, and an address that has been
    /// hand-edited into nonsense fails on read rather than becoming a packet
    /// that wakes nothing.
    pub mac: MacAddress,
    /// When the user recorded it.
    pub recorded_at: DateTime<Utc>,
}

/// Recorded hardware addresses, encrypted at rest by the [`AppStore`] beneath
/// (I11).
///
/// Encryption is not incidental here. A hardware address is a durable
/// identifier for a specific piece of hardware, and a plaintext list of the
/// addresses of every machine a person owns is a better inventory of them than
/// anything else on the disk.
#[derive(Clone)]
pub struct WakeStore {
    store: Arc<dyn AppStore>,
}

impl WakeStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>) -> Self {
        WakeStore { store }
    }

    /// Record (or replace) the hardware address for `device`.
    ///
    /// **This write is the user's consent to wake this device**, and the only
    /// way one is given — see [`may_wake`](crate::may_wake). Nothing infers an
    /// address from the wire; a caller reaching this has a person's typed or
    /// pasted input behind it.
    ///
    /// `now` is a parameter rather than a clock read here, following
    /// `TrustRecord::has_expired` and `peerbeam_transfer::is_expired`: it makes
    /// the round-trip exactly assertable instead of approximately, and this
    /// timestamp is a note for the user, never an input to a decision.
    pub fn remember(
        &self,
        device: &DeviceId,
        mac: MacAddress,
        now: DateTime<Utc>,
    ) -> Result<(), WakeError> {
        let record = WakeRecord {
            device: device.clone(),
            mac,
            recorded_at: now,
        };
        let bytes = serde_json::to_vec(&record).map_err(|e| WakeError::Storage(e.to_string()))?;
        self.store
            .put(NS, device.as_str(), &bytes)
            .map_err(|e| WakeError::Storage(e.to_string()))
    }

    /// The address recorded for `device`, or `None` if there is none.
    ///
    /// A record that is present but will not decode is an **error**, never a
    /// silent `None`. The two are worlds apart to a caller: `None` means "the
    /// user has not recorded an address" and leads to
    /// [`WakeError::NotRecorded`], which tells them to go and record one — over
    /// the top of an address that is already there. `peerbeam-chat` learned
    /// this the expensive way; see `ChatStore::take_pending_landing`.
    pub fn lookup(&self, device: &DeviceId) -> Result<Option<WakeRecord>, WakeError> {
        let Some(bytes) = self
            .store
            .get(NS, device.as_str())
            .map_err(|e| WakeError::Storage(e.to_string()))?
        else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| WakeError::Storage(format!("wake record for {device} is unreadable: {e}")))
    }

    /// Forget the address recorded for `device`, returning whether there was
    /// one. **This is the revocation** ([`may_wake`](crate::may_wake) leg 1):
    /// afterwards this machine cannot wake that device, and has nothing on disk
    /// saying how it ever could.
    pub fn forget(&self, device: &DeviceId) -> Result<bool, WakeError> {
        self.store
            .delete(NS, device.as_str())
            .map_err(|e| WakeError::Storage(e.to_string()))
    }

    /// Every recorded address, ascending by device id.
    ///
    /// A row this build cannot decode is **skipped with a warning**, not fatal
    /// — the opposite of [`lookup`](Self::lookup), and for the reason
    /// `ChatStore::history` gives: this is a listing, so containing the damage
    /// to one row saves every other row, whereas answering `None` for a single
    /// device is a statement about that device which happens to be false.
    ///
    /// Nothing here decides what to delete, so the strict-or-nothing rule that
    /// `ChatStore::outbox_owned_blobs` needs does not apply.
    pub fn all(&self) -> Result<Vec<WakeRecord>, WakeError> {
        let rows = self
            .store
            .list(NS)
            .map_err(|e| WakeError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for (key, value) in rows {
            match serde_json::from_slice::<WakeRecord>(&value) {
                Ok(rec) => out.push(rec),
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "skipping unreadable wake record");
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;

    /// A fresh store, returning the `WakeStore` alongside the raw handle and
    /// the `TempDir` that backs it. The raw handle lets a test write a value
    /// `WakeStore`'s own encode path could never produce — a deliberately
    /// undecodable row — which is the only way to exercise the two opposite
    /// reactions [`lookup`](WakeStore::lookup) and [`all`](WakeStore::all) have
    /// to one.
    fn new_store() -> (WakeStore, Arc<dyn AppStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> =
            Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (WakeStore::new(app.clone()), app, dir)
    }

    fn desktop() -> DeviceId {
        DeviceId::from("pb-desktop")
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// **The round trip.** What goes in comes back out, field for field —
    /// including the timestamp, which is why `remember` takes an explicit
    /// `now` rather than reading a clock a test can only approximate.
    #[test]
    fn a_recorded_address_comes_back_exactly() {
        let (wake, _raw, _dir) = new_store();
        let mac: MacAddress = "de:ad:be:ef:00:01".parse().unwrap();
        let now = at("2026-08-20T09:30:00Z");

        wake.remember(&desktop(), mac, now).unwrap();

        assert_eq!(
            wake.lookup(&desktop()).unwrap(),
            Some(WakeRecord {
                device: desktop(),
                mac,
                recorded_at: now,
            })
        );
    }

    #[test]
    fn a_device_with_nothing_recorded_looks_up_as_none() {
        let (wake, _raw, _dir) = new_store();
        assert_eq!(wake.lookup(&desktop()).unwrap(), None);
    }

    /// Records are per device: recording one machine's address must not answer
    /// for another's.
    #[test]
    fn each_device_keeps_its_own_address() {
        let (wake, _raw, _dir) = new_store();
        let laptop = DeviceId::from("pb-laptop");
        let now = at("2026-08-20T09:30:00Z");

        wake.remember(&desktop(), "de:ad:be:ef:00:01".parse().unwrap(), now)
            .unwrap();
        wake.remember(&laptop, "de:ad:be:ef:00:02".parse().unwrap(), now)
            .unwrap();

        assert_eq!(
            wake.lookup(&desktop()).unwrap().unwrap().mac.to_string(),
            "de:ad:be:ef:00:01"
        );
        assert_eq!(
            wake.lookup(&laptop).unwrap().unwrap().mac.to_string(),
            "de:ad:be:ef:00:02"
        );
    }

    /// Re-recording replaces rather than accumulating — a person who moved a
    /// network card or mistyped once has one address on disk, not two.
    #[test]
    fn recording_again_replaces_the_address() {
        let (wake, _raw, _dir) = new_store();
        let now = at("2026-08-20T09:30:00Z");
        let later = at("2026-08-20T10:00:00Z");

        wake.remember(&desktop(), "de:ad:be:ef:00:01".parse().unwrap(), now)
            .unwrap();
        wake.remember(&desktop(), "de:ad:be:ef:00:02".parse().unwrap(), later)
            .unwrap();

        let rec = wake.lookup(&desktop()).unwrap().unwrap();
        assert_eq!(rec.mac.to_string(), "de:ad:be:ef:00:02");
        assert_eq!(rec.recorded_at, later);
        assert_eq!(wake.all().unwrap().len(), 1);
    }

    /// **The revocation.** Forgetting says whether there was anything to
    /// forget, and afterwards the device is back to having no address at all —
    /// which is [`may_wake`](crate::may_wake)'s first leg closed.
    #[test]
    fn forgetting_removes_the_address_and_reports_whether_there_was_one() {
        let (wake, _raw, _dir) = new_store();
        let now = at("2026-08-20T09:30:00Z");
        wake.remember(&desktop(), "de:ad:be:ef:00:01".parse().unwrap(), now)
            .unwrap();

        assert!(wake.forget(&desktop()).unwrap(), "there was one to forget");
        assert_eq!(wake.lookup(&desktop()).unwrap(), None);
        assert!(
            !wake.forget(&desktop()).unwrap(),
            "forgetting again finds nothing"
        );
    }

    /// The listing is every record, ascending by device id (the store's own
    /// key order), so a surface renders a stable list.
    #[test]
    fn all_lists_every_record_by_device_id() {
        let (wake, _raw, _dir) = new_store();
        let now = at("2026-08-20T09:30:00Z");
        for (id, mac) in [
            ("pb-desktop", "de:ad:be:ef:00:01"),
            ("pb-attic", "de:ad:be:ef:00:02"),
            ("pb-nas", "de:ad:be:ef:00:03"),
        ] {
            wake.remember(&DeviceId::from(id), mac.parse().unwrap(), now)
                .unwrap();
        }

        let ids: Vec<String> = wake
            .all()
            .unwrap()
            .into_iter()
            .map(|r| r.device.0)
            .collect();
        assert_eq!(ids, vec!["pb-attic", "pb-desktop", "pb-nas"]);
    }

    /// **An unreadable record is not "no address recorded".** Folding the two
    /// together would tell the user to record an address on top of one that is
    /// already there, and would do it while the real fault — a corrupt or
    /// hand-mangled row — went unmentioned.
    #[test]
    fn an_unreadable_record_is_an_error_not_a_silent_absence() {
        let (wake, raw, _dir) = new_store();
        raw.put(NS, desktop().as_str(), b"not json at all").unwrap();

        let err = wake
            .lookup(&desktop())
            .expect_err("an unreadable record must not read as absent");
        assert!(
            matches!(&err, WakeError::Storage(m) if m.contains("pb-desktop")),
            "the error must name the device whose record failed: {err}"
        );
    }

    /// An address hand-edited into something that is no longer a hardware
    /// address fails the same way — the validation in `MacAddress` runs on the
    /// way in *and* on the way out, so a `00:00:00:00:00:00` typed into the
    /// file by hand cannot become a packet that wakes nothing.
    #[test]
    fn a_hand_edited_address_that_is_no_longer_valid_fails_on_read() {
        let (wake, raw, _dir) = new_store();
        raw.put(
            NS,
            desktop().as_str(),
            br#"{"device":"pb-desktop","mac":"00:00:00:00:00:00","recorded_at":"2026-08-20T09:30:00Z"}"#,
        )
        .unwrap();

        assert!(wake.lookup(&desktop()).is_err());
    }

    /// ...and the listing takes the opposite view of the same row: one bad
    /// record must not hide every good one, so it is skipped and the rest are
    /// returned. Both behaviours in one test, because it is the *contrast*
    /// that is the design.
    #[test]
    fn the_listing_skips_an_unreadable_row_rather_than_losing_the_list() {
        let (wake, raw, _dir) = new_store();
        wake.remember(
            &DeviceId::from("pb-laptop"),
            "de:ad:be:ef:00:09".parse().unwrap(),
            at("2026-08-20T09:30:00Z"),
        )
        .unwrap();
        raw.put(NS, desktop().as_str(), b"not json at all").unwrap();

        assert!(wake.lookup(&desktop()).is_err(), "the one row still errs");
        let all = wake.all().unwrap();
        assert_eq!(all.len(), 1, "the good row survives the bad one");
        assert_eq!(all[0].device, DeviceId::from("pb-laptop"));
    }

    /// The record is stored as legible JSON with the address in canonical
    /// text, so the file is something a person can read and correct.
    #[test]
    fn the_stored_record_is_legible_json() {
        let (wake, raw, _dir) = new_store();
        wake.remember(
            &desktop(),
            "DE-AD-BE-EF-00-01".parse().unwrap(),
            at("2026-08-20T09:30:00Z"),
        )
        .unwrap();

        let bytes = raw.get(NS, desktop().as_str()).unwrap().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.contains("\"de:ad:be:ef:00:01\""),
            "whatever notation was pasted, one canonical form is stored: {text}"
        );
    }
}
