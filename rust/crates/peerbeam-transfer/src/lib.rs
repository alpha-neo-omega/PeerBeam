//! Streaming, chunked file transfer over a [`peerbeam_domain::port::Link`].
//!
//! Handles the transfer mechanics that are independent of the transport:
//!
//! - **Unlimited file size / never load into RAM** — files are streamed from
//!   and to a [`StorageProvider`](peerbeam_domain::port::StorageProvider) one
//!   chunk at a time; peak memory is one chunk buffer per direction.
//! - **Chunked** — [`SendRequest::chunk_size`] bounds each frame.
//! - **Progress** — a [`Progress`](peerbeam_domain::entity::Progress) is
//!   emitted per chunk on an mpsc channel.
//! - **Pause / Cancel** — via a cloneable [`TransferControl`] the UI holds.
//! - **Retry** — each frame send is retried on transient link errors.
//!
//! The transport (QUIC/TCP/…) is any `Link`; the filesystem is any
//! `StorageProvider`, so this crate is fully testable with in-memory links
//! and temp files.

mod auth;
mod clipboard;
mod control;
mod folder;
mod peek;
mod protocol;
mod recover;
mod secure;
mod stream;

pub use auth::{authenticate, Identity, Session};
pub use clipboard::{receive_clipboard, send_clipboard};
pub use control::TransferControl;
pub use folder::{receive_folder, send_folder, FolderReceived, FolderSendRequest};
pub use peek::PeekLink;
pub use protocol::{Control, TransferMeta, BACK_PAUSE, BACK_RESUME};
pub use recover::{receive_file_recover, send_file_recover, LinkFactory};
pub use secure::SecureLink;
pub use stream::{receive_file, send_file, Received, SendRequest, TransferOutcome};
