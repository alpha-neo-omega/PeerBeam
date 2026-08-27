//! The Space record, the rules a name and a member id must satisfy, and the one
//! place a recorded membership is compared against trust.

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// Maximum space name, in UTF-8 bytes.
///
/// Generous next to any label a person types onto a handful of devices, and
/// bounded because the name is echoed into log lines, CLI tables and FFI
/// events. The same reasoning as `peerbeam_notes::MAX_TITLE`.
pub const MAX_NAME: usize = 128;

/// Maximum device id accepted as a member, in bytes. Same bound as a chat
/// message id, and for the same reason: an id is echoed into events, storage
/// keys and log lines.
pub const MAX_DEVICE_ID: usize = 128;

/// Every way a space operation can be refused, each naming what was wrong.
///
/// One variant per cause rather than a single `Invalid(String)`: a surface that
/// wants to say *"pair with that device first"* and one that wants to say
/// *"pick another name"* need to tell those apart without matching on prose.
#[derive(Debug, thiserror::Error)]
pub enum SpaceError {
    /// The name was empty, or nothing but whitespace.
    #[error("a space needs a name: {0:?} is empty or only whitespace")]
    EmptyName(String),

    /// The name was longer than [`MAX_NAME`].
    #[error("space name too long: {len} bytes (max {MAX_NAME})")]
    NameTooLong { len: usize },

    /// The name held a character that cannot be rendered honestly.
    #[error(
        "space name contains a character that cannot be displayed safely \
         (control or bidi override at char {at})"
    )]
    UndisplayableName { at: usize },

    /// Another space already answers to this name.
    #[error(
        "a space named {existing:?} already exists, so {wanted:?} would be \
         ambiguous (names are compared ignoring case and surrounding spaces)"
    )]
    NameTaken { wanted: String, existing: String },

    /// The member id could not be a device id whatever device it named.
    #[error("{id:?} is not a usable device id: {reason}")]
    BadMember { id: String, reason: String },

    /// The member id is well-formed but names a device this machine has never
    /// trusted — a typo, or a device that was revoked.
    #[error(
        "no device {id} is trusted on this machine, so it cannot be added to a \
         space — pair with it first"
    )]
    UnknownMember { id: String },

    /// The trust store could not be read, so membership could not be checked.
    /// Refused rather than assumed either way; see [`SpaceError::UnknownMember`].
    #[error("cannot tell whether {id} is trusted: {reason}")]
    TrustUnreadable { id: String, reason: String },

    /// No space has this id.
    #[error("no space with id {0}")]
    NotFound(String),

    /// The store beneath failed.
    #[error("space storage: {0}")]
    Storage(String),

    /// A record could not be encoded or decoded.
    #[error("space serialization: {0}")]
    Serialization(String),
}

/// One space, exactly as it sits in the store.
///
/// # `members` is what was recorded, not who is still trusted
///
/// Nothing prunes this list — see [`Space::view`] — so reading it directly is
/// reading history, not the present. Every store read hands back a
/// [`SpaceView`] instead, which cannot be constructed without answering the
/// trust question, so no surface can accidentally show a revoked device as a
/// live member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    /// Opaque, stable, local-only. Survives a rename, which is the whole reason
    /// the name is not the key.
    pub id: String,
    /// What the user typed, trimmed. Unique across this device's spaces, after
    /// [`normalise`].
    pub name: String,
    /// Members in the order they were added.
    ///
    /// `#[serde(default)]` because a space with no members is the ordinary
    /// state for the moment between `create` and the first `add_member`: an
    /// absent list means that same thing, rather than an unreadable row.
    #[serde(default)]
    pub members: Vec<DeviceId>,
}

