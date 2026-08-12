//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod handler;
mod message;
mod record;
mod send;
mod store;

pub use handler::{ChatHandler, ReceivedSink};
pub use message::{ChatError, ChatMessage, MAX_BODY, MSG_TEXT};
pub use record::{ChatRecord, Direction, Status};
pub use send::{send_message, SendError};
pub use store::{namespace, ChatStore, OutboxEntry, OUTBOX_NS};
