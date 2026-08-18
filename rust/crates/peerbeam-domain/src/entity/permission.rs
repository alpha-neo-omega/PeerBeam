//! Per-device permissions: *what* a device the user approved may actually do.
//!
//! # Why this exists
//!
//! Approval used to be one bit that meant **everything**: a device the user
//! accepted once could be sent this machine's presence status, its clipboard,
//! an accepted pipe, and every transfer, for as long as the record survived.
//! There was no way to say *"this laptop may sync files but must never read my
//! clipboard"*.
//!
//! [`VISION.md`] permits remote capabilities only as "explicit, **permissioned**,
//! narrowly-scoped actions", and I6 requires "explicit, revocable,
//! **per-capability** consent". That is this type: a per-device grant list,
//! stored next to the approval it refines.
//!
//! # Why `Permission` and not `Capability`
//!
//! "Capability" already means two different things in this workspace — a
//! negotiated wire channel ([`crate::session::Capability`]) and a device's
//! reachability ([`crate::entity::DeviceCapabilities`]) — plus a third,
//! architectural one in I2. A third *runtime* meaning inside `peerbeam-domain`
//! would make every one of them harder to read. These are permissions: local
//! policy about a peer, never negotiated with it and never on the wire.
//!
//! # The slot model, and the upgrade rule it exists to express
//!
//! Each permission owns a **slot**: a small integer that is assigned once and
//! never reused, even if a permission is one day retired. A [`PermissionSet`]
//! is the bitmap of the slots it grants.
//!
//! Slots exist so that *"a permission added later is denied by default"* is a
//! property of the data rather than a promise in a comment.
//! [`PermissionSet::granted_on_approval`] is a **frozen** constant naming
//! exactly the five permissions that existed when this field was introduced, so
//! a slot allocated afterwards is clear in it by construction — and
//! [`PermissionSet::grants_slot`] lets a test assert that against a slot no
//! [`Permission`] variant occupies yet, without inventing a fake variant to
//! prove it with.
//!
//! [`VISION.md`]: https://github.com/peerbeam/peerbeam/blob/main/docs/VISION.md

use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One thing a device may be permitted to do.
///
/// Deliberately limited to features that **exist today**. A permission whose
/// feature has not been built cannot be tested, and would be the wrong shape by
/// the time it were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    /// Send and receive file transfers.
    Files,
    /// Exchange chat messages.
    Chat,
    /// Receive this machine's clipboard while clipboard sync is on.
    Clipboard,
    /// Receive this machine's device-status heartbeat while sharing is on.
    Presence,
    /// Have an inbound `peerbeam pipe` accepted by a listening terminal.
    Pipe,
}

impl Permission {
    /// Every permission this build knows, in slot order.
    ///
    /// Used to render a device's grants and to drive the "each permission gates
    /// its own feature" tests. It is **not** the legacy default and must never
    /// be used as one — see [`PermissionSet::granted_on_approval`].
    pub const ALL: [Permission; 5] = [
        Permission::Files,
        Permission::Chat,
        Permission::Clipboard,
        Permission::Presence,
        Permission::Pipe,
    ];

    /// This permission's permanent bit position.
    ///
    /// **Assigned once, never reused.** A retired permission's slot stays
    /// retired: reusing it would silently hand a new power to every device that
    /// still holds the old grant on disk.
    #[must_use]
    pub const fn slot(self) -> u8 {
        match self {
            Permission::Files => 0,
            Permission::Chat => 1,
            Permission::Clipboard => 2,
            Permission::Presence => 3,
            Permission::Pipe => 4,
        }
    }

    /// The single-bit mask for this permission's [`slot`](Self::slot).
    #[must_use]
    pub const fn mask(self) -> u32 {
        1u32 << self.slot()
    }

    /// The stable name used on disk, in `--json`, and on the CLI.
    ///
    /// Stored as a **name**, not as a number, so a `trust.json` stays legible to
    /// the person whose machine it is and so a renumbering can never silently
    /// re-point a grant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Permission::Files => "files",
            Permission::Chat => "chat",
            Permission::Clipboard => "clipboard",
            Permission::Presence => "presence",
            Permission::Pipe => "pipe",
        }
    }

    /// Parse a stored or typed name. ASCII-case-insensitive so `FILES` typed at
    /// a shell resolves, but no aliases and no prefixes: a mistyped permission
    /// must be an error, never a different grant.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Permission::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(name))
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The set of permissions granted to one device.
///
/// A bitmap of [`Permission::slot`]s. [`Default`] is **empty** — a set nobody
/// filled in grants nothing, which is the fail-closed direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PermissionSet(u32);

