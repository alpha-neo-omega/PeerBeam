//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

pub mod gate;
mod handler;
mod message;
mod record;
mod search;
mod send;
pub mod staging;
mod store;

pub use gate::may_exchange_chat;
pub use handler::{ChatHandler, ReceivedSink};
pub use message::{
    mint_id, ChatError, ChatMessage, FileDecline, FileRef, Reaction, MAX_BODY, MAX_ID, MAX_NAME,
    MAX_REACTION, MSG_FILE_DECLINE, MSG_FILE_REF, MSG_REACTION, MSG_TEXT,
};
pub use record::{display_name, ChatRecord, Direction, FileMeta, Kind, Status, StoredReaction};
pub use search::{SearchHit, SearchResults, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT};
pub use send::{
    begin_file_send, flush_to_session, next_file_for, prepare_file_send, send_file_decline,
    send_file_ref, send_message, send_reaction, stage_file_send, PendingFile, SendError,
};
pub use staging::{StagingError, StagingLimits, StagingStore};
pub use store::{namespace, ChatStore, OutboxEntry, StagedFile, OUTBOX_NS};
