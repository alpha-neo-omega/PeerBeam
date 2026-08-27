//! Applying the three membership messages a peer can send.
//!
//! # Everything here is a claim by one device about itself
//!
//! That sentence is the whole security model of this file. A peer may say "I
//! joined" or "I left", and both are statements about **its own** membership,
//! which it is entitled to make. Nothing a peer sends may change anyone else's:
//! there is no message meaning "add Carol", and if there were, Bob could enrol
//! Carol's device in a group she never agreed to — the enrolment A2's fourth
//! condition exists to forbid.
//!
//! So [`apply`] only ever writes `sender` into or out of a roster. The sender is
//! taken from the authenticated session, never from the message body, which is
//! what makes "about itself" true rather than merely intended.

use peerbeam_domain::id::DeviceId;

use crate::group::GroupError;
use crate::message::{
    GroupInvite, GroupJoined, GroupLeft, MSG_GROUP_INVITE, MSG_GROUP_JOINED, MSG_GROUP_LEFT,
};
use crate::store::GroupStore;

/// What arriving membership traffic did, so a surface can show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEvent {
    /// Somebody offered this device a group. **Nothing has been joined**: the
    /// invitation is held for the user to accept or ignore, and only their
    /// acceptance writes a roster (A2, condition 4).
    Invited {
        group: String,
        name: String,
        from: DeviceId,
        members: Vec<DeviceId>,
    },
    /// A member announced itself, and is now in the local roster.
    Joined { group: String, who: DeviceId },
    /// A member announced it had left, and is now out of the local roster.
    Left { group: String, who: DeviceId },
    /// A frame this build does not implement, or one that named a group this
    /// device does not hold. Neither is an error: the first is a newer peer,
    /// the second is ordinary — somebody left a group and its members did not
    /// all find out at once.
    Ignored,
}

