//! Transfer port: how bytes move between peers.
//!
//! The payload is a binary [`Frame`] — never JSON or base64. Protocols
//! (QUIC, TCP, WebRTC) implement [`TransferProvider`]; a live connection
//! is a [`Link`].

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;

use crate::entity::{Route, TransferSession};
use crate::error::Result;
use crate::id::ProviderId;

/// The transport protocol a provider implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Quic,
    Tcp,
    WebRtc,
}

/// The semantic class of a wire frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Authenticated identity handshake.
    Handshake,
    /// Transfer/file metadata.
    Meta,
    /// A file data chunk.
    Chunk,
    /// An acknowledgement.
    Ack,
    /// Control (pause/resume/cancel).
    Control,
}

/// One framed message on a [`Link`]. Payload is raw bytes.
#[derive(Debug, Clone)]
pub struct Frame {
    /// What kind of frame this is.
    pub kind: FrameKind,
    /// Binary payload (already sealed/compressed by the caller as needed).
    pub payload: Bytes,
}

/// Where a transfer server should listen.
#[derive(Debug, Clone, Copy)]
pub struct Bind {
    /// Port to bind (0 = OS-assigned).
    pub port: u16,
}

/// Receiver → sender live progress back-channel: the **receiver** writes how
/// many bytes it has actually taken in, so the **sender** can show the peer's
/// real progress instead of just bytes-handed-to-the-transport (which, over a
/// slow internet/Tailscale link, reaches 100% long before the receiver does).
///
/// It is a side-channel, independent of the frame stream — reporting must never
/// block or interfere with the transfer, so implementations run it on their own
/// resource (e.g. a separate QUIC stream) and callers drive it from a separate
/// task.
#[async_trait]
pub trait ProgressSink: Send {
    /// Report the total bytes received so far (monotonic). Best-effort.
    async fn report(&mut self, received: u64) -> Result<()>;
}

/// The sender's read end of the [`ProgressSink`] back-channel.
#[async_trait]
pub trait ProgressSource: Send {
    /// Next reported received-byte count, or `None` when the channel closes.
    async fn recv(&mut self) -> Result<Option<u64>>;
}

/// A single live connection to a peer.
#[async_trait]
pub trait Link: Send + Sync {
    /// Send one frame.
    async fn send_frame(&mut self, frame: Frame) -> Result<()>;

    /// Receive the next frame, or `None` when the peer closes cleanly.
    async fn recv_frame(&mut self) -> Result<Option<Frame>>;

    /// Close the connection.
    async fn close(&mut self) -> Result<()>;

    /// Receiver side: open the progress back-channel to report received bytes.
    /// `None` if the transport doesn't support it (falls back to bytes-sent).
    /// Owned so it can be driven concurrently with frame I/O.
    fn progress_sink(&self) -> Option<Box<dyn ProgressSink>> {
        None
    }

    /// Sender side: the read end of the peer's progress back-channel. `None` if
    /// unsupported. Owned so it can be read concurrently with frame I/O.
    fn progress_source(&self) -> Option<Box<dyn ProgressSource>> {
        None
    }
}

/// A transport that can dial peers and accept inbound connections.
#[async_trait]
pub trait TransferProvider: Send + Sync {
    /// Stable id of this provider instance.
    fn id(&self) -> ProviderId;

    /// The protocol this provider speaks.
    fn protocol(&self) -> Protocol;

    /// Open an outbound connection along `route` for `session`.
    async fn dial(&self, route: &Route, session: &TransferSession) -> Result<Box<dyn Link>>;

    /// Start listening and yield inbound connections as they arrive.
    async fn serve(&self, bind: Bind) -> Result<BoxStream<'static, Result<Box<dyn Link>>>>;
}
