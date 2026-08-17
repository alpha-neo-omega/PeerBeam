//! The encrypted byte pipe: `peerbeam pipe` (ChannelType `0x0107`).
//!
//! ```text
//! $ tar cz ./project | peerbeam pipe --to laptop
//! $ peerbeam pipe --listen > project.tgz
//! ```
//!
//! # Why this lives beside the transfer engine, not in a capability crate
//!
//! Chat, Clipboard and Presence are *message* channels: a typed payload, a
//! handler, a crate each. A pipe is none of that. It is an **unbounded byte
//! stream with no length and no filename**, which is precisely what
//! [`crate::stream`] already moves — so a pipe is that machinery pointed at a
//! different pair of ends, and it reuses it literally: [`read_fill`] for
//! chunking, [`send_with_retry`] for framing, [`recv_verify`] for the closing
//! handshake. There is exactly one chunk loop per direction in this crate and
//! a pipe does not add a second (I2).
//!
//! [`read_fill`]: crate::stream::read_fill
//! [`send_with_retry`]: crate::stream::send_with_retry
//! [`recv_verify`]: crate::stream::recv_verify
//!
//! # What a pipe drops, and what it must keep
//!
//! Dropped from the transfer framing: `Meta` (a pipe has no name and no size —
//! that absence *is* the feature) and `ResumeAck` (stdin is not seekable, so
//! there is nothing to resume from).
//!
//! Kept, and load-bearing: the `Complete{checksum}` → `Verify{ok}` terminator.
//! A receiver that treated a dropped link as end-of-stream would exit `0` on a
//! truncated `peerbeam pipe --listen > project.tgz`, which is silent
//! corruption of a file the user believes is complete. So the stream ends only
//! on an explicit `Complete`, and the SHA-256 it carries is checked against the
//! bytes actually written out. A pipe cannot un-write stdout, so a mismatch is
//! reported the only way it can be — a non-zero exit and a line on stderr.
//!
//! # Memory
//!
//! One chunk buffer per direction, never a whole stream (I10). The sender
//! frames a fixed-size read; the receiver writes each chunk straight out and
//! flushes it, so a 40 GB pipe runs at flat memory on both ends and a consumer
//! downstream of stdout sees bytes while the stream is still running.
//!
//! # Consent
//!
//! Deliberately unlike a file transfer's approval prompt, and the difference is
//! the whole safety story — see [`gate`] and `docs/SECURITY.md`.

mod gate;
mod stream;

pub use gate::{caps_support_stream, may_accept_pipe};
pub use stream::{receive_pipe, send_pipe, PipeStats, PIPE_CHUNK};
