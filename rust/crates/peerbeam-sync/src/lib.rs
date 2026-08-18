//! Folder reconciliation: what a peer has, and what this device needs.
//!
//! **A one-way pull mirror, not bidirectional continuous sync.** Two devices
//! editing the same file while apart is a conflict problem with no good
//! automatic answer, and pretending otherwise is how sync tools lose work. This
//! fetches what a peer has and never deletes, never overwrites newer local
//! work, and never pushes.
//!
//! Bytes travel over the Transfer channel, exactly as `MESSAGE_REGISTRY.md`
//! always said: a second bulk path would mean a second set of resume, checksum
//! and progress semantics to keep in step with the first.

mod handler;
mod manifest;

pub use handler::{build, IncomingSink, ManifestSink, SendFile, SyncHandler};
pub use manifest::{
    plan, FileEntry, FileRequest, Manifest, ManifestRequest, Plan, SyncError, MAX_FILES, MAX_PATH,
    MSG_FILE_REQUEST, MSG_MANIFEST, MSG_MANIFEST_REQUEST,
};
