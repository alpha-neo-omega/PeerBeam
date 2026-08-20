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

use std::time::Duration;

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

/// Resumable state of an in-progress frame read, held in [`QuicLink`] so that a
/// [`QuicLink::recv_frame`] future dropped mid-frame (e.g. a `tokio::select!`
/// branch losing the race) loses no bytes: the next call continues from here.
/// This makes `recv_frame` **cancellation-safe**, which `read_exact` is not.
enum RecvState {
    /// Between frames — nothing read yet.
    Idle,
    /// Reading the 5-byte length-delimited header.
    Header { buf: [u8; 5], filled: usize },
    /// Header parsed; reading the `payload` (`filled` of `buf.len()` bytes read).
    Payload {
        kind: FrameKind,
        buf: Vec<u8>,
        filled: usize,
    },
}

/// One live QUIC connection presented as a framed [`Link`].
pub struct QuicLink {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    recv_state: RecvState,
}

impl QuicLink {
    /// Wrap an opened/accepted bidirectional stream on `conn`.
    pub(crate) fn new(conn: Connection, send: SendStream, recv: RecvStream) -> Self {
        Self {
            conn,
            send,
            recv,
            recv_state: RecvState::Idle,
        }
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

/// The same, for a quinn error whose message hides the reason.
///
/// `WriteError`/`ReadError` render `ConnectionLost` as the bare text "connection
/// lost" and keep the `ConnectionError` that says *why* — timed out, reset by
/// peer, closed by peer, transport error — only in `source()`. A log line saying
/// a transfer failed because the connection was lost, with no cause, cannot tell
/// a starved CPU from a peer that hung up from a protocol violation.
fn conn_err_caused(e: &dyn std::error::Error) -> DomainError {
    let mut msg = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        msg.push_str(": ");
        msg.push_str(&c.to_string());
        cause = c.source();
    }
    DomainError::Connection(format!("quic: {msg}"))
}

/// Outcome of [`fill`].
enum FillOutcome {
    /// `buf` was filled to `buf.len()`.
    Filled,
    /// The stream finished before `buf` was full (peer closed its send side).
    Eof,
}

/// Read from `recv` into `buf[*filled..]` until `buf` is full or the stream ends,
/// advancing `*filled` as bytes arrive. Cancellation-safe: `RecvStream::read`
/// suspends only when *no* bytes are available (so a dropped-while-pending future
/// consumes nothing), and every byte it returns is recorded in `*filled` before
/// the next await — so re-calling with the same `buf`/`filled` resumes cleanly.
async fn fill(recv: &mut RecvStream, buf: &mut [u8], filled: &mut usize) -> Result<FillOutcome> {
    while *filled < buf.len() {
        match recv
            .read(&mut buf[*filled..])
            .await
            .map_err(|e| conn_err_caused(&e))?
        {
            Some(0) | None => return Ok(FillOutcome::Eof),
            Some(n) => *filled += n,
        }
    }
    Ok(FillOutcome::Filled)
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
        self.send
            .write_all(&header)
            .await
            .map_err(|e| conn_err_caused(&e))?;
        self.send
            .write_all(&frame.payload)
            .await
            .map_err(|e| conn_err_caused(&e))?;
        Ok(())
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        // Cancellation-safe, resumable read: all partial progress lives in
        // `self.recv_state`, so a future dropped mid-frame (a losing select!
        // branch) resumes here on the next call instead of desyncing the stream.
        loop {
            if matches!(self.recv_state, RecvState::Idle) {
                self.recv_state = RecvState::Header {
                    buf: [0u8; 5],
                    filled: 0,
                };
            }
            match &mut self.recv_state {
                RecvState::Idle => unreachable!("set to Header above"),
                RecvState::Header { buf, filled } => {
                    match fill(&mut self.recv, &mut buf[..], filled).await? {
                        // EOF at a frame boundary (nothing read) is a clean close;
                        // a partially-read header is a truncation error.
                        FillOutcome::Eof if *filled == 0 => {
                            self.recv_state = RecvState::Idle;
                            return Ok(None);
                        }
                        FillOutcome::Eof => {
                            self.recv_state = RecvState::Idle;
                            return Err(conn_err("stream ended mid-header"));
                        }
                        FillOutcome::Filled => {
                            let kind = u8_to_kind(buf[0])?;
                            let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
                            if len > MAX_FRAME {
                                self.recv_state = RecvState::Idle;
                                return Err(DomainError::Transfer(
                                    "frame exceeds MAX_FRAME".into(),
                                ));
                            }
                            self.recv_state = RecvState::Payload {
                                kind,
                                buf: vec![0u8; len as usize],
                                filled: 0,
                            };
                        }
                    }
                }
                RecvState::Payload { buf, filled, .. } => {
                    match fill(&mut self.recv, &mut buf[..], filled).await? {
                        // A truncated payload after a header is a hard error.
                        FillOutcome::Eof => {
                            self.recv_state = RecvState::Idle;
                            return Err(conn_err("stream ended mid-payload"));
                        }
                        FillOutcome::Filled => {
                            let RecvState::Payload { kind, buf, .. } =
                                std::mem::replace(&mut self.recv_state, RecvState::Idle)
                            else {
                                unreachable!("matched Payload above")
                            };
                            return Ok(Some(Frame {
                                kind,
                                payload: Bytes::from(buf),
                            }));
                        }
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<()> {
        // Best-effort: finish our send side, then close the connection.
        let _ = self.send.finish();
        self.conn.close(CLOSE_OK.into(), b"bye");
        Ok(())
    }

    async fn graceful_close(&mut self) -> Result<()> {
        // Deliver buffered data before closing: quinn's Connection::close sends
        // CONNECTION_CLOSE immediately and drops un-transmitted stream data, so a
        // final frame written just before it (e.g. a Shutdown control message)
        // would be lost and the peer would misread a clean close as a transport
        // loss. Finish the send side, then wait (bounded) for the peer to
        // acknowledge all data + FIN (`stopped()` resolves once acknowledged),
        // and only then close the connection. A gone/unresponsive peer just hits
        // the timeout, after which we close anyway.
        let _ = self.send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.send.stopped()).await;
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
