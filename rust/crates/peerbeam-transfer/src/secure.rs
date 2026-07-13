//! Authenticated, replay-protected framing over a [`Link`].
//!
//! `SecureLink` wraps a raw link and an authenticated [`Session`]. Every
//! outgoing frame is sealed with AES-256-GCM under the session's send key and
//! a **monotonic-counter nonce**; every incoming frame must carry the next
//! expected counter and pass GCM verification, or it is rejected.
//!
//! This gives, transparently to whatever runs on top (file, folder, or
//! clipboard transfer):
//!
//! - **Keyed integrity** — the GCM tag authenticates each frame; a flipped
//!   bit fails to open.
//! - **Replay / reorder protection** — a duplicated or out-of-order frame
//!   carries the wrong counter and is refused.
//! - **Confidentiality** — frame contents are encrypted on the wire.

use async_trait::async_trait;
use bytes::Bytes;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link, Nonce};

use crate::auth::Session;

/// A [`Link`] that seals/opens frames using an authenticated [`Session`].
pub struct SecureLink<'a> {
    inner: &'a mut dyn Link,
    enc: &'a dyn EncryptionProvider,
    session: Session,
    send_ctr: u64,
    recv_ctr: u64,
}

impl<'a> SecureLink<'a> {
    /// Wrap `inner` with the authenticated `session`.
    pub fn new(inner: &'a mut dyn Link, enc: &'a dyn EncryptionProvider, session: Session) -> Self {
        Self {
            inner,
            enc,
            session,
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    fn nonce(prefix: [u8; 4], ctr: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&prefix);
        n[4..].copy_from_slice(&ctr.to_be_bytes());
        Nonce(n)
    }
}

#[async_trait]
impl Link for SecureLink<'_> {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        let nonce = Self::nonce(self.session.send_prefix, self.send_ctr);
        let sealed = self
            .enc
            .seal(&self.session.send_key, &nonce, &encode_frame(&frame))?;
        self.send_ctr += 1;
        self.inner
            .send_frame(Frame {
                kind: FrameKind::Control,
                payload: Bytes::from(sealed),
            })
            .await
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let Some(outer) = self.inner.recv_frame().await? else {
            return Ok(None);
        };
        let payload = &outer.payload;
        if payload.len() < 12 {
            return Err(DomainError::Integrity("secure frame too short".into()));
        }

        // The nonce (prefix + counter) is prepended by `seal`. Reject any
        // frame that isn't the next expected one before spending a decrypt.
        let got_prefix = &payload[..4];
        let got_ctr = u64::from_be_bytes(payload[4..12].try_into().expect("8 bytes"));
        if got_prefix != self.session.recv_prefix || got_ctr != self.recv_ctr {
            return Err(DomainError::Integrity(
                "replayed, reordered, or forged frame".into(),
            ));
        }

        let plain = self.enc.open(&self.session.recv_key, payload)?;
        self.recv_ctr += 1;
        Ok(Some(decode_frame(&plain)?))
    }

    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

/// Serialize a frame as `[kind tag byte] || payload`.
fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + frame.payload.len());
    out.push(kind_tag(frame.kind));
    out.extend_from_slice(&frame.payload);
    out
}

fn decode_frame(bytes: &[u8]) -> Result<Frame> {
    let (tag, payload) = bytes
        .split_first()
        .ok_or_else(|| DomainError::Integrity("empty secure frame".into()))?;
    Ok(Frame {
        kind: tag_kind(*tag)?,
        payload: Bytes::copy_from_slice(payload),
    })
}

fn kind_tag(kind: FrameKind) -> u8 {
    match kind {
        FrameKind::Handshake => 0,
        FrameKind::Meta => 1,
        FrameKind::Chunk => 2,
        FrameKind::Ack => 3,
        FrameKind::Control => 4,
    }
}

fn tag_kind(tag: u8) -> Result<FrameKind> {
    Ok(match tag {
        0 => FrameKind::Handshake,
        1 => FrameKind::Meta,
        2 => FrameKind::Chunk,
        3 => FrameKind::Ack,
        4 => FrameKind::Control,
        other => return Err(DomainError::Integrity(format!("unknown frame tag {other}"))),
    })
}
