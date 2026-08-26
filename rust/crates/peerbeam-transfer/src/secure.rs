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
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link};
use peerbeam_domain::session::SessionError;

use crate::auth::Session;
use crate::session::ChannelCrypto;

/// A [`Link`] that seals/opens frames using an authenticated [`Session`].
///
/// The per-frame sealing (AES-256-GCM, counter-nonce, strict replay) lives in
/// [`ChannelCrypto`], shared with the multiplexed session layer so there is one
/// implementation of the sealing scheme.
pub struct SecureLink<'a> {
    inner: &'a mut dyn Link,
    enc: &'a dyn EncryptionProvider,
    crypto: ChannelCrypto,
}

impl<'a> SecureLink<'a> {
    /// Wrap `inner` with the authenticated `session`.
    pub fn new(inner: &'a mut dyn Link, enc: &'a dyn EncryptionProvider, session: Session) -> Self {
        Self {
            inner,
            enc,
            crypto: ChannelCrypto::from_keys(
                session.send_key,
                session.recv_key,
                session.send_prefix,
                session.recv_prefix,
            ),
        }
    }
}

fn sess_to_dom(e: SessionError) -> DomainError {
    DomainError::Integrity(e.to_string())
}

#[async_trait]
impl Link for SecureLink<'_> {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        // Seal with the current counter; advance only after the inner send
        // succeeds so a retried send re-seals with the same nonce (the receiver
        // never advanced) rather than desyncing.
        let sealed = self
            .crypto
            .seal(self.enc, &encode_frame(&frame))
            .map_err(sess_to_dom)?;
        self.inner
            .send_frame(Frame {
                kind: FrameKind::Control,
                payload: Bytes::from(sealed),
            })
            .await?;
        self.crypto.advance_send().map_err(sess_to_dom)?;
        Ok(())
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let Some(outer) = self.inner.recv_frame().await? else {
            return Ok(None);
        };
        let plain = self
            .crypto
            .open(self.enc, &outer.payload)
            .map_err(sess_to_dom)?;
        Ok(Some(decode_frame(&plain)?))
    }

    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }

    /// Forwarded, not inherited. The trait's default `graceful_close` calls
    /// `close`, which here is the **abrupt** close — so a wrapper that did not
    /// forward this silently downgraded every caller that asked for delivery of
    /// a final frame, including the session's own `Shutdown`. The wrapper adds
    /// framing, not a close policy.
    async fn graceful_close(&mut self) -> Result<()> {
        self.inner.graceful_close().await
    }
}

/// Serialize a frame as `[kind tag byte] || payload`. Shared with the session
/// layer's owned [`SealedLink`](crate::session::SealedLink) so there is one
/// frame codec for the sealing scheme.
pub(crate) fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + frame.payload.len());
    out.push(kind_tag(frame.kind));
    out.extend_from_slice(&frame.payload);
    out
}

pub(crate) fn decode_frame(bytes: &[u8]) -> Result<Frame> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::stream::send_with_retry;
    use peerbeam_crypto::AeadCrypto;
    use peerbeam_domain::id::DeviceId;

    /// Inner link that fails its first `send_frame`, then delivers every frame
    /// into `wire`. Models a transient send error that succeeds on retry.
    struct FlakySender {
        wire: Vec<Frame>,
        fail_first: bool,
    }

    #[async_trait]
    impl Link for FlakySender {
        async fn send_frame(&mut self, frame: Frame) -> Result<()> {
            if self.fail_first {
                self.fail_first = false;
                return Err(DomainError::Connection("transient".into()));
            }
            self.wire.push(frame);
            Ok(())
        }
        async fn recv_frame(&mut self) -> Result<Option<Frame>> {
            Ok(None)
        }
        async fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }

    /// Inner link that replays a fixed list of frames for the receiver.
    struct Replayer(std::vec::IntoIter<Frame>);

    #[async_trait]
    impl Link for Replayer {
        async fn send_frame(&mut self, _f: Frame) -> Result<()> {
            Ok(())
        }
        async fn recv_frame(&mut self) -> Result<Option<Frame>> {
            Ok(self.0.next())
        }
        async fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn sessions() -> (Session, Session) {
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        let p1 = [9u8; 4];
        let p2 = [7u8; 4];
        let send = Session {
            send_key: k1,
            recv_key: k2,
            send_prefix: p1,
            recv_prefix: p2,
            peer_id: DeviceId::from("peer"),
            peer_name: "peer".into(),
            newly_trusted: false,
            pairing_code: String::new(),
            transcript: Vec::new(),
        };
        let recv = Session {
            send_key: k2,
            recv_key: k1,
            send_prefix: p2,
            recv_prefix: p1,
            peer_id: DeviceId::from("me"),
            peer_name: "me".into(),
            newly_trusted: false,
            pairing_code: String::new(),
            transcript: Vec::new(),
        };
        (send, recv)
    }

    /// A send that fails once and is retried must NOT advance the nonce counter
    /// on the failed attempt — otherwise the receiver rejects the re-sent frame
    /// (and everything after) as reordered. Regression for the counter/nonce
    /// desync in `send_frame`.
    #[tokio::test]
    async fn retry_after_transient_send_error_keeps_counter_in_sync() {
        let enc = AeadCrypto::new();
        let (send_sess, recv_sess) = sessions();

        // Sender over a link that drops the first send, then delivers.
        let mut inner = FlakySender {
            wire: Vec::new(),
            fail_first: true,
        };
        {
            let mut secure = SecureLink::new(&mut inner, &enc, send_sess);
            for i in 0..3u8 {
                let frame = Frame {
                    kind: FrameKind::Chunk,
                    payload: Bytes::from(vec![i; 16]),
                };
                // retries = 1: the first frame's initial send fails, retry wins.
                send_with_retry(&mut secure, frame, 1).await.unwrap();
            }
        }
        // Exactly three frames reached the wire (the failed attempt delivered nothing).
        assert_eq!(inner.wire.len(), 3);

        // Receiver decodes all three in order — proves counters stayed aligned.
        let mut replay = Replayer(inner.wire.into_iter());
        let mut secure = SecureLink::new(&mut replay, &enc, recv_sess);
        for i in 0..3u8 {
            let got = secure.recv_frame().await.unwrap().expect("frame present");
            assert_eq!(got.kind, FrameKind::Chunk);
            assert_eq!(&got.payload[..], &vec![i; 16][..]);
        }
    }
}
