//! Clipboard transfer over a [`Link`].
//!
//! Sends a [`ClipboardItem`] to a peer. Text-like items (text/URL/code) go in
//! a single inline `Control` frame; images stream as `Chunk` frames between a
//! `BinaryMeta` header and `Complete`, so a large image never needs a single
//! giant frame.
//!
//! ```text
//! text/url/code:  Inline(item)
//! image:          BinaryMeta(mime, at, size) → Chunk … → Complete
//! ```

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use peerbeam_domain::entity::ClipboardItem;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link};

use crate::protocol::chunk_frame;
use crate::stream::send_with_retry;

/// Chunk size for streaming image payloads.
const CLIP_CHUNK: usize = 64 * 1024;

/// Upper bound on a clipboard image, both for the peer-declared size and the
/// actual bytes received. Clipboard content is small by nature; anything larger
/// is rejected rather than allocated, so a malicious `BinaryMeta { size }`
/// cannot exhaust memory.
const MAX_CLIP_BYTES: u64 = 64 * 1024 * 1024;

/// Clipboard control/metadata messages (carried in Control frames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum ClipMessage {
    /// A fully-inline text/URL/code item.
    Inline { item: ClipboardItem },
    /// Header for a binary (image) payload; chunks follow, then `Complete`.
    BinaryMeta {
        mime: String,
        at: DateTime<Utc>,
        size: u64,
    },
    /// End of a binary payload.
    Complete,
}

fn clip_frame(msg: &ClipMessage) -> Frame {
    Frame {
        kind: FrameKind::Control,
        payload: Bytes::from(serde_json::to_vec(msg).expect("ClipMessage serializable")),
    }
}

fn parse_clip(frame: &Frame) -> Result<ClipMessage> {
    serde_json::from_slice(&frame.payload)
        .map_err(|e| DomainError::Transfer(format!("bad clipboard message: {e}")))
}

/// Send a clipboard item to a peer over `link`.
pub async fn send_clipboard(link: &mut dyn Link, item: &ClipboardItem, retries: u32) -> Result<()> {
    match item.as_bytes() {
        // Binary (image): header, streamed chunks, complete.
        Some(bytes) => {
            let meta = ClipMessage::BinaryMeta {
                mime: item.mime.clone(),
                at: item.at,
                size: bytes.len() as u64,
            };
            send_with_retry(link, clip_frame(&meta), retries).await?;
            for chunk in bytes.chunks(CLIP_CHUNK) {
                send_with_retry(link, chunk_frame(chunk), retries).await?;
            }
            send_with_retry(link, clip_frame(&ClipMessage::Complete), retries).await?;
        }
        // Text/URL/code: one inline frame.
        None => {
            send_with_retry(
                link,
                clip_frame(&ClipMessage::Inline { item: item.clone() }),
                retries,
            )
            .await?;
        }
    }
    Ok(())
}

/// Receive a clipboard item from a peer over `link`.
pub async fn receive_clipboard(link: &mut dyn Link) -> Result<ClipboardItem> {
    // First frame is the clipboard control message.
    let msg = loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Control => break parse_clip(&frame)?,
            Some(_) => continue,
            None => return Err(DomainError::Transfer("link closed before clipboard".into())),
        }
    };

    match msg {
        ClipMessage::Inline { item } => Ok(item),
        ClipMessage::BinaryMeta { mime, at, size } => {
            // Never trust the peer's declared size for allocation. Reject an
            // oversized declaration outright, and cap the pre-allocation.
            if size > MAX_CLIP_BYTES {
                return Err(DomainError::Transfer(format!(
                    "clipboard payload too large: {size} bytes"
                )));
            }
            let mut buf = Vec::with_capacity(size.min(CLIP_CHUNK as u64) as usize);
            loop {
                match link.recv_frame().await? {
                    Some(frame) => match frame.kind {
                        FrameKind::Chunk => {
                            // Bound actual receipt too, in case the peer streams
                            // more than it declared (or never sends Complete).
                            if buf.len() as u64 + frame.payload.len() as u64 > MAX_CLIP_BYTES {
                                return Err(DomainError::Transfer(
                                    "clipboard payload exceeded limit".into(),
                                ));
                            }
                            buf.extend_from_slice(&frame.payload);
                        }
                        FrameKind::Control => match parse_clip(&frame)? {
                            ClipMessage::Complete => break,
                            _ => continue,
                        },
                        _ => {}
                    },
                    None => {
                        return Err(DomainError::Transfer(
                            "link closed before clipboard complete".into(),
                        ))
                    }
                }
            }
            Ok(ClipboardItem::image(buf, mime, at))
        }
        ClipMessage::Complete => Err(DomainError::Transfer(
            "unexpected clipboard Complete before meta".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn clip_message_roundtrips() {
        let msgs = vec![
            ClipMessage::Inline {
                item: ClipboardItem::text("https://x.com".into(), t0()),
            },
            ClipMessage::BinaryMeta {
                mime: "image/png".into(),
                at: t0(),
                size: 123,
            },
            ClipMessage::Complete,
        ];
        for m in msgs {
            assert_eq!(parse_clip(&clip_frame(&m)).unwrap(), m);
        }
    }

    /// A malicious `BinaryMeta { size }` must be rejected up front, never fed
    /// into `Vec::with_capacity` (which would try to allocate that much and
    /// abort the process). Regression for the clipboard alloc DoS.
    #[tokio::test]
    async fn rejects_oversized_declared_size() {
        use std::collections::VecDeque;

        struct MockLink(VecDeque<Frame>);
        #[async_trait::async_trait]
        impl Link for MockLink {
            async fn send_frame(&mut self, _f: Frame) -> Result<()> {
                Ok(())
            }
            async fn recv_frame(&mut self) -> Result<Option<Frame>> {
                Ok(self.0.pop_front())
            }
            async fn close(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let meta = clip_frame(&ClipMessage::BinaryMeta {
            mime: "image/png".into(),
            at: t0(),
            size: u64::MAX,
        });
        let mut link = MockLink(VecDeque::from([meta]));
        let err = receive_clipboard(&mut link).await.unwrap_err();
        assert!(matches!(err, DomainError::Transfer(_)));
    }
}
