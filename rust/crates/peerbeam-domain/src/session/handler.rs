//! The message-handler port: how a capability plugs into a session.

use async_trait::async_trait;

use super::error::SessionError;
use super::frame::SessionFrame;
use super::ids::ChannelType;

/// Handles inbound messages for one channel type.
///
/// Each capability (transfer today; chat, presence, … in later phases)
/// implements this for its own [`ChannelType`]. The session dispatcher routes
/// every inbound [`SessionFrame`] to the handler registered for its channel; a
/// handler error is scoped to that channel by the dispatcher, never escalated to
/// the whole session.
#[async_trait]
pub trait MessageHandler: Send + Sync {
    /// The channel type this handler serves.
    fn channel_type(&self) -> ChannelType;

    /// Handle one inbound frame for this channel.
    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError>;
}
