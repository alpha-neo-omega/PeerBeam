//! A [`Link`] adapter that replays one already-read frame.
//!
//! Receivers peek the first frame to dispatch file vs folder transfers
//! without the sender protocol knowing; the wrapped link then behaves as if
//! the frame had never been read.

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
}
