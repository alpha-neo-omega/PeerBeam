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
}

impl TrustRecord {
    /// What this device may **actually** do, right now.
    ///
    /// Empty unless [`approved`](Self::approved): permissions narrow a standing
    /// the user granted, they never create one. Keeping that rule here — rather
    /// than restating it at each gate and each surface — is what makes
    /// [`TrustStore::may`] and every listing agree by construction, so a pinned
    /// stranger can never be *shown* holding permissions it cannot use.
    ///
    /// It matters most for a **pre-upgrade** record: an unapproved one has no
    /// `permissions` key either, so it deserializes with the same default an
    /// approved one gets. That default is right for the record it was written
    /// for (an approved device keeps everything it had) and inert for the other
    /// (a stranger the handshake pinned may nothing), and this is where the
    /// second half is enforced instead of merely being true by luck.
    ///
    /// [`TrustStore::may`]: crate::port::TrustStore::may
    #[must_use]
    pub fn effective_permissions(&self) -> PermissionSet {
        if self.approved {
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
            trusted_at: chrono::Utc::now(),
            approved: false,
            permissions: PermissionSet::granted_on_approval(),
        };
        assert_eq!(record.effective_permissions(), PermissionSet::none());
        for p in Permission::ALL {
            assert!(!record.effective_permissions().grants(p));
        }
    }

    #[test]
    fn an_approved_records_effective_set_is_the_one_it_stores() {
        let mut record = parse(PRE_UPGRADE);
        record.permissions = record.permissions.set(Permission::Clipboard, false);
        assert_eq!(record.effective_permissions(), record.permissions);
        assert!(!record.effective_permissions().grants(Permission::Clipboard));
        assert!(record.effective_permissions().grants(Permission::Files));
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
}
