//! Notes: text you keep, and (once sync lands) keep in step across the devices
//! you have granted the notes permission.
//!
//! Deliberately not a document store. A note is text with a title, an id and a
//! last-edited time; conflict resolution is last-writer-wins and deletion
//! leaves a tombstone. Anything richer — attachments, history, folders — is a
//! different feature wearing this one's name.

mod gate;
mod message;
mod note;
mod store;

pub use gate::may_sync_notes;
pub use message::{NoteBatch, MAX_BATCH_BYTES, MAX_BATCH_NOTES, MSG_NOTE_BATCH};
pub use note::{mint_id, Note, NoteError, MAX_BODY, MAX_ID, MAX_TITLE};
pub use store::{NoteStore, NS};
