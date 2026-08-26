//! [`Link`] adapters that replay one already-read frame.
//!
//! Receivers peek the first frame to dispatch file vs folder transfers
//! without the sender protocol knowing; the wrapped link then behaves as if
//! the frame had never been read.
//!
//! Two flavours, differing only in how they hold the link they wrap:
//!
//! - [`PeekLink`] **borrows** (`&mut dyn Link`) — for a caller that already
//!   owns the link for the whole transfer and only needs the replay to live
//!   as long as one receive call (see `session::receive_on_channel`).
//! - [`OwnedPeekLink`] **owns** (`Box<dyn Link>`) — for a caller that must
//!   hand the peeked link *back* (see `session::peek_incoming_meta`).

use async_trait::async_trait;

use peerbeam_domain::error::Result;
use peerbeam_domain::port::{Frame, Link};

/// Replays [`first`](Self::new) before delegating every call to the inner
/// link.
pub struct PeekLink<'a> {
    first: Option<Frame>,
    inner: &'a mut dyn Link,
}

impl<'a> PeekLink<'a> {
    /// Wrap `inner`, replaying `first` on the next `recv_frame`.
    pub fn new(first: Frame, inner: &'a mut dyn Link) -> Self {
        Self {
            first: Some(first),
            inner,
        }
    }
}

#[async_trait]
impl Link for PeekLink<'_> {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        if let Some(f) = self.first.take() {
            Ok(Some(f))
        } else {
            self.inner.recv_frame().await
        }
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

/// Replays [`first`](Self::new) before delegating every call to the inner
/// link, **owning** that link rather than borrowing it.
///
/// The borrowing [`PeekLink`] cannot be used where the peeked link has to be
/// handed back to the caller: an
/// [`IncomingStreamChannel`](crate::IncomingStreamChannel) stores its link as
/// an owned `Box<dyn Link>`, and a borrow cannot be stored there (it would
/// have to outlive the local it borrows from). `OwnedPeekLink` takes the box
/// instead and is itself boxable straight back into that field — which is what
/// lets `session::peek_incoming_meta` read the first frame and still return a
/// channel that behaves exactly as though nothing had been read.
///
/// The replay is byte-for-byte the same mechanism as [`PeekLink`]'s: the very
/// next `recv_frame` yields the peeked frame, every later one goes to the
/// inner link, and `send_frame`/`close` are always delegated.
pub struct OwnedPeekLink {
    first: Option<Frame>,
    inner: Box<dyn Link>,
}

impl OwnedPeekLink {
    /// Take ownership of `inner`, replaying `first` on the next `recv_frame`.
    #[must_use]
    pub fn new(first: Frame, inner: Box<dyn Link>) -> Self {
        Self {
            first: Some(first),
            inner,
        }
    }
}

#[async_trait]
impl Link for OwnedPeekLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        if let Some(f) = self.first.take() {
            Ok(Some(f))
        } else {
            self.inner.recv_frame().await
        }
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
