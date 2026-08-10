//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod message;
mod record;
mod store;

pub use message::{ChatError, ChatMessage, MAX_BODY, MSG_TEXT};
pub use record::{ChatRecord, Direction, Status};
pub use store::{namespace, ChatStore};
