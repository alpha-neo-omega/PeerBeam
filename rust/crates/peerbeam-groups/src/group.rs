//! A Group: a named set of trusted devices that all know about each other.
//!
//! # A Group is not a Space, and the difference is the whole point
//!
//! A [`Space`](peerbeam_spaces) is a label this device keeps for a set of
//! devices. Nothing about it reaches a peer, so no member learns who else is in
//! one — and the price is that there are no group replies, because a member's
//! answer comes back to the sender alone.
//!
//! A Group pays the opposite price. Every member holds the **complete roster**,
//! so replies reach everyone and a conversation is possible — and every member
//! learns who every other member is. That disclosure is the entire cost, it is
//! irreversible once made, and amendment
//! [A2](../../../../docs/ARCHITECTURAL_INVARIANTS.md) requires it to be stated
//! to the user at the point of joining rather than discovered afterwards.
//!
//! Neither construct is converted into the other, and a Space's fan-out send is
//! not quietly renamed "group chat".
//!
//! # No hub, stated as a data structure
//!
//! There is no `owner`, no `creator`, and no `admin` field on this type, and
//! that absence is load-bearing rather than an omission to be filled in later.
//! The moment one member answers membership questions on behalf of the others,
//! that member is a hub — whatever it is called — and A2's first binding
//! condition is broken. Every member holds the same roster and answers only for
//! itself.

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;

/// The longest a group name may be, in characters.
///
/// The same limit `peerbeam_spaces` uses, for the same reason: it is a label a
/// person types and reads back in a list, not a document.
pub const MAX_NAME: usize = 64;

/// The most members a group may hold.
///
/// **A send is N direct sends** (A2, condition 2), so this is a bound on how
/// much one message costs rather than an arbitrary ceiling: a hundred members
/// is a hundred dials, and a build that let someone type a thousand would be
/// offering a button that cannot work. It is deliberately generous enough that
/// no ordinary group meets it.
pub const MAX_MEMBERS: usize = 64;

/// What can be wrong with a group, or with an operation on one.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GroupError {
    #[error("a group needs a name")]
    EmptyName,
    #[error("a group name may be at most {MAX_NAME} characters")]
    NameTooLong,
    #[error("there is already a group called {name:?}")]
    DuplicateName { name: String },
    #[error("no group {id:?} on this device")]
    UnknownGroup { id: String },
    #[error("a group may hold at most {MAX_MEMBERS} devices")]
    TooManyMembers,
    #[error("{id:?} is not a usable device id: {reason}")]
    BadMember { id: String, reason: String },
    #[error("{id:?} is not a device this machine trusts")]
    UntrustedMember { id: String },
    #[error("could not read the trust store for {id:?}: {reason}")]
    TrustUnreadable { id: String, reason: String },
    #[error("could not read the group store: {reason}")]
    Unreadable { reason: String },
    #[error("could not write the group store: {reason}")]
    Unwritable { reason: String },
}

/// A group as this device holds it.
///
/// Every member holds a record with the same `id` and the same `members`; the
/// `name` is deliberately **not** synchronised — see the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Shared across every member, and minted at random.
    ///
    /// Unlike a Space's id this one crosses the wire, so it has to be unique
    /// across *devices* rather than merely within one store — two people
    /// creating a group at the same moment on two machines must not collide.
    /// That is why it is 128 bits and not the 64 a Space uses.
    ///
    /// It is also why it is random rather than derived from anything: a name,
    /// a member list or a timestamp would collide exactly when two groups are
    /// most alike, and would leak what it was derived from to anyone who saw
    /// the id.
    pub id: String,
    /// What this device calls the group.
    ///
    /// **Local, and not reconciled.** An invitation carries the creator's name
    /// as a suggestion, and nothing afterwards forces two members to agree: a
    /// rename here is a rename here. Synchronising it would need somebody to
    /// arbitrate when two members rename at once, and that somebody would be a
    /// hub — the thing A2 permits this feature only by not having. The cost is
    /// that two members may see different names for one conversation, which is
    /// a smaller price than a coordinator.
    pub name: String,
    /// Every device in the group, **including this one**.
    ///
    /// Self-inclusive on purpose: the roster is the same list on every member,
    /// so "who is in this group" has one answer rather than one answer plus
    /// whoever is asking. Sending skips this device rather than filtering it
    /// out of the roster.
    #[serde(default)]
    pub members: Vec<DeviceId>,
}

/// An invitation this device has received and not yet answered.
///
/// **Not a group.** Holding one means somebody offered; it grants nothing, and
/// nothing is joined until this device's own user accepts (A2, condition 4).
/// It is stored apart from groups for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvite {
    /// The group being offered.
    pub group: String,
    /// The inviter's name for it — a suggestion, since names are local.
    pub name: String,
    /// Who offered, as the authenticated session reported them.
    pub from: DeviceId,
    /// Who is already in it.
    ///
    /// **This is the disclosure.** Accepting means learning these devices and
    /// being learned by them, which is why a surface must state it before the
    /// user answers rather than after (A2, condition 5).
    pub members: Vec<DeviceId>,
    /// When it arrived, RFC 3339.
    pub at: String,
}

