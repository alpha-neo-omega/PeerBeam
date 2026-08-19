//! Folder reconciliation: what a peer has, and what this device needs.
//!
//! **Bidirectional, with conflicts kept rather than resolved.** Per-file version
//! vectors distinguish "their copy is newer" from "we both changed it" — the
//! question a modification time cannot answer, and the one that decides whether
//! an edit survives. When both sides changed, both copies are kept: no
//! automatic rule picks correctly, and every one of them loses somebody's work.
//!
//! Bytes travel over the Transfer channel, exactly as `MESSAGE_REGISTRY.md`
//! always said: a second bulk path would mean a second set of resume, checksum
//! and progress semantics to keep in step with the first.

mod apply;
mod delta;
mod handler;
mod index;
mod manifest;
mod reconcile;
mod rename;
mod version;

pub use apply::{apply_local, Outcome};
pub use delta::{plan as plan_delta, reassemble, ChunkMap, Need};
pub use handler::{build, build_with, IncomingSink, ManifestSink, SendFile, SyncHandler};
pub use index::{IndexEntry, SyncIndex, NS as INDEX_NS};
pub use manifest::{
    plan, FileEntry, FileRequest, Manifest, ManifestRequest, Plan, SyncError, MAX_FILES, MAX_PATH,
    MSG_FILE_REQUEST, MSG_MANIFEST, MSG_MANIFEST_REQUEST,
};
pub use reconcile::{conflict_name, reconcile, Action, RemoteFile};
pub use rename::{detect as detect_renames, Rename};
pub use version::{Relation, VersionVector};
