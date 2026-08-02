//! Channel transport port: a connection that multiplexes independent streams.
//!
//! A session multiplexes many logical channels over one authenticated
//! connection. This port models exactly that: the ability to open and accept
//! independent streams, each presented as a [`Link`]. It maps directly onto a
//! transport's native multi-stream capability (e.g. QUIC bidirectional streams)
//! — there is no custom multiplexing protocol; one channel is one stream.

use async_trait::async_trait;

use crate::error::Result;

use super::transfer::Link;

/// A connection that can open and accept independent streams.
///
/// Each stream is a full [`Link`] (its own ordered, reliable, flow-controlled
/// frame pipe), so a channel inherits ordering and flow control from the
/// transport rather than reimplementing them. Methods take `&self`: a transport
/// is shared (e.g. an `Arc`) and drives opens/accepts concurrently.
#[async_trait]
pub trait ChannelTransport: Send + Sync {
    /// Open a new outbound stream.
    async fn open_stream(&self) -> Result<Box<dyn Link>>;

    /// Accept the next inbound stream, or `None` when the connection has closed.
    async fn accept_stream(&self) -> Result<Option<Box<dyn Link>>>;

    /// Close the whole connection and all its streams.
    async fn close(&self) -> Result<()>;
}
