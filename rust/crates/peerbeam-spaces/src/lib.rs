//! Spaces: named sets of trusted devices you can message or send to at once.
//!
//! A Space is a **local, private label** over device ids this machine already
//! trusts, and nothing else. It is not shared with anyone, it is not synced,
//! and there is no group protocol on the wire — this crate defines no message
//! type, registers no channel and touches no `PeerSession`.
//!
//! # What a fan-out actually is
//!
//! Sending to a Space is N ordinary 1:1 sends over the per-peer sessions that
//! already exist. Each member receives a normal direct message, indistinguishable
//! from one typed to them alone, and passes through the same per-capability gate
//! it always did. Nothing here authorizes anything.
//!
//! # Nobody learns who else is in it, and that is the feature
//!
//! Because no group identity travels, a member cannot enumerate the others,
//! cannot tell a fan-out from a direct message, and cannot learn that a Space
//! exists at all. That is the privacy property, not a missing feature: the
//! moment a Space had a shared identity on the wire, some device would have to
//! hold the roster and hand it out — and a device that brokers membership for
//! everyone else is a hub. `docs/SPACES.md` states this for users.
//!
//! # Constitutional fit
//!
//! * **I3 (peer-to-peer first)** — no hub is involved in the common case or in
//!   any case. There is no coordinating server to add, because a Space never
//!   leaves the device that defined it.
//! * **I6 (trust-gated capabilities)** — membership grants nothing. Every send
//!   is still gated per peer, per capability, at send time
//!   ([`Space::view`] explains why that means membership may use the weaker
//!   `is_trusted` predicate), and a revoked device leaves the fan-out at the
//!   very next read rather than at the next edit.
//! * **[VISION.md](../../../docs/VISION.md)'s non-goals** — "no hub-brokered
//!   group chat, feeds, discovery of strangers, or public rooms". A local label
//!   over devices the user already paired with is none of those.
//! * **I11 (local-first)** — a Space works offline, is stored encrypted at rest
//!   by the [`AppStore`](peerbeam_domain::port::AppStore), and needs no
//!   synchronization to be usable.
//!
//! Deliberately not a group: no shared roster, no group key, no membership
//! messages, no invites, no "who else is here". Any of those would be a
//! different feature wearing this one's name.

mod space;
mod store;

pub use space::{normalise, Space, SpaceError, SpaceView, MAX_DEVICE_ID, MAX_NAME};
pub use store::{SpaceStore, NS};
