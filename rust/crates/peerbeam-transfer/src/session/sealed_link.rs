//! An owned [`Link`] that seals/opens frames with a channel's [`ChannelCrypto`].
//!
//! [`crate::SecureLink`] borrows its inner link (`&mut dyn Link`); a channel's
//! transfer task instead needs to *own* the stream so it can be moved into the
//! task and handed to the transfer engine. `SealedLink` is that owned wrapper —
//! same sealing scheme (via [`ChannelCrypto`]) and the same frame codec as
//! `SecureLink`, so `send_file`/`receive_file` run over a per-channel-keyed
//! PeerSession stream unchanged.
//!
//! The per-channel progress back-channel is intentionally not exposed
//! (`progress_sink`/`progress_source` stay `None`): it is a connection-level
//! uni-stream and cannot be cleanly attributed to one channel when several
//! transfers share a connection, so channel transfers report bytes-sent progress.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link};
use peerbeam_domain::session::SessionError;

use super::crypto::ChannelCrypto;
use crate::secure::{decode_frame, encode_frame};

/// An owned link that seals every frame with a channel's own key.
pub(crate) struct SealedLink {
    inner: Box<dyn Link>,
    crypto: ChannelCrypto,
    enc: Arc<dyn EncryptionProvider>,
}

impl SealedLink {
    /// Wrap `inner` with the channel's crypto context.
    pub(crate) fn new(
        inner: Box<dyn Link>,
        crypto: ChannelCrypto,
        enc: Arc<dyn EncryptionProvider>,
    ) -> Self {
        SealedLink { inner, crypto, enc }
    }
}

fn to_dom(e: SessionError) -> DomainError {
    DomainError::Integrity(e.to_string())
}

#[async_trait]
impl Link for SealedLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        // Seal with the current counter; advance only after a successful send
        // (retry-safe, matching SecureLink).
        let sealed = self
            .crypto
            .seal(&*self.enc, &encode_frame(&frame))
            .map_err(to_dom)?;
        self.inner
            .send_frame(Frame {
                kind: FrameKind::Control,
                payload: Bytes::from(sealed),
            })
            .await?;
        self.crypto.advance_send().map_err(to_dom)?;
        Ok(())
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let Some(outer) = self.inner.recv_frame().await? else {
            return Ok(None);
        };
        let plain = self
            .crypto
            .open(&*self.enc, &outer.payload)
            .map_err(to_dom)?;
        Ok(Some(decode_frame(&plain)?))
    }

    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }

}