impl Group {
    /// Everyone but this device — the recipients of one message.
    ///
    /// `me` is passed rather than read from anywhere, so this stays a pure
    /// function of the roster and the caller's identity and can be tested
    /// without an engine.
    #[must_use]
    pub fn recipients(&self, me: &DeviceId) -> Vec<DeviceId> {
        self.members.iter().filter(|m| *m != me).cloned().collect()
    }

    /// Whether `device` is in this group's roster.
    #[must_use]
    pub fn holds(&self, device: &DeviceId) -> bool {
        self.members.iter().any(|m| m == device)
    }
}

/// A group name reduced to the form two names are compared in.
///
/// Case-folded and whitespace-collapsed, so "Work Trip" and "work  trip" are
/// the same label to a person and are treated as one here. Matches what
/// `peerbeam_spaces::normalise` does, because a user who has learned the rule
/// for one should not have to learn a second.
#[must_use]
pub fn normalise(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Check a name a person typed, returning it trimmed.
///
/// # Errors
/// [`GroupError::EmptyName`] or [`GroupError::NameTooLong`].
pub fn validate_name(name: &str) -> Result<String, GroupError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(GroupError::EmptyName);
    }
    // Counted in characters, not bytes: a name of emoji is not longer than a
    // name of letters to the person who typed it.
    if trimmed.chars().count() > MAX_NAME {
        return Err(GroupError::NameTooLong);
    }
    Ok(trimmed.to_string())
}

/// Check a device id before it is written into a roster.
///
/// Rejects the empty string and anything carrying a control character or a
/// separator, for the reason `peerbeam_spaces::validate_member` does: these ids
/// end up in file names, log lines and JSON, and one that can forge a delimiter
/// is one that can forge a record.
///
/// # Errors
/// [`GroupError::BadMember`].
pub fn validate_member(id: &DeviceId) -> Result<(), GroupError> {
    let raw = &id.0;
    if raw.trim().is_empty() {
        return Err(GroupError::BadMember {
            id: raw.clone(),
            reason: "it is empty".into(),
        });
    }
    if let Some(bad) = raw
        .chars()
        .find(|c| c.is_control() || *c == '/' || *c == '\\' || *c == '\0')
    {
        return Err(GroupError::BadMember {
            id: raw.clone(),
            reason: format!("it contains {bad:?}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(s: &str) -> DeviceId {
        DeviceId::from(s)
    }

    #[test]
    fn recipients_exclude_this_device_and_nobody_else() {
        let g = Group {
            id: "g1".into(),
            name: "Trip".into(),
            members: vec![dev("pb-me"), dev("pb-a"), dev("pb-b")],
        };
        assert_eq!(g.recipients(&dev("pb-me")), vec![dev("pb-a"), dev("pb-b")]);
        // A device that is not a member gets the whole roster, which is the
        // honest answer: it is not in the group, so nothing is "itself".
        assert_eq!(g.recipients(&dev("pb-x")).len(), 3);
    }

    /// **The roster includes this device.** A member list that excluded the
    /// holder would answer "who is in this group" differently on every machine,
    /// and an invitation built from it would omit whoever sent it.
    #[test]
    fn the_roster_holds_this_device_too() {
        let g = Group {
            id: "g1".into(),
            name: "Trip".into(),
            members: vec![dev("pb-me"), dev("pb-a")],
        };
        assert!(g.holds(&dev("pb-me")));
        assert!(g.holds(&dev("pb-a")));
        assert!(!g.holds(&dev("pb-nobody")));
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("  Trip  ").unwrap(), "Trip");
        assert_eq!(validate_name("   "), Err(GroupError::EmptyName));
        let long = "x".repeat(MAX_NAME + 1);
        assert_eq!(validate_name(&long), Err(GroupError::NameTooLong));
        // Measured in characters: a name of emoji at the limit is accepted.
        let emoji = "🙂".repeat(MAX_NAME);
        assert!(validate_name(&emoji).is_ok());
    }

    #[test]
    fn two_names_a_person_would_call_the_same_normalise_alike() {
        assert_eq!(normalise("Work Trip"), normalise("work  trip"));
        assert_ne!(normalise("Work"), normalise("Works"));
    }

    #[test]
    fn a_member_id_that_could_forge_a_delimiter_is_refused() {
        assert!(validate_member(&dev("pb-ok")).is_ok());
        for bad in ["", "  ", "pb-a/b", "pb-a\\b", "pb-a\nb"] {
            assert!(
                validate_member(&dev(bad)).is_err(),
                "{bad:?} was accepted into a roster"
            );
        }
    }
}
