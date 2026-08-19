//! A QUIC connection presented as a [`ChannelTransport`].
//!
//! Multiplexing reuses QUIC's native bidirectional streams: opening a channel is
//! `open_bi`, accepting one is `accept_bi`, and each stream is wrapped as the
//! existing [`QuicLink`]. There is no custom multiplexing protocol — the
//! transport already multiplexes.

use std::time::Duration;

use async_trait::async_trait;
use quinn::Connection;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{ChannelTransport, Link};

use crate::QuicLink;

fn conn_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::Connection(format!("quic: {e}"))
}

/// One live QUIC connection exposed as a multi-channel transport. Each channel is
/// a bidirectional stream on the connection.
pub struct QuicChannels {
    conn: Connection,
}

impl QuicChannels {
    /// Wrap a connected/accepted [`Connection`].
    pub(crate) fn new(conn: Connection) -> Self {
        QuicChannels { conn }
    }

    /// The peer's remote address (for logging).
    #[must_use]
    pub fn remote(&self) -> std::net::SocketAddr {
        self.conn.remote_address()
    }

    /// The round-trip time QUIC has measured on this connection, or `None`
    /// while it has measured none.
    ///
    /// This is quinn's own smoothed RTT — the estimate its loss recovery
    /// already runs on — so reading it costs one lock and adds no probe
    /// traffic of its own. There is no PeerBeam-level ping, and there does not
    /// need to be one.
    ///
    /// **The `None` is the whole point of the signature.**
    /// [`quinn::Connection::rtt`] never fails: before the first ACK-driven
    /// sample it returns the *configured initial* RTT (333 ms, RFC 9002
    /// §6.2.2), which is a constant every connection starts life holding and
    /// not a measurement of anything. Reporting that would put "333 ms" beside
    /// a device on the same desk. `frame_rx.acks` counts ACK frames actually
    /// received, and an ACK is what drives quinn's estimator, so a zero there
    /// means no sample can have been taken and there is nothing honest to say.
    #[must_use]
    pub fn rtt(&self) -> Option<Duration> {
        let stats = self.conn.stats();
        (stats.frame_rx.acks > 0).then_some(stats.path.rtt)
    }
}

#[async_trait]
impl ChannelTransport for QuicChannels {
    async fn open_stream(&self) -> Result<Box<dyn Link>> {
        let (send, recv) = self.conn.open_bi().await.map_err(conn_err)?;
        Ok(Box::new(QuicLink::new(self.conn.clone(), send, recv)))
    }

    async fn accept_stream(&self) -> Result<Option<Box<dyn Link>>> {
        match self.conn.accept_bi().await {
            Ok((send, recv)) => Ok(Some(Box::new(QuicLink::new(self.conn.clone(), send, recv)))),
            // A closed connection is a clean end of the stream sequence.
            Err(quinn::ConnectionError::LocallyClosed)
            | Err(quinn::ConnectionError::ApplicationClosed(_)) => Ok(None),
            Err(e) => Err(conn_err(e)),
        }
    }

    async fn close(&self) -> Result<()> {
        self.conn.close(0u32.into(), b"bye");
        Ok(())
    }
}
