//! Device trust entity (persisted, TOFU-style).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::PermissionSet;
use crate::id::DeviceId;

/// A record that a device's identity key has been trusted by the user.
///
/// Trust-on-first-use: the fingerprint is pinned on first pairing and
/// compared on every subsequent connection to detect key changes.
///
/// Pinning a key (MITM protection) is deliberately separate from being
/// *approved* for auto-accept: a device's key is pinned as soon as it is
/// first seen, regardless of whether the user accepts or declines the
/// transfer that triggered the handshake. Only an explicit accept should
/// let future connections skip the approval prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustRecord {
    /// The trusted device.
    pub device: DeviceId,
    /// Hex fingerprint of the device's long-term public key.
    pub fingerprint: String,
    /// Name the device presented when trusted.
    pub name: String,
    /// When trust was established.
    pub trusted_at: DateTime<Utc>,
    /// Whether the user has explicitly accepted a transfer from this device,
    /// making it eligible for auto-accept on future connections.
    ///
    /// `#[serde(default)]` so trust stores written before this field existed
    /// still load — those records deserialize as `approved: false`, requiring
    /// one more explicit approval after upgrading (fail-closed, not a
    /// silent auto-accept).
    #[serde(default)]
    pub approved: bool,
    /// **What** this device may do, refining the single bit above.
    ///
    /// `approved` says the user chose this device; this says which of that
    /// choice's powers they left it. It is only ever consulted together with
    /// `approved` — see [`TrustStore::may`], which is the one predicate that
    /// reads this field — so an unapproved device's set is inert.
    ///
    /// # The upgrade rule
    ///
    /// `#[serde(default = "PermissionSet::granted_on_approval")]`, and the
    /// default is the **frozen** five-permission set, not "none" and not "all":
    ///
    /// * *none* would silently revoke every working device the moment the user
    ///   upgraded — chat stops, transfers stop, and nothing says why. A record
    ///   written before this field means the permissions that existed **when it
    ///   was written**, so everything that worked before the upgrade still
    ///   works after it.
    /// * *all* would auto-grant permissions added in later releases to devices
    ///   nobody ever reviewed. Because the default enumerates fixed slots rather
    ///   than every bit, a permission introduced later is denied by default —
    ///   for legacy and newly approved records alike.
    ///
    /// Note this differs from `approved`'s own upgrade rule directly above, and
    /// deliberately: `approved` was a *new* power that no prior record could
    /// have consented to, so it defaults off. Permissions are a *narrowing* of a
    /// power a legacy record already held in full, so defaulting them off would
    /// take away what the user already granted. See `docs/SECURITY.md`.
    ///
    /// [`TrustStore::may`]: crate::port::TrustStore::may
    #[serde(default = "PermissionSet::granted_on_approval")]
    pub permissions: PermissionSet,
    /// When the user's grant runs out, if they put a clock on it.
    ///
    /// `None` — the ordinary case — is trust that holds until it is revoked.
    /// `Some(t)` is the answer to *"trust this device for 30 minutes"*: at and
    /// after `t` the record is worth exactly what it was before anybody
    /// approved it — a pin, granting nothing.
    ///
    /// # Enforced where it is read, never by a sweeper
    ///
    /// [`has_expired`](Self::has_expired) is consulted by the predicates every
    /// gate already asks ([`TrustStore::is_trusted`], [`TrustStore::is_approved`],
    /// [`TrustStore::may`]), so the window shuts on time whether or not anything
    /// has run since. A background cleaner would be a second source of truth and
    /// the slower one: a sweep that has not happened yet is a device still
    /// trusted after its window closed, and the interval between sweeps is
    /// exactly the interval an attacker wants.
    ///
    /// # The pin outlives the window
    ///
    /// Expiry ends the user's *grant*; it does not forget the key. The record
    /// stays on disk and [`TrustStore::lookup`] keeps returning it, which is
    /// what `peerbeam_transfer::auth` compares a presented fingerprint against.
    /// Deleting the record on expiry instead would turn a 30-minute window into
    /// a TOFU reset — the device's next handshake would pin whatever key
    /// answered, and a key change would no longer be detectable. Forgetting a
    /// device is what `revoke` is for.
    ///
    /// # The upgrade rule
    ///
    /// `#[serde(default)]`, so a store written before this field existed loads
    /// with `None`: trusted indefinitely, which is precisely what those records
    /// meant when they were written. Unlike `approved`, "absent" here is *not*
    /// the fail-closed direction and must not be made one — reading a missing
    /// deadline as "expired" would revoke every device on the machine the
    /// moment the user upgraded.
    ///
    /// `skip_serializing_if` keeps an indefinitely-trusted record byte-identical
    /// to what earlier builds wrote, so upgrading does not rewrite every line of
    /// a file whose devices nobody time-limited.
    ///
    /// [`TrustStore::lookup`]: crate::port::TrustStore::lookup
    /// [`TrustStore::is_trusted`]: crate::port::TrustStore::is_trusted
    /// [`TrustStore::is_approved`]: crate::port::TrustStore::is_approved
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The user's own note that this is **one of their machines** — their
    /// laptop, their phone, the desktop at work — so a surface can offer *"send
    /// to my laptop"* and cut a device list down to the handful that matter.
    ///
    /// # A label, not a claim, and not a grant
    ///
    /// Marking a device says nothing **to** it: no message is sent, nothing is
    /// negotiated, the peer neither learns nor sets this, and the device it
    /// describes may well be marked on one machine and not on the next. It is a
    /// word the user wrote about a record on their own disk.
    ///
    /// It therefore widens **nothing**. A device marked mine that nobody
    /// approved is still approved for nothing; one whose `permissions` withhold
    /// [`Permission::Browse`] still may not browse; one whose window has closed
    /// is still expired. Every permission answer in the workspace is computed by
    /// [`effective_permissions_at`](Self::effective_permissions_at), which does
    /// not read this field — and must never learn to. The moment a gate consults
    /// it, "mine" stops being a note and becomes a way to hand a device powers
    /// nobody ever approved it for, settable by anything that can write one bool
    /// into the local trust file. `marking_a_device_mine_grants_it_nothing`
    /// holds that shut as a property rather than as a promise.
    ///
    /// # The upgrade rule
    ///
    /// `#[serde(default)]`, so a store written before this field existed loads
    /// with `false`. Unlike `approved`, that is not the *fail-closed* direction
    /// — there is no permission here to fail closed about — it is the only
    /// **true** one. Nobody who wrote those records ever said a device was
    /// theirs, and defaulting to `true` would sweep every stranger the TOFU
    /// handshake pinned into the one list a user taps "send" on without reading
    /// it. Absent means *unsaid*, and unsaid is not mine.
    ///
    /// Note this is a different reason from [`expires_at`](Self::expires_at),
    /// where absent had to mean "no deadline" because the fail-closed reading
    /// would have revoked every device on the machine on upgrade. Neither
    /// direction revokes anything here, so the choice is settled by which
    /// statement is factually right rather than by which is safer.
    ///
    /// `skip_serializing_if` keeps an unmarked record byte-identical to what
    /// earlier builds wrote, so upgrading does not rewrite a file for a feature
    /// the user has not touched.
    ///
    /// [`Permission::Browse`]: crate::entity::Permission::Browse
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mine: bool,
}

