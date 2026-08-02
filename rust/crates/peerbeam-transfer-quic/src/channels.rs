//! A QUIC connection presented as a [`ChannelTransport`].
//!
//! Multiplexing reuses QUIC's native bidirectional streams: opening a channel is
//! `open_bi`, accepting one is `accept_bi`, and each stream is wrapped as the
//! existing [`QuicLink`]. There is no custom multiplexing protocol — the
//! transport already multiplexes.

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
