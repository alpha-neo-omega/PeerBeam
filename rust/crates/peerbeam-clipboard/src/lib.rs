//! Clipboard sync: the `Clipboard` capability on PeerSession (ChannelType
//! `0x0102`, `docs/MESSAGE_REGISTRY.md` §2).
//!
//! One message type — [`Clip`] — carrying the text a user just copied. It rides
//! an ordinary message channel, exactly like Chat and Presence, so it inherits
//! the session's authentication, sealing and channel semantics and adds no
//! transport of its own (I2).
//!
//! # The privacy story
//!
//! Two gates gate every outbound clip, and they are the whole feature:
//!
//! * **Opt-in, default off.** One setting, *"Sync clipboard with trusted
//!   devices"*. While it is off this device sends nothing at all.
//! * **Trusted-only, not configurable.** A clip never leaves for a peer that is
//!   not trusted, whatever the setting says.
//!
//! Both live in one function, [`may_share_clip`], which
//! [`ClipboardSender::send`] consults before it opens a channel or sends a
//! frame. Receiving is unconditional: a device with sync off still applies
//! what its peers send.
//!
//! # There is no password detection, and the UI says so
//!
//! **Everything copied while the setting is on is sent, passwords included.**
//! This is not an oversight to be patched later. A clipboard read returns plain
//! text and nothing else: Flutter's `Clipboard.getData` carries no sensitivity
//! flag, and X11 and Wayland define no standard one, so there is no signal
//! distinguishing a password manager's paste buffer from a shopping list.
//!
//! A heuristic that guessed would be wrong in both directions, and both are
//! bad. Guessing "secret" on ordinary text silently drops clips the user
//! expected to arrive, teaching them the feature is broken. Guessing "safe" on
//! a real credential ships it while the UI implies something was checked —
//! which is worse than never having claimed to check, because the user relaxes
//! on the strength of a promise nothing is keeping. So this build makes no
//! guess and says exactly that on the toggle, and a UI test pins that wording
//! so a later tidy-up cannot quietly delete the warning.
//!
//! The honest controls are the ones that are actually enforceable: it is off
//! until you turn it on, it goes only to devices you pinned, and you can turn
//! it off again in one tap.
//!
//! # Desktop sends, every platform receives
//!
//! Android 10+ forbids reading the clipboard from the background, so a phone
//! can never auto-send — no permission and no workaround changes that, and
//! pretending otherwise would mean a toggle that silently does nothing. The
//! asymmetry is a platform limit, stated in the UI rather than hidden: a phone
//! advertises [`CLIPBOARD_FEAT_CLIP`](peerbeam_domain::session::CLIPBOARD_FEAT_CLIP)
//! truthfully and applies incoming clips in full.
//!
//! Nothing is persisted (I4). A synced clipboard is live state, and a stored
//! history of everything the user ever copied is precisely the artefact this
//! feature must not create.

pub mod gate;
mod handler;
mod history;
mod message;
mod send;

pub use gate::{caps_support_clip, may_share_clip};
pub use handler::{ClipboardHandler, ClipboardSink};
pub use history::{ClipEntry, ClipHistory, MAX_ENTRIES, NS as HISTORY_NS};
pub use message::{Clip, ClipboardError, MAX_CLIP, MSG_CLIP};
pub use send::{ClipboardSender, Push, SendError, SyncSetting};