impl Space {
    /// This space as it stands **right now**, with every recorded member sorted
    /// into one it can still reach and one it cannot.
    ///
    /// # Read-time enforcement, not a prune on write
    ///
    /// A membership does not stop being true because somebody wrote to this
    /// store. Two of the three ways a member goes stale involve no write here
    /// at all: the user revokes the device (`peerbeam trust revoke`, which is
    /// `FsTrust::remove`), or a time-limited grant simply runs out while
    /// nothing runs. Pruning members when a space is written would therefore be
    /// a sweeper, and — exactly as
    /// [`TrustRecord::expires_at`](peerbeam_domain::entity::TrustRecord) records
    /// for trust expiry — the interval between sweeps is the interval in which
    /// a fan-out still includes a device the user revoked. Asking the trust
    /// store per read means the answer is recomputed every time somebody looks,
    /// so a revoke stops the *next* send rather than the next edit.
    ///
    /// Pruning would also destroy something the user did not ask to lose. Trust
    /// comes back — a 30-minute window is renewed, a re-paired device is
    /// approved again — and a pruned membership does not. This is the same
    /// trade `TrustStore::lookup` makes by keeping an expired record on disk:
    /// expiry ends the grant, it does not forget what was granted.
    ///
    /// # Why `is_trusted` and not `is_approved`
    ///
    /// [`TrustStore::is_trusted`] asks "do we still hold a live pin for this
    /// device", which is precisely what `revoke` removes and what an expiry
    /// closes. [`TrustStore::is_approved`] asks a *stronger* question that chat
    /// has never required — see `peerbeam_chat::gate::may_exchange_chat`, whose
    /// second leg exists because gating chat on approval would silently cut off
    /// every device the user never explicitly approved. Using it here would
    /// report those same devices as stale members of a space they are perfectly
    /// able to receive from.
    ///
    /// That is not a weakened gate, because **a space is not a gate**. Nothing
    /// is authorized by being in one: a fan-out is N ordinary 1:1 sends, each
    /// passing through the per-capability gate it always did. This partition
    /// exists so a list does not lie, not to decide what may leave the machine.
    ///
    /// Fails **closed** per member: [`TrustStore::is_trusted`] answers `false`
    /// on a store error, so a member whose trust cannot be read is reported
    /// stale rather than fanned out to.
    #[must_use]
    pub fn view(&self, trust: &dyn TrustStore) -> SpaceView {
        let (live, stale) = self
            .members
            .iter()
            .cloned()
            .partition(|m| trust.is_trusted(m));
        SpaceView {
            id: self.id.clone(),
            name: self.name.clone(),
            live,
            stale,
        }
    }
}

/// A space as it stands now: recorded members split by whether this machine
/// still trusts them.
///
/// The only shape a store read returns. `stale` is reported rather than hidden
/// because a member that has gone is something the user should see and act on —
/// re-pair the device, or remove it — and a count that silently shrank tells
/// them nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceView {
    pub id: String,
    pub name: String,
    /// Members this machine still trusts: the recipients of a fan-out, in the
    /// order they were added.
    pub live: Vec<DeviceId>,
    /// Members it no longer trusts — revoked, or past the end of a time-limited
    /// grant. Still recorded, deliberately; nothing is sent to them.
    pub stale: Vec<DeviceId>,
}

/// The form two names are compared in to decide whether they are the same name.
///
/// Trimmed and lowercased. A person picking "Work" out of a list cannot tell it
/// from "work", and both are typed at `peerbeam space send work …`; letting
/// both exist makes that lookup ambiguous, which is the failure
/// [`SpaceError::NameTaken`] prevents.
#[must_use]
pub fn normalise(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Check a user-supplied name and return the form that gets stored (trimmed,
/// case preserved).
pub(crate) fn validate_name(raw: &str) -> Result<String, SpaceError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(SpaceError::EmptyName(raw.to_string()));
    }
    if name.len() > MAX_NAME {
        return Err(SpaceError::NameTooLong { len: name.len() });
    }
    if let Some(at) = undisplayable(name) {
        return Err(SpaceError::UndisplayableName { at });
    }
    Ok(name.to_string())
}

