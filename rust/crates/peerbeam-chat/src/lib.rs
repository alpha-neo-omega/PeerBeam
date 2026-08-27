//! Secure peer-to-peer chat: the `Chat` capability on PeerSession.

pub mod gate;
mod handler;
mod message;
mod record;
pub mod reply;
pub mod retention;
mod search;
mod send;
pub mod staging;
mod store;

pub use gate::may_exchange_chat;
pub use handler::{ChatHandler, ForeignSink, ReceivedSink};
pub use message::{
    mint_id, ChatError, ChatMessage, FileDecline, FileRef, Reaction, Receipt, MAX_BODY, MAX_ID,
    MAX_NAME, MAX_REACTION, MSG_FILE_DECLINE, MSG_FILE_REF, MSG_REACTION, MSG_RECEIPT, MSG_TEXT,
};
pub use record::{display_name, ChatRecord, Direction, FileMeta, Kind, Status, StoredReaction};
pub use reply::{resolve_replies, ReplyContext, ReplyParent, PREVIEW_CHARS};
pub use retention::{
    prune_all_conversations, prune_conversation, Pruned, Retention, MAX_TTL_SECS, RETENTION_NS,
};
pub use search::{SearchHit, SearchResults, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT};
pub use send::{
    begin_file_send, flush_to_session, next_file_for, prepare_file_send, send_file_decline,
    send_file_ref, send_foreign, send_message, send_reaction, send_receipt, send_reply,
    stage_file_send, PendingFile, SendError,
};
pub use staging::{StagingError, StagingLimits, StagingStore};
pub use store::{namespace, ChatStore, Landing, OutboxEntry, StagedFile, OUTBOX_NS};