impl TrustRecord {
    /// Whether this record's window has closed, as of `now`.
    ///
    /// `now` is a parameter rather than a clock read here — the same shape as
    /// `peerbeam_transfer::is_expired` — so the boundary is asserted exactly
    /// instead of by a test that sleeps and hopes. The layer that *is* asking
    /// about the present reads the clock: see [`TrustStore::may`].
    ///
    /// Expired **at** the deadline, not after it: a 30-minute window opened at
    /// 10:00 is over at 10:30:00, the same "invalid at or after" rule a resume
    /// token follows. A record with no deadline never expires.
    ///
    /// [`TrustStore::may`]: crate::port::TrustStore::may
    #[must_use]
    pub fn has_expired(&self, now: DateTime<Utc>) -> bool {
        matches!(self.expires_at, Some(deadline) if now >= deadline)
    }

    /// Whether the user's grant is still in force at `now`.
    ///
    /// [`approved`](Self::approved) records that the user chose this device;
    /// this adds the one thing that can end that choice with nobody doing
    /// anything — the window they attached to it running out. Every "may this
    /// device receive something of mine?" question resolves through here, so
    /// expiry cannot be honoured by one surface and forgotten by the next.
    #[must_use]
    pub fn is_approved_at(&self, now: DateTime<Utc>) -> bool {
        self.approved && !self.has_expired(now)
    }