/// The first character position that would make a name render dishonestly, if
/// any: C0/C1 controls (a newline paints extra rows into a list, and an escape
/// sequence is acted on by a terminal) and bidi overrides/isolates (the
/// homograph trick — `work\u{202E}nimda` reads as `workadmin`).
///
/// Refused outright, where `peerbeam_chat::display_name` substitutes `U+FFFD`
/// into the same characters instead. The inputs are not alike: a file name
/// arrives from a peer and refusing it would make a file unreceivable, so it
/// can only be defanged. A space name is typed by the person at this keyboard,
/// who can simply type another one — and a label they cannot read back is not a
/// label.
fn undisplayable(name: &str) -> Option<usize> {
    name.chars().position(|c| {
        c.is_control()
            || matches!(c,
                '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    })
}

/// Check that a member id could be a device id at all.
///
/// # Deliberately a shape check, not a format check
///
/// This device's *own* id is `pb-` + 12 hex chars
/// ([`device_id_from_fingerprint`](peerbeam_domain::entity::device_id_from_fingerprint)),
/// but a peer's id is whatever it put in its Hello — `peerbeam_transfer::auth`
/// takes it verbatim — so a build that insisted on that format would refuse to
/// put a genuinely trusted device into a space because it introduced itself
/// differently. What is checked here is what makes an id *usable*: non-empty,
/// bounded, and free of whitespace and control characters, so that it survives
/// a CLI argument, a log line and a storage key unambiguously.
///
/// The question this cannot answer — *is there really such a device?* — is
/// answered next, by the trust store, in [`SpaceStore::add_member`].
///
/// [`SpaceStore::add_member`]: crate::SpaceStore::add_member
pub(crate) fn validate_member(id: &DeviceId) -> Result<(), SpaceError> {
    let s = id.as_str();
    let reason = if s.is_empty() {
        "empty".to_string()
    } else if s.len() > MAX_DEVICE_ID {
        format!("{} bytes (max {MAX_DEVICE_ID})", s.len())
    } else if let Some(at) = s.chars().position(|c| c.is_control() || c.is_whitespace()) {
        format!("whitespace or a control character at char {at}")
    } else {
        return Ok(());
    };
    Err(SpaceError::BadMember {
        id: s.to_string(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::error::{DomainError, Result};

    /// A trust store holding exactly the devices named, plus a switch for the
    /// third answer a real one can give: unreadable.
    struct FakeTrust {
        pinned: Vec<&'static str>,
        broken: bool,
    }

    impl FakeTrust {
        fn holding(pinned: &[&'static str]) -> Self {
            FakeTrust {
                pinned: pinned.to_vec(),
                broken: false,
            }
        }
    }

    impl TrustStore for FakeTrust {
        fn record(&self, _record: TrustRecord) -> Result<()> {
            Ok(())
        }
        fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>> {
            if self.broken {
                return Err(DomainError::Storage("trust store unreadable".into()));
            }
            if !self.pinned.contains(&device.as_str()) {
                return Ok(None);
            }
            Ok(Some(TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: false,
                permissions: PermissionSet::granted_on_approval(),
                expires_at: None,
                mine: false,
                auto_accept: false,
            }))
        }
    }

    fn space(members: &[&str]) -> Space {
        Space {
            id: "s1".into(),
            name: "Work".into(),
            members: members.iter().map(|m| DeviceId::from(*m)).collect(),
        }
    }

    #[test]
    fn a_name_that_is_empty_or_only_whitespace_is_refused() {
        for raw in ["", " ", "\t\t", "   \u{00a0}  "] {
            assert!(
                matches!(validate_name(raw), Err(SpaceError::EmptyName(_))),
                "{raw:?} was accepted as a name"
            );
        }
    }

    #[test]
    fn a_name_is_stored_trimmed_but_with_its_case_intact() {
        assert_eq!(validate_name("  Work Laptops  ").unwrap(), "Work Laptops");
    }

    #[test]
    fn an_oversized_name_is_refused_rather_than_truncated() {
        // Truncating would give the user a space they did not name, and — since
        // names must be unique — one that could silently collide with another.
        assert!(matches!(
            validate_name(&"x".repeat(MAX_NAME + 1)),
            Err(SpaceError::NameTooLong { len }) if len == MAX_NAME + 1
        ));
        assert!(validate_name(&"x".repeat(MAX_NAME)).is_ok());
    }

    /// A name is refused, not defanged, and the refusal says where. Deleting
    /// [`undisplayable`] from [`validate_name`] must make this fail.
    #[test]
    fn a_name_that_cannot_be_rendered_honestly_is_refused() {
        for (raw, at) in [
            ("work\nadmin", 4),
            ("work\u{202E}nimda", 4),
            ("a\u{0007}b", 1),
            ("x\u{2066}y", 1),
        ] {
            assert!(
                matches!(validate_name(raw), Err(SpaceError::UndisplayableName { at: got }) if got == at),
                "{raw:?} was accepted, or refused at the wrong position"
            );
        }
    }

    #[test]
    fn names_are_the_same_name_when_they_differ_only_by_case_or_padding() {
        assert_eq!(normalise("  Work  "), normalise("work"));
        assert_eq!(normalise("WORK"), normalise("work"));
        assert_ne!(normalise("work"), normalise("works"));
    }

    #[test]
    fn a_member_id_that_could_not_be_a_device_id_is_refused_by_reason() {
        for (raw, needle) in [
            ("", "empty"),
            ("pb-a b", "whitespace"),
            ("pb-a\nb", "whitespace"),
        ] {
            let err = validate_member(&DeviceId::from(raw)).expect_err("accepted");
            let text = err.to_string();
            assert!(
                text.contains(needle),
                "{raw:?} was refused, but the message did not say why: {text}"
            );
        }
        let long = "p".repeat(MAX_DEVICE_ID + 1);
        assert!(matches!(
            validate_member(&DeviceId::from(long.as_str())),
            Err(SpaceError::BadMember { .. })
        ));
    }

    /// A peer's id is whatever it put in its Hello, so anything printable and
    /// bounded must be accepted — a stricter `pb-<12 hex>` rule would lock a
    /// genuinely trusted device out of every space.
    #[test]
    fn an_id_that_is_not_this_builds_own_format_is_still_a_usable_member() {
        for raw in ["pb-abcdef012345", "laptop", "PB-ÜBER", "pb-0"] {
            assert!(
                validate_member(&DeviceId::from(raw)).is_ok(),
                "{raw:?} is a usable id and was refused"
            );
        }
    }

    /// **The read-time partition.** Nothing wrote to the space between these
    /// two views; only the trust store differs.
    #[test]
    fn a_member_that_is_no_longer_trusted_is_reported_stale_not_hidden() {
        let space = space(&["pb-alice", "pb-bob"]);

        let all = space.view(&FakeTrust::holding(&["pb-alice", "pb-bob"]));
        assert_eq!(all.live.len(), 2);
        assert!(all.stale.is_empty());

        let revoked = space.view(&FakeTrust::holding(&["pb-alice"]));
        assert_eq!(revoked.live, vec![DeviceId::from("pb-alice")]);
        assert_eq!(
            revoked.stale,
            vec![DeviceId::from("pb-bob")],
            "a revoked member must be visible as gone, not quietly dropped"
        );
        assert_eq!(
            space.members.len(),
            2,
            "and the record itself is untouched, so re-pairing restores it"
        );
    }

    /// Fails closed per member: an unreadable trust store is not a reason to
    /// send to everybody.
    #[test]
    fn an_unreadable_trust_store_makes_every_member_stale() {
        let view = space(&["pb-alice", "pb-bob"]).view(&FakeTrust {
            pinned: vec!["pb-alice", "pb-bob"],
            broken: true,
        });
        assert!(view.live.is_empty(), "a store error is not trust");
        assert_eq!(view.stale.len(), 2);
    }

    #[test]
    fn live_members_keep_the_order_they_were_added_in() {
        let view = space(&["pb-c", "pb-a", "pb-b"]).view(&FakeTrust::holding(&["pb-c", "pb-b"]));
        assert_eq!(
            view.live,
            vec![DeviceId::from("pb-c"), DeviceId::from("pb-b")]
        );
    }
}