/// Apply one foreign Chat frame, if it is a group message.
///
/// `sender` is the authenticated peer, from the session — **not** anything in
/// the payload. A message body cannot name who sent it here, so a peer cannot
/// speak for a device that is not itself.
///
/// # Errors
/// Only a store failure. A malformed payload, an unknown type, or a group this
/// device does not hold are all [`GroupEvent::Ignored`]: a peer cannot break
/// this device's conversation by sending nonsense, and a group somebody else
/// still thinks we are in is not a fault to report.
pub fn apply(
    store: &GroupStore,
    sender: &DeviceId,
    message_type: u16,
    payload: &[u8],
) -> Result<GroupEvent, GroupError> {
    match message_type {
        MSG_GROUP_INVITE => {
            let Ok(invite) = serde_json::from_slice::<GroupInvite>(payload) else {
                return Ok(GroupEvent::Ignored);
            };
            // Held, not adopted. `adopt` runs when the user accepts.
            Ok(GroupEvent::Invited {
                group: invite.group,
                name: invite.name,
                from: sender.clone(),
                members: invite.members,
            })
        }
        MSG_GROUP_JOINED => {
            let Ok(joined) = serde_json::from_slice::<GroupJoined>(payload) else {
                return Ok(GroupEvent::Ignored);
            };
            // A join for a group we do not hold is ignored rather than created:
            // creating one would let any peer put a group on this device by
            // announcing itself into it.
            if store.get(&joined.group).is_err() {
                return Ok(GroupEvent::Ignored);
            }
            store.add_member(&joined.group, sender)?;
            Ok(GroupEvent::Joined {
                group: joined.group,
                who: sender.clone(),
            })
        }
        MSG_GROUP_LEFT => {
            let Ok(left) = serde_json::from_slice::<GroupLeft>(payload) else {
                return Ok(GroupEvent::Ignored);
            };
            if store.get(&left.group).is_err() {
                return Ok(GroupEvent::Ignored);
            }
            store.remove_member(&left.group, sender)?;
            Ok(GroupEvent::Left {
                group: left.group,
                who: sender.clone(),
            })
        }
        _ => Ok(GroupEvent::Ignored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{GroupInvite, GroupJoined, GroupLeft};
    use crate::store::tests::new_store;

    fn dev(s: &str) -> DeviceId {
        DeviceId::from(s)
    }

    /// **An invitation joins nothing.** It is an offer; only the user's
    /// acceptance writes a roster. A build where receiving one enrolled the
    /// device would let anyone put anyone into a group (A2, condition 4).
    #[test]
    fn an_invitation_is_held_not_adopted() {
        let (store, _t, _d) = new_store();
        let invite = GroupInvite {
            group: "g1".into(),
            name: "Trip".into(),
            members: vec![dev("pb-alice")],
        };
        let ev = apply(
            &store,
            &dev("pb-alice"),
            MSG_GROUP_INVITE,
            &serde_json::to_vec(&invite).unwrap(),
        )
        .unwrap();

        assert!(matches!(ev, GroupEvent::Invited { .. }));
        assert!(
            store.list().unwrap().is_empty(),
            "receiving an invitation must not create a group"
        );
    }

    /// **A peer speaks only for itself.** The sender comes from the
    /// authenticated session, so a `GroupJoined` naming somebody else in its
    /// body still adds the sender and nobody else — there is no field it could
    /// use to enrol a third party.
    #[test]
    fn a_join_adds_the_authenticated_sender_and_nobody_else() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();

        // The payload carries only a group id — by construction there is
        // nowhere to name a victim — and the sender is supplied separately.
        let joined = GroupJoined {
            group: g.id.clone(),
        };
        let ev = apply(
            &store,
            &dev("pb-alice"),
            MSG_GROUP_JOINED,
            &serde_json::to_vec(&joined).unwrap(),
        )
        .unwrap();

        assert_eq!(
            ev,
            GroupEvent::Joined {
                group: g.id.clone(),
                who: dev("pb-alice")
            }
        );
        let after = store.get(&g.id).unwrap();
        assert!(after.holds(&dev("pb-alice")));
        assert_eq!(after.members.len(), 2, "exactly one device was added");
    }

    /// A join for a group this device does not hold must not create one:
    /// otherwise any peer could put a group on this machine by announcing
    /// itself into an id it invented.
    #[test]
    fn a_join_for_an_unknown_group_creates_nothing() {
        let (store, _t, _d) = new_store();
        let joined = GroupJoined {
            group: "never-heard-of-it".into(),
        };
        let ev = apply(
            &store,
            &dev("pb-alice"),
            MSG_GROUP_JOINED,
            &serde_json::to_vec(&joined).unwrap(),
        )
        .unwrap();
        assert_eq!(ev, GroupEvent::Ignored);
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn a_leave_removes_only_the_sender() {
        let (store, _t, _d) = new_store();
        let g = store.create("Trip").unwrap();
        store.add_member(&g.id, &dev("pb-alice")).unwrap();
        store.add_member(&g.id, &dev("pb-bob")).unwrap();

        let left = GroupLeft {
            group: g.id.clone(),
        };
        apply(
            &store,
            &dev("pb-alice"),
            MSG_GROUP_LEFT,
            &serde_json::to_vec(&left).unwrap(),
        )
        .unwrap();

        let after = store.get(&g.id).unwrap();
        assert!(!after.holds(&dev("pb-alice")));
        assert!(after.holds(&dev("pb-bob")), "bob was removed too");
    }

    /// **Nonsense from a peer must not break the conversation.** A malformed
    /// payload, or a type this build does not implement, is ignored — not an
    /// error that would fail the channel and take the chat down with it.
    #[test]
    fn a_malformed_or_unknown_message_is_ignored_rather_than_fatal() {
        let (store, _t, _d) = new_store();
        for (ty, payload) in [
            (MSG_GROUP_INVITE, b"not json".as_slice()),
            (MSG_GROUP_JOINED, b"{".as_slice()),
            (MSG_GROUP_LEFT, b"[]".as_slice()),
            (9999, b"{}".as_slice()),
        ] {
            assert_eq!(
                apply(&store, &dev("pb-alice"), ty, payload).unwrap(),
                GroupEvent::Ignored,
                "type {ty} with {payload:?} was not ignored"
            );
        }
    }
}
