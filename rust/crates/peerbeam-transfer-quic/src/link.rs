//! A [`Link`] backed by one QUIC bidirectional stream.
//!
//! Frames are length-delimited on the stream:
//!
//! ```text
//! ┌──────────┬───────────────┬───────────────────────┐
//! │ kind: u8 │ len: u32 (BE) │ payload: len bytes     │
//! └──────────┴───────────────┴───────────────────────┘
//! ```
//!
//! QUIC already provides an ordered, reliable, congestion-controlled,
//! encrypted byte stream, so the codec only needs framing. Reads never
//! materialise more than one frame; the transfer engine above bounds frame
//! size to its chunk size.

use async_trait::async_trait;
use bytes::Bytes;
use quinn::{Connection, RecvStream, SendStream};

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link, ProgressSink, ProgressSource};

/// Upper bound on a single frame (defensive: a malformed/hostile peer cannot
/// make us allocate unbounded memory). Well above any real chunk size.
const MAX_FRAME: u32 = 64 * 1024 * 1024;

/// Reason code sent when closing the connection cleanly.
const CLOSE_OK: u32 = 0;

/// One live QUIC connection presented as a framed [`Link`].
pub struct QuicLink {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl QuicLink {
    /// Wrap an opened/accepted bidirectional stream on `conn`.
    pub(crate) fn new(conn: Connection, send: SendStream, recv: RecvStream) -> Self {
        Self { conn, send, recv }
    }

    /// The peer's remote address (for logging).
    pub fn remote(&self) -> std::net::SocketAddr {
        self.conn.remote_address()
    }
}

fn kind_to_u8(k: FrameKind) -> u8 {
    match k {
        FrameKind::Handshake => 0,
        FrameKind::Meta => 1,
        FrameKind::Chunk => 2,
        FrameKind::Ack => 3,
        FrameKind::Control => 4,
    }
}

fn u8_to_kind(b: u8) -> Result<FrameKind> {
    Ok(match b {
        0 => FrameKind::Handshake,
        1 => FrameKind::Meta,
        2 => FrameKind::Chunk,
        3 => FrameKind::Ack,
        4 => FrameKind::Control,
        other => return Err(DomainError::Transfer(format!("bad frame kind {other}"))),
    })
}

fn conn_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::Connection(format!("quic: {e}"))
}

#[async_trait]
impl Link for QuicLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        let len: u32 = frame
            .payload
            .len()
            .try_into()
            .map_err(|_| DomainError::Transfer("frame too large".into()))?;
        if len > MAX_FRAME {
            return Err(DomainError::Transfer("frame exceeds MAX_FRAME".into()));
        }
        let mut header = [0u8; 5];
        header[0] = kind_to_u8(frame.kind);
        header[1..5].copy_from_slice(&len.to_be_bytes());
        self.send.write_all(&header).await.map_err(conn_err)?;
        self.send
            .write_all(&frame.payload)
            .await
            .map_err(conn_err)?;
        Ok(())
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let mut header = [0u8; 5];
        match self.recv.read_exact(&mut header).await {
            Ok(()) => {}
            // Peer finished the stream at a frame boundary — clean close.
            Err(quinn::ReadExactError::FinishedEarly { .. }) => return Ok(None),
            Err(e) => return Err(conn_err(e)),
        }
        let kind = u8_to_kind(header[0])?;
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        if len > MAX_FRAME {
            return Err(DomainError::Transfer("frame exceeds MAX_FRAME".into()));
        }
        let mut payload = vec![0u8; len as usize];
        // A truncated payload after a header is a hard error, not a clean EOF.
        self.recv.read_exact(&mut payload).await.map_err(conn_err)?;
        Ok(Some(Frame {
            kind,
            payload: Bytes::from(payload),
        }))
    }

    async fn close(&mut self) -> Result<()> {
        // Best-effort: finish our send side, then close the connection.
        let _ = self.send.finish();
        self.conn.close(CLOSE_OK.into(), b"bye");
        Ok(())
    }

    fn progress_sink(&self) -> Option<Box<dyn ProgressSink>> {
        Some(Box::new(QuicProgressSink {
            conn: self.conn.clone(),
            stream: None,
        }))
    }

    fn progress_source(&self) -> Option<Box<dyn ProgressSource>> {
        Some(Box::new(QuicProgressSource {
            conn: self.conn.clone(),
            stream: None,
        }))
    }
}

/// Receiver side of the progress back-channel: a dedicated QUIC uni-stream on
/// the same (TLS-encrypted, already-authenticated) connection, carrying 8-byte
/// big-endian received-byte counts. Opened lazily on first report.
struct QuicProgressSink {
    conn: Connection,
    stream: Option<SendStream>,
}

#[async_trait]
impl ProgressSink for QuicProgressSink {
    async fn report(&mut self, received: u64) -> Result<()> {
        if self.stream.is_none() {
            self.stream = Some(self.conn.open_uni().await.map_err(conn_err)?);
        }
        let s = self.stream.as_mut().expect("opened above");
        s.write_all(&received.to_be_bytes()).await.map_err(conn_err)
    }
}

/// Sender side: accepts the peer's progress uni-stream and reads received-byte
/// counts. Accepts lazily on first `recv`.
struct QuicProgressSource {
    conn: Connection,
    stream: Option<RecvStream>,
}

#[async_trait]
impl ProgressSource for QuicProgressSource {
    async fn recv(&mut self) -> Result<Option<u64>> {
        if self.stream.is_none() {
            self.stream = Some(self.conn.accept_uni().await.map_err(conn_err)?);
        }
        let s = self.stream.as_mut().expect("accepted above");
        let mut buf = [0u8; 8];
        match s.read_exact(&mut buf).await {
            Ok(()) => Ok(Some(u64::from_be_bytes(buf))),
            Err(quinn::ReadExactError::FinishedEarly { .. }) => Ok(None),
            Err(e) => Err(conn_err(e)),
        }
    }
}
