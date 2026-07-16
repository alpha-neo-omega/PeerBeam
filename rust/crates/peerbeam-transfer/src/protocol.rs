//! Transfer wire protocol — the framing on top of a [`Link`].
//!
//! A transfer is a strictly-ordered sequence of frames on one link:
//!
//! ```text
//! Meta(name,size,chunk_size)  →  Chunk … Chunk  →  Control::Complete
//! ```
//!
//! Because a [`Link`] preserves order, chunks carry no index — the receiver
//! appends them in arrival order. Chunk bytes ride in the raw
//! [`Frame::payload`] (no base64, no JSON wrapping) so there is no per-chunk
//! bloat. Metadata and control messages are small JSON payloads.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind};

/// Metadata announced once at the start of a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferMeta {
    /// Unique id for this transfer.
    pub transfer_id: String,
    /// File name (base name only is used by the receiver).
    pub name: String,
    /// Total size in bytes (informational; `0` if unknown/streamed).
    pub size: u64,
    /// Sender's chunk size in bytes.
    pub chunk_size: u32,
}

/// A control message (small JSON in a [`FrameKind::Control`] frame).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// Receiver → sender: bytes already on disk; the sender resumes from here.
    ResumeAck { offset: u64 },
    /// Sender → receiver: the whole file has been sent; carries the SHA-256
    /// checksum of the complete file for integrity verification.
    Complete { checksum: String },
    /// Receiver → sender: whether the received file matched the checksum.
    Verify { ok: bool },
    /// Either side is aborting the transfer.
    Cancel,
    /// Sender → receiver: cooperative pause — the sender has stopped sending
    /// chunks; the receiver should stop too so both sides show "paused".
    /// Sent once per local pause edge (see `stream::send_file`'s
    /// `signalled_pause` tracking) — never polled or retransmitted, so it
    /// cannot loop.
    Pause,
    /// Sender → receiver: cooperative resume, the counterpart to [`Pause`](Control::Pause).
    Resume,
}

/// Sentinel values on the receiver→sender progress back-channel
/// ([`peerbeam_domain::port::ProgressSink`]/`ProgressSource`), which otherwise
/// only ever carries real received-byte counts. Reserved from the very top of
/// the `u64` range: a file would need to be exabytes in size for a real byte
/// count to collide with either value, which is not a real-world concern.
///
/// These mirror [`Control::Pause`]/[`Control::Resume`] in the other
/// direction — receiver → sender — so a receiver-side pause stops the sender
/// even though the sender only reads the main stream at the very start and
/// end of a transfer (see the `peerbeam-ffi` `drive()` back-channel wiring).
pub const BACK_PAUSE: u64 = u64::MAX;
pub const BACK_RESUME: u64 = u64::MAX - 1;

/// Build the opening metadata frame.
pub fn meta_frame(meta: &TransferMeta) -> Frame {
    Frame {
        kind: FrameKind::Meta,
        payload: Bytes::from(serde_json::to_vec(meta).expect("TransferMeta is serializable")),
    }
}

/// Build a data chunk frame from raw bytes (copies `data`).
///
/// Prefer [`chunk_frame_owned`] on the hot send path: it moves an owned buffer
/// into the frame with no per-chunk copy. This borrowing variant stays for
/// callers that only hold a slice (e.g. small/one-shot payloads).
pub fn chunk_frame(data: &[u8]) -> Frame {
    Frame {
        kind: FrameKind::Chunk,
        payload: Bytes::copy_from_slice(data),
    }
}

/// Build a data chunk frame from an owned buffer with no copy.
pub fn chunk_frame_owned(payload: Bytes) -> Frame {
    Frame {
        kind: FrameKind::Chunk,
        payload,
    }
}

/// Build a control frame.
pub fn control_frame(control: &Control) -> Frame {
    Frame {
        kind: FrameKind::Control,
        payload: Bytes::from(serde_json::to_vec(control).expect("Control is serializable")),
    }
}

/// Parse a metadata frame.
pub fn parse_meta(frame: &Frame) -> Result<TransferMeta> {
    serde_json::from_slice(&frame.payload)
        .map_err(|e| DomainError::Transfer(format!("bad transfer meta: {e}")))
}

/// Parse a control frame.
pub fn parse_control(frame: &Frame) -> Result<Control> {
    serde_json::from_slice(&frame.payload)
        .map_err(|e| DomainError::Transfer(format!("bad control: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let meta = TransferMeta {
            transfer_id: "t1".into(),
            name: "movie.mkv".into(),
            size: 1_000_000,
            chunk_size: 65536,
        };
        let frame = meta_frame(&meta);
        assert_eq!(frame.kind, FrameKind::Meta);
        assert_eq!(parse_meta(&frame).unwrap(), meta);
    }

    #[test]
    fn control_roundtrip() {
        for c in [
            Control::ResumeAck { offset: 42 },
            Control::Complete {
                checksum: "abc123".into(),
            },
            Control::Verify { ok: true },
            Control::Cancel,
            Control::Pause,
            Control::Resume,
        ] {
            let frame = control_frame(&c);
            assert_eq!(frame.kind, FrameKind::Control);
            assert_eq!(parse_control(&frame).unwrap(), c);
        }
    }

    #[test]
    fn back_channel_sentinels_are_distinct_and_never_real_byte_counts() {
        // Real received-byte counts come from `frame.payload.len()` sums over
        // an actual transfer; both reserved values sit at the very top of the
        // `u64` range, far past anything reachable in practice.
        assert_ne!(BACK_PAUSE, BACK_RESUME);
        assert_eq!(BACK_PAUSE, u64::MAX);
        assert_eq!(BACK_RESUME, u64::MAX - 1);
    }

    #[test]
    fn chunk_frame_carries_raw_bytes() {
        let data = vec![1u8, 2, 3, 4];
        let frame = chunk_frame(&data);
        assert_eq!(frame.kind, FrameKind::Chunk);
        assert_eq!(&frame.payload[..], &data[..]);
    }

    #[test]
    fn chunk_frame_owned_matches_borrowed_variant() {
        // The zero-copy send path must produce a byte-identical frame to the
        // copying helper — same kind, same payload.
        let data = vec![9u8, 8, 7, 6, 5];
        let borrowed = chunk_frame(&data);
        let owned = chunk_frame_owned(Bytes::from(data.clone()));
        assert_eq!(owned.kind, borrowed.kind);
        assert_eq!(owned.payload, borrowed.payload);
        assert_eq!(&owned.payload[..], &data[..]);
    }

    #[test]
    fn parse_rejects_garbage() {
        let bad = Frame {
            kind: FrameKind::Meta,
            payload: Bytes::from_static(b"not json"),
        };
        assert!(parse_meta(&bad).is_err());
        assert!(parse_control(&bad).is_err());
    }
}
