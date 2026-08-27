//! What a group puts on the wire, and what it deliberately does not.
//!
//! # Three messages, and no fourth
//!
//! * [`GroupInvite`] — *"here is a group, and here is who is in it"*. An
//!   **offer**, never an enrolment: nothing about receiving one changes the
//!   recipient's store until their own user accepts (A2, condition 4).
//! * [`GroupJoined`] — *"I accepted; add me"*. Sent by the joiner to every
//!   member it was told about, directly, one send each.
//! * [`GroupLeft`] — *"remove me"*. Same shape.
//!
//! There is no `GroupRosterRequest`, and its absence is the design. A message
//! asking another device "who is in this group?" would make that device the
//! answer — a hub, whatever it was named — and A2 permits this feature only on
//! the condition that no such device exists. Every member already holds the
//! whole roster; nobody needs to ask.
//!
//! # Why a member announces itself rather than being announced
//!
//! When someone joins, **they** tell the other members, rather than the
//! inviter telling them on their behalf. That is condition 4 read strictly: a
//! device is added to a roster by an action of its own user, and a design where
//! the inviter broadcasts "X has joined" lets one user enrol another's device
//! in something it never agreed to.
//!
//! It costs consistency, and the cost is accepted rather than hidden: a member
//! offline when someone joins learns about them at the next contact, so two
//! rosters can differ for a while. There is nobody to ask for the truth,
//! because having somebody to ask is the thing being refused.

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::MessageType;

/// MessageType id for a group invitation, in the Chat channel namespace.
///
/// Chat's namespace rather than a channel of its own: a group **is** a
/// conversation, its messages are chat messages, and a second channel would
/// mean a second set of open/accept/close paths carrying the same traffic.
pub const MSG_GROUP_INVITE: u16 = 6;

/// MessageType id for "I accepted your invitation".
pub const MSG_GROUP_JOINED: u16 = 7;

/// MessageType id for "remove me from this group".
pub const MSG_GROUP_LEFT: u16 = 8;

/// An offer to join a group, carrying the roster as the sender holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupInvite {
    /// The shared group id.
    pub group: String,
    /// The sender's name for the group, offered as a suggestion.
    ///
    /// Not binding: names are local (see [`Group::name`](crate::Group)), so
    /// this is what the recipient starts with rather than what they are stuck
    /// with.
    pub name: String,
    /// Every device the sender believes is in the group, itself included.
    ///
    /// **This is the metadata cost, on the wire.** Accepting means learning
    /// this list and being added to everyone else's, which is why A2 condition
    /// 5 requires the UI to say so before the user accepts rather than after.
    pub members: Vec<DeviceId>,
}

impl GroupInvite {
    /// The MessageType this rides as.
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_GROUP_INVITE)
    }
}

/// "I accepted your invitation — add me to your roster."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupJoined {
    /// The group being joined.
    pub group: String,
}

impl GroupJoined {
    /// The MessageType this rides as.
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_GROUP_JOINED)
    }
}

/// "Remove me from this group."
///
/// Advisory, and honestly so: a member that ignores it keeps sending, and
/// nothing here can stop that — there is no authority to appeal to. What
/// leaving *does* guarantee is local and complete: this device drops the group
/// and stops sending to it. Refusing another device's messages afterwards is
/// what `trust revoke-permission <device> chat` is for, which is a decision
/// about that device rather than about a label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupLeft {
    /// The group being left.
    pub group: String,
}

impl GroupLeft {
    /// The MessageType this rides as.
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_GROUP_LEFT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The three ids must not collide with chat's existing five.** They share
    /// one MessageType namespace, so a duplicate would route a group invite
    /// into the text handler — or worse, silently parse as one.
    #[test]
    fn the_group_message_ids_do_not_collide_with_chats() {
        // Read from `peerbeam_chat` rather than restated here: a copy would go
        // on passing after chat added a sixth message type that landed on one
        // of ours, which is the exact collision this test exists to catch.
        let chat_ids = [
            peerbeam_chat::MSG_TEXT,
            peerbeam_chat::MSG_FILE_REF,
            peerbeam_chat::MSG_FILE_DECLINE,
            peerbeam_chat::MSG_REACTION,
            peerbeam_chat::MSG_RECEIPT,
        ];
        for id in [MSG_GROUP_INVITE, MSG_GROUP_JOINED, MSG_GROUP_LEFT] {
            assert!(
                !chat_ids.contains(&id),
                "group message id {id} collides with a chat message id"
            );
        }
        let mine = [MSG_GROUP_INVITE, MSG_GROUP_JOINED, MSG_GROUP_LEFT];
        let unique: std::collections::BTreeSet<u16> = mine.into_iter().collect();
        assert_eq!(unique.len(), mine.len(), "two group ids are the same");
    }

    /// An invitation must survive a round trip with its roster intact — it is
    /// the only thing that carries one.
    #[test]
    fn an_invitation_round_trips_with_its_roster() {
        let invite = GroupInvite {
            group: "abc123".into(),
            name: "Trip".into(),
            members: vec![DeviceId::from("pb-a"), DeviceId::from("pb-b")],
        };
        let wire = serde_json::to_vec(&invite).unwrap();
        let back: GroupInvite = serde_json::from_slice(&wire).unwrap();
        assert_eq!(back, invite);
    }
}