impl PermissionSet {
    /// What a device is granted when the user approves it — and what a record
    /// written **before this field existed** is taken to mean.
    ///
    /// # This list is frozen
    ///
    /// It names the five permissions that existed when `permissions` was
    /// introduced, and **must never be added to**. That single rule resolves
    /// both halves of the upgrade problem:
    ///
    /// * Reading a pre-upgrade record as *"no permissions"* would silently
    ///   revoke every working device the moment the user upgraded — chat stops,
    ///   transfers stop, and nothing says why. So a legacy record means the
    ///   permissions that existed when it was written, and every device that
    ///   worked before the upgrade keeps working after it.
    /// * Reading it as *"all permissions"* is the mirror danger: a permission
    ///   added in some later release would be auto-granted to devices nobody
    ///   ever reviewed. Because this constant enumerates five fixed slots rather
    ///   than "every bit", a slot allocated later is clear in it by
    ///   construction — for legacy and for newly approved records alike, since
    ///   both start from here.
    ///
    /// A permission added later is therefore **opt-in, always**: it is granted
    /// only by an explicit `peerbeam trust permit` (or the app's toggle), which
    /// is precisely the "explicit, permissioned, narrowly-scoped" consent
    /// `VISION.md` requires. `a_permission_added_later_is_not_granted_by_the_default`
    /// pins the constant so that adding to it fails the build's tests rather
    /// than quietly widening what an unreviewed device may do.
    #[must_use]
    pub const fn granted_on_approval() -> Self {
        PermissionSet(
            Permission::Files.mask()
                | Permission::Chat.mask()
                | Permission::Clipboard.mask()
                | Permission::Presence.mask()
                | Permission::Pipe.mask(),
        )
    }

    /// The empty set: grants nothing.
    #[must_use]
    pub const fn none() -> Self {
        PermissionSet(0)
    }

    /// Whether this set grants `permission`.
    #[must_use]
    pub const fn grants(self, permission: Permission) -> bool {
        self.0 & permission.mask() != 0
    }

    /// Whether this set grants whatever occupies `slot`.
    ///
    /// The raw form of [`grants`](Self::grants). It exists so that *"a
    /// permission introduced after this record was written is denied"* can be
    /// **asserted** — against a slot no [`Permission`] variant occupies yet —
    /// instead of being asserted about a fake sixth variant added to the enum
    /// purely to have something to test with. A slot beyond the width of the
    /// bitmap is likewise not granted.
    #[must_use]
    pub const fn grants_slot(self, slot: u8) -> bool {
        slot < u32::BITS as u8 && self.0 & (1u32 << slot) != 0
    }

    /// This set with `permission` granted or withheld.
    ///
    /// Returns a new set rather than mutating: a permission change is persisted
    /// as a whole record, so there is no window in which a half-applied set is
    /// visible to a gate.
    #[must_use]
    pub const fn set(self, permission: Permission, granted: bool) -> Self {
        if granted {
            PermissionSet(self.0 | permission.mask())
        } else {
            PermissionSet(self.0 & !permission.mask())
        }
    }

    /// The permissions granted, in slot order.
    ///
    /// Slot order (not alphabetical) so a listing reads the same everywhere and
    /// a new permission appears in a stable place.
    #[must_use]
    pub fn granted(self) -> Vec<Permission> {
        Permission::ALL
            .into_iter()
            .filter(|p| self.grants(*p))
            .collect()
    }

    /// The raw bitmap. For tests and diagnostics; the stored form is names.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// Serialized as an **array of names** — `["files","chat",…]` — so a
/// `trust.json` says what it grants to anyone who opens it, and so `--json`
/// consumers get the explicit array they filter on rather than a number to
/// decode.
impl Serialize for PermissionSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let granted = self.granted();
        let mut seq = serializer.serialize_seq(Some(granted.len()))?;
        for p in granted {
            seq.serialize_element(p.as_str())?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for PermissionSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Names;

        impl<'de> Visitor<'de> for Names {
            type Value = PermissionSet;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an array of permission names")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<PermissionSet, A::Error> {
                let mut set = PermissionSet::none();
                while let Some(name) = seq.next_element::<String>()? {
                    // A name this build does not know is **ignored, not an
                    // error**: it is a permission from a newer release, and a
                    // store written by one must still load here rather than
                    // failing to parse and taking every pin with it. Ignoring
                    // it is also the fail-closed reading — an unknown grant is
                    // not honoured by a build that cannot enforce it.
                    if let Some(p) = Permission::parse(&name) {
                        set = set.set(p, true);
                    }
                }
                Ok(set)
            }
        }

