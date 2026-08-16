//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

mod handler;
mod message;
mod record;
mod send;
pub mod staging;
mod store;

pub use handler::{ChatHandler, ReceivedSink};
pub use message::{
    mint_id, ChatError, ChatMessage, FileRef, MAX_BODY, MAX_NAME, MSG_FILE_REF, MSG_TEXT,
};
pub use record::{display_name, ChatRecord, Direction, FileMeta, Kind, Status};
pub use send::{flush_to_session, prepare_file_send, send_file_ref, send_message, SendError};
pub use staging::{StagingError, StagingLimits, StagingStore};
pub use store::{namespace, ChatStore, OutboxEntry, StagedFile, OUTBOX_NS};
