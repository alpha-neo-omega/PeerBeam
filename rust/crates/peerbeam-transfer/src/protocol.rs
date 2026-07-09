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
    /// Cumulative bytes received (receiver → sender), for progress/backpressure.
    Ack { received: u64 },
    /// The sender has sent the whole file.
    Complete,
    /// Either side is aborting the transfer.
    Cancel,
}

/// Build the opening metadata frame.
pub fn meta_frame(meta: &TransferMeta) -> Frame {
    Frame {
        kind: FrameKind::Meta,
        payload: Bytes::from(serde_json::to_vec(meta).expect("TransferMeta is serializable")),
    }
}

/// Build a data chunk frame from raw bytes.
pub fn chunk_frame(data: &[u8]) -> Frame {
    Frame {
        kind: FrameKind::Chunk,
        payload: Bytes::copy_from_slice(data),
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
            Control::Ack { received: 42 },
            Control::Complete,
            Control::Cancel,
        ] {
            let frame = control_frame(&c);
            assert_eq!(frame.kind, FrameKind::Control);
            assert_eq!(parse_control(&frame).unwrap(), c);
        }
    }

    #[test]
    fn chunk_frame_carries_raw_bytes() {
        let data = vec![1u8, 2, 3, 4];
        let frame = chunk_frame(&data);
        assert_eq!(frame.kind, FrameKind::Chunk);
        assert_eq!(&frame.payload[..], &data[..]);
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
