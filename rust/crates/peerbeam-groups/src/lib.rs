//! Groups: named sets of trusted devices that **all know about each other**.
//!
//! # What a Group is, and what a Space is
//!
//! `peerbeam_spaces` gives a device a private label for a set of peers. Nothing
//! about a Space reaches anyone, so no member learns who else is in one — and
//! the price is that there are no group replies.
//!
//! A Group pays the opposite price and buys the conversation. Every member
//! holds the same roster, so a reply reaches everyone; and every member learns
//! who every other member is. Neither is a better version of the other, and
//! neither is converted into the other.
//!
//! Groups exist only because amendment **A2** permits them, on eight binding
//! conditions. Three of them are structural rather than behavioural, and are
//! visible in the shapes here rather than enforced by a check somewhere:
//!
//! * **No hub** — [`Group`] has no owner, creator or admin field, and
//!   [`message`] defines no "who is in this group?" request. There is nothing
//!   to ask, because every member already holds the answer.
//! * **Explicit joining** — a device is added to a roster by
//!   [`GroupJoined`] from **that device**, never by the inviter announcing it.
//!   [`GroupStore::create`] takes no member list for the same reason.
//! * **Permission still gates every send** — [`GroupStore::reachable`] asks the
//!   trust store per member, at read time, and returns who was refused so a
//!   caller can name them.
//!
//! The other five (no relay, the stated metadata cost, Spaces unchanged, CLI
//! parity, no group key) are kept by the layers above this crate; see
//! `docs/ARCHITECTURAL_INVARIANTS.md`.

mod control;
mod group;
mod message;
mod store;

pub use control::{apply, GroupEvent};
pub use group::{
    normalise, validate_member, validate_name, Group, GroupError, MAX_MEMBERS, MAX_NAME,
};
pub use message::{
    GroupInvite, GroupJoined, GroupLeft, MSG_GROUP_INVITE, MSG_GROUP_JOINED, MSG_GROUP_LEFT,
};
pub use store::{GroupStore, NS};