    /// What this device may **actually** do, as of `now`.
    ///
    /// Empty unless [`is_approved_at`](Self::is_approved_at): permissions narrow
    /// a standing the user granted, they never create one, and a standing that
    /// has run out is not one. Keeping both rules here — rather than restating
    /// them at each gate and each surface — is what makes
    /// [`TrustStore::may`] and every listing agree by construction, so a pinned
    /// stranger, or a device whose half-hour is up, can never be *shown* holding
    /// permissions it cannot use.
    ///
    /// It matters most for a **pre-upgrade** record: an unapproved one has no
    /// `permissions` key either, so it deserializes with the same default an
    /// approved one gets. That default is right for the record it was written
    /// for (an approved device keeps everything it had) and inert for the other
    /// (a stranger the handshake pinned may nothing), and this is where the
    /// second half is enforced instead of merely being true by luck.
    ///
    /// Three fields decide this and no others: [`approved`](Self::approved),
    /// [`permissions`](Self::permissions), [`expires_at`](Self::expires_at).
    /// [`mine`](Self::mine) is deliberately not among them — see its own note
    /// for what adding it would cost.
    ///
    /// [`TrustStore::may`]: crate::port::TrustStore::may
    #[must_use]
    pub fn effective_permissions_at(&self, now: DateTime<Utc>) -> PermissionSet {
        if self.is_approved_at(now) {
            self.permissions
        } else {
            PermissionSet::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::Permission;
    use crate::id::DeviceId;

    /// A `trust.json` record exactly as a build before permissions wrote it.
    const PRE_UPGRADE: &str = r#"{
        "device": "pb-laptop00001",
        "fingerprint": "3f9a",
        "name": "laptop",
        "trusted_at": "2026-08-17T10:30:00Z",
        "approved": true
    }"#;

    fn parse(json: &str) -> TrustRecord {
        serde_json::from_str(json).expect("a pre-upgrade record must still load")
    }

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 8, 17, hour, minute, 0)
            .single()
            .expect("a real instant")
    }

    /// **The upgrade rule at the serde boundary.** An absent `permissions` key
    /// means the permissions that existed when the record was written.
    #[test]
    fn an_absent_permissions_field_means_the_frozen_five() {
        let record = parse(PRE_UPGRADE);
        assert_eq!(
            record.permissions,
            PermissionSet::granted_on_approval(),
            "not `none` (which would silently revoke a working device) and not \
             `all bits` (which would auto-grant whatever comes next)"
        );
        for slot in 5..32u8 {
            assert!(
                !record.permissions.grants_slot(slot),
                "slot {slot} — a permission added later — must stay clear"
            );
        }
    }

    /// An unapproved record deserializes the same way and is inert: it may
    /// nothing, and nothing may show it holding anything.
    #[test]
    fn an_unapproved_record_may_nothing_however_its_field_reads() {
        let record = TrustRecord {
            device: DeviceId::from("pb-stranger001"),
            fingerprint: "77b2".into(),
            name: "Unknown Peer".into(),
            trusted_at: at(10, 0),
            approved: false,
            permissions: PermissionSet::granted_on_approval(),
            expires_at: None,
            mine: false,
        };
        assert_eq!(
            record.effective_permissions_at(at(10, 1)),
            PermissionSet::none()
        );
        for p in Permission::ALL {
            assert!(!record.effective_permissions_at(at(10, 1)).grants(p));
        }
    }

    #[test]
    fn an_approved_records_effective_set_is_the_one_it_stores() {
        let mut record = parse(PRE_UPGRADE);
        record.permissions = record.permissions.set(Permission::Clipboard, false);
        let now = at(11, 0);
        assert_eq!(record.effective_permissions_at(now), record.permissions);
        assert!(!record
            .effective_permissions_at(now)
            .grants(Permission::Clipboard));
        assert!(record
            .effective_permissions_at(now)
            .grants(Permission::Files));
    }

    /// Round-trips through this build's own serializer with the field present.
    #[test]
    fn a_record_written_by_this_build_round_trips() {
        let record = parse(PRE_UPGRADE);
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains("\"permissions\""),
            "the field is written: {json}"
        );
        assert_eq!(serde_json::from_str::<TrustRecord>(&json).unwrap(), record);
    }

    // ── expiry ──────────────────────────────────────────────────────────────

    /// **The upgrade rule for the window.** A record written before expiry
    /// existed carries no deadline, and must therefore be trusted for as long
    /// as it always was. Defaulting it to "expired" — the direction `approved`
    /// defaults in — would revoke every device on the machine on upgrade.
    #[test]
    fn a_record_without_the_field_is_trusted_indefinitely() {
        let record = parse(PRE_UPGRADE);
        assert_eq!(record.expires_at, None);
        // Long after any window a person would have set, and long after the
        // record itself: with no deadline there is nothing to be past.
        for year in [2026, 2099, 9999] {
            use chrono::TimeZone;
            let far = Utc
                .with_ymd_and_hms(year, 12, 31, 23, 59, 0)
                .single()
                .unwrap();
            assert!(
                !record.has_expired(far),
                "a record with no deadline expired"
            );
            assert!(record.is_approved_at(far));
            assert_eq!(
                record.effective_permissions_at(far),
                PermissionSet::granted_on_approval()
            );
        }
    }

    /// **The boundary, asserted exactly.** A window that opens at 10:00 and runs
    /// for thirty minutes is over *at* 10:30 — not a tick later. `now` is
    /// passed in, so this is a statement about the predicate rather than about
    /// how long the test slept.
    #[test]
    fn a_window_closes_at_its_deadline_not_after_it() {
        let mut record = parse(PRE_UPGRADE);
        record.expires_at = Some(at(10, 30));

        assert!(!record.has_expired(at(10, 29)), "still inside the window");
        assert!(
            record.has_expired(at(10, 30)),
            "the deadline instant is already out — `>=`, not `>`"
        );
        assert!(record.has_expired(at(10, 31)));
    }

    /// **What expiry actually costs the device.** Not one permission — all of
    /// them, plus the standing they narrow. An expired record is worth exactly
    /// what it was before anyone approved it.
    #[test]
    fn an_expired_records_permissions_all_read_false() {
        let mut record = parse(PRE_UPGRADE);
        record.permissions = PermissionSet::granted_on_approval();
        record.expires_at = Some(at(10, 30));

        assert!(record.is_approved_at(at(10, 29)));
        for p in PermissionSet::granted_on_approval().granted() {
            assert!(
                record.effective_permissions_at(at(10, 29)).grants(p),
                "{p} must hold while the window is open"
            );
        }

        assert!(!record.is_approved_at(at(10, 30)));
        assert_eq!(
            record.effective_permissions_at(at(10, 30)),
            PermissionSet::none()
        );
        for p in Permission::ALL {
            assert!(
                !record.effective_permissions_at(at(10, 30)).grants(p),
                "an expired device may not {p}"
            );
        }
    }

    /// Expiry ends the grant; it does not edit the record. The stored
    /// permissions survive so that renewing an expired device restores what the
    /// user actually left it, rather than the frozen five.
    #[test]
    fn expiry_does_not_rewrite_what_the_user_granted() {
        let mut record = parse(PRE_UPGRADE);
        record.permissions = record.permissions.set(Permission::Clipboard, false);
        record.expires_at = Some(at(10, 30));
        let stored = record.permissions;

        assert_eq!(
            record.effective_permissions_at(at(11, 0)),
            PermissionSet::none()
        );
        assert_eq!(record.permissions, stored, "the grant is intact on disk");
        assert!(record.approved, "and so is the fact that it was approved");
    }

    /// A window on a device nobody approved grants nothing before it closes
    /// either — permissions narrow a standing, and an unapproved record has
    /// none to narrow whatever its dates say.
    #[test]
    fn a_window_on_an_unapproved_record_is_still_nothing() {
        let mut record = parse(PRE_UPGRADE);
        record.approved = false;
        record.expires_at = Some(at(23, 0));
        assert!(!record.is_approved_at(at(10, 0)));
        assert_eq!(
            record.effective_permissions_at(at(10, 0)),
            PermissionSet::none()
        );
    }

    /// A window round-trips, and an indefinite record still writes no key —
    /// so upgrading a store nobody time-limited leaves it as it was.
    #[test]
    fn a_window_round_trips_and_an_indefinite_record_writes_no_key() {
        let mut record = parse(PRE_UPGRADE);
        assert!(
            !serde_json::to_string(&record)
                .unwrap()
                .contains("expires_at"),
            "an indefinite record must not grow a key"
        );

        record.expires_at = Some(at(10, 30));
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains("\"expires_at\""),
            "a window is written: {json}"
        );
        assert_eq!(serde_json::from_str::<TrustRecord>(&json).unwrap(), record);
    }

    // ── "one of my devices" ─────────────────────────────────────────────────

    /// **The upgrade rule for the label.** A record written before it existed
    /// said nothing about whose device it is, so it loads saying nothing —
    /// `false`. Defaulting to `true` would put every stranger the handshake
    /// ever pinned into the user's "My devices" list.
    #[test]
    fn a_record_without_the_field_is_not_mine() {
        let record = parse(PRE_UPGRADE);
        assert!(!record.mine, "an unsaid label must not read as a claim");
    }

    /// **The property most likely to rot.** The label is a note the user wrote
    /// on their own disk; it must buy the device nothing. Every combination of
    /// approval, grant and window is checked twice — once unmarked, once
    /// marked, differing in that one field and nothing else — and both must
    /// answer identically. A gate that starts reading `mine` fails here.
    #[test]
    fn marking_a_device_mine_grants_it_nothing() {
        let now = at(11, 0);
        let windows = [None, Some(at(10, 30)), Some(at(23, 0))];
        let grants = [
            PermissionSet::none(),
            PermissionSet::granted_on_approval(),
            PermissionSet::granted_on_approval().set(Permission::Clipboard, false),
            Permission::ALL
                .into_iter()
                .fold(PermissionSet::none(), |set, p| set.set(p, true)),
        ];
        for approved in [false, true] {
            for permissions in grants {
                for expires_at in windows {
                    let unmarked = TrustRecord {
                        device: DeviceId::from("pb-laptop00001"),
                        fingerprint: "3f9a".into(),
                        name: "laptop".into(),
                        trusted_at: at(10, 0),
                        approved,
                        permissions,
                        expires_at,
                        mine: false,
                    };
                    // Struct-update syntax, so "differs only in `mine`" is a
                    // fact about the construction rather than a claim in a
                    // comment that a new field could quietly falsify.
                    let marked = TrustRecord {
                        mine: true,
                        ..unmarked.clone()
                    };

                    assert_eq!(
                        marked.effective_permissions_at(now),
                        unmarked.effective_permissions_at(now),
                        "marking mine changed the permission set \
                         (approved={approved}, expires_at={expires_at:?})"
                    );
                    assert_eq!(
                        marked.is_approved_at(now),
                        unmarked.is_approved_at(now),
                        "marking mine changed approval"
                    );
                    assert_eq!(
                        marked.has_expired(now),
                        unmarked.has_expired(now),
                        "marking mine changed the window"
                    );
                    for p in Permission::ALL {
                        assert_eq!(
                            marked.effective_permissions_at(now).grants(p),
                            unmarked.effective_permissions_at(now).grants(p),
                            "marking mine changed {p}"
                        );
                    }
                }
            }
        }
    }

    /// The label is not an approval in disguise either: a device the user calls
    /// theirs but has never accepted may nothing, which is the case a "send to
    /// my laptop" button will hit the first time somebody marks a fresh pin.
    #[test]
    fn a_device_that_is_mine_but_unapproved_may_nothing() {
        let mut record = parse(PRE_UPGRADE);
        record.approved = false;
        record.mine = true;
        assert!(record.mine);
        assert!(!record.is_approved_at(at(11, 0)));
        assert_eq!(
            record.effective_permissions_at(at(11, 0)),
            PermissionSet::none()
        );
    }

    /// The label round-trips, and an unmarked record still writes no key — so
    /// upgrading a store nobody has grouped leaves its bytes as they were.
    #[test]
    fn the_label_round_trips_and_an_unmarked_record_writes_no_key() {
        let mut record = parse(PRE_UPGRADE);
        assert!(
            !serde_json::to_string(&record).unwrap().contains("mine"),
            "an unmarked record must not grow a key"
        );

        record.mine = true;
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains("\"mine\":true"),
            "the label is written: {json}"
        );
        assert_eq!(serde_json::from_str::<TrustRecord>(&json).unwrap(), record);
    }
}