        deserializer.deserialize_seq(Names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The frozen constant.** Adding a permission to
    /// [`PermissionSet::granted_on_approval`] would auto-grant it to every
    /// device already on disk — legacy records and previously approved ones
    /// alike — without anyone reviewing them. This test is what makes that a
    /// build failure instead of a silent widening.
    #[test]
    fn the_default_grant_is_frozen_to_the_five_that_existed() {
        assert_eq!(
            PermissionSet::granted_on_approval().bits(),
            0b0001_1111,
            "granted_on_approval must stay exactly Files|Chat|Clipboard|Presence|Pipe"
        );
    }

    /// **The upgrade rule, stated against a slot nothing occupies yet.** Slot 5
    /// is where a sixth permission would land. It must be clear in the default
    /// grant, so neither a pre-upgrade record nor a newly approved device picks
    /// up a power added after it was reviewed.
    #[test]
    fn a_permission_added_later_is_not_granted_by_the_default() {
        let default = PermissionSet::granted_on_approval();
        for slot in 5..32u8 {
            assert!(
                !default.grants_slot(slot),
                "slot {slot} — a permission added later — must not be granted by default"
            );
        }
    }

    /// Slots are permanent: this pins the assignment so a reordering of the
    /// enum cannot silently re-point every grant on every machine.
    #[test]
    fn slots_are_fixed_and_distinct() {
        assert_eq!(Permission::Files.slot(), 0);
        assert_eq!(Permission::Chat.slot(), 1);
        assert_eq!(Permission::Clipboard.slot(), 2);
        assert_eq!(Permission::Presence.slot(), 3);
        assert_eq!(Permission::Pipe.slot(), 4);
        let mut seen = 0u32;
        for p in Permission::ALL {
            assert_eq!(seen & p.mask(), 0, "{p} reuses a slot");
            seen |= p.mask();
        }
    }

    #[test]
    fn an_empty_set_grants_nothing_and_is_the_default() {
        let empty = PermissionSet::default();
        assert_eq!(empty, PermissionSet::none());
        for p in Permission::ALL {
            assert!(!empty.grants(p), "{p} must not be granted by an empty set");
        }
    }

    /// Setting and clearing one permission leaves the others exactly as they
    /// were — the property the whole model rests on.
    #[test]
    fn setting_one_permission_does_not_disturb_the_others() {
        let all = PermissionSet::granted_on_approval();
        for target in Permission::ALL {
            let without = all.set(target, false);
            assert!(!without.grants(target));
            for other in Permission::ALL.into_iter().filter(|p| *p != target) {
                assert!(
                    without.grants(other),
                    "revoking {target} must not disturb {other}"
                );
            }
            assert_eq!(without.set(target, true), all, "and it is reversible");
        }
    }

    #[test]
    fn names_round_trip_through_json_as_an_array() {
        let set = PermissionSet::none()
            .set(Permission::Files, true)
            .set(Permission::Pipe, true);
        let json = serde_json::to_value(set).unwrap();
        assert_eq!(json, serde_json::json!(["files", "pipe"]));
        assert_eq!(
            serde_json::from_value::<PermissionSet>(json).unwrap(),
            set,
            "the stored form must read back as the same set"
        );
    }

    /// A store written by a newer build carries a name this one cannot enforce.
    /// It must load — failing to parse would take every pin on the machine with
    /// it — and the unknown grant must not be honoured.
    #[test]
    fn an_unknown_permission_name_loads_and_is_not_honoured() {
        let set: PermissionSet =
            serde_json::from_value(serde_json::json!(["files", "browse", "chat"])).unwrap();
        assert!(set.grants(Permission::Files));
        assert!(set.grants(Permission::Chat));
        assert_eq!(
            set.bits(),
            Permission::Files.mask() | Permission::Chat.mask()
        );
    }

    #[test]
    fn an_empty_array_is_an_empty_set_not_a_default() {
        let set: PermissionSet = serde_json::from_value(serde_json::json!([])).unwrap();
        assert_eq!(set, PermissionSet::none());
    }

    /// Parsing is exact (case aside). A typo must be rejected, never rounded to
    /// a neighbouring grant.
    #[test]
    fn parsing_is_exact_and_case_insensitive() {
        assert_eq!(Permission::parse("clipboard"), Some(Permission::Clipboard));
        assert_eq!(Permission::parse("CLIPBOARD"), Some(Permission::Clipboard));
        assert_eq!(Permission::parse("clip"), None);
        assert_eq!(Permission::parse("clipboards"), None);
        assert_eq!(Permission::parse(""), None);
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn granted_lists_in_slot_order() {
        assert_eq!(
            PermissionSet::granted_on_approval().granted(),
            Permission::ALL.to_vec()
        );
    }
}
