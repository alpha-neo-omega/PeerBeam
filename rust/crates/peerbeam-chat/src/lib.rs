//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod message;

pub use message::{ChatError, ChatMessage, MAX_BODY, MSG_TEXT};
