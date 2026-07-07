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

/// A single live connection to a peer.
#[async_trait]
pub trait Link: Send + Sync {
    /// Send one frame.
    async fn send_frame(&mut self, frame: Frame) -> Result<()>;

    /// Receive the next frame, or `None` when the peer closes cleanly.
    async fn recv_frame(&mut self) -> Result<Option<Frame>>;

    /// Close the connection.
    async fn close(&mut self) -> Result<()>;
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
