//! Shared harness for transfer integration tests: an in-memory bounded `Link`
//! and small data helpers. Kept in `tests/common/` so it is compiled as a
//! module of each test binary rather than as its own test target.
#![allow(dead_code)]

use async_trait::async_trait;
use tokio::sync::mpsc;

use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, Link};

/// One end of an in-memory duplex link backed by bounded channels, so the
/// channel exerts backpressure like a real socket.
pub struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl MemLink {
    /// A connected pair. `cap` bounds in-flight frames (backpressure).
    pub fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a_tx, b_rx) = mpsc::channel(cap);
        let (b_tx, a_rx) = mpsc::channel(cap);
        (
            MemLink { tx: a_tx, rx: a_rx },
            MemLink { tx: b_tx, rx: b_rx },
        )
    }
}

#[async_trait]
impl Link for MemLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| DomainError::Connection("peer closed".into()))
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        Ok(self.rx.recv().await)
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Deterministic, position-dependent byte pattern (period 251, a prime, so
/// off-by-one and misordering are caught).
pub fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Drain all currently-queued progress updates.
pub fn drain(rx: &mut mpsc::UnboundedReceiver<Progress>) -> Vec<Progress> {
    let mut out = Vec::new();
    while let Ok(p) = rx.try_recv() {
        out.push(p);
    }
    out
}

/// An in-memory `ChannelTransport`: opening a stream hands the peer a connected
/// [`MemLink`] end and returns the local end; accepting yields the streams the
/// peer opened, in order (FIFO).
pub struct MemTransport {
    peer_accept: mpsc::UnboundedSender<Box<dyn Link>>,
    my_accept: tokio::sync::Mutex<mpsc::UnboundedReceiver<Box<dyn Link>>>,
}

impl MemTransport {
    /// A connected pair of transports.
    pub fn pair() -> (std::sync::Arc<MemTransport>, std::sync::Arc<MemTransport>) {
        let (a_to_b, b_recv) = mpsc::unbounded_channel();
        let (b_to_a, a_recv) = mpsc::unbounded_channel();
        let a = std::sync::Arc::new(MemTransport {
            peer_accept: a_to_b,
            my_accept: tokio::sync::Mutex::new(a_recv),
        });
        let b = std::sync::Arc::new(MemTransport {
            peer_accept: b_to_a,
            my_accept: tokio::sync::Mutex::new(b_recv),
        });
        (a, b)
    }
}

#[async_trait]
impl peerbeam_domain::port::ChannelTransport for MemTransport {
    async fn open_stream(&self) -> Result<Box<dyn Link>> {
        let (mine, theirs) = MemLink::pair(32);
        self.peer_accept
            .send(Box::new(theirs))
            .map_err(|_| DomainError::Connection("peer transport closed".into()))?;
        Ok(Box::new(mine))
    }

    async fn accept_stream(&self) -> Result<Option<Box<dyn Link>>> {
        let mut rx = self.my_accept.lock().await;
        Ok(rx.recv().await)
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
