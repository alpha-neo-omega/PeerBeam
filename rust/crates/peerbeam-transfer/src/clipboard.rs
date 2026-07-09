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
            let mut buf = Vec::with_capacity(size as usize);
            loop {
                match link.recv_frame().await? {
                    Some(frame) => match frame.kind {
                        FrameKind::Chunk => buf.extend_from_slice(&frame.payload),
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
}
