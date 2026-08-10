//! Sending a chat message over an established session.

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::ChannelType;
use peerbeam_transfer::SessionHandle;

use crate::message::{ChatError, ChatMessage};
use crate::record::ChatRecord;
use crate::store::ChatStore;

/// Failure sending a chat message.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("chat session error: {0}")]
    Session(String),
}

/// Send `body` to `peer` over an established session: persist our copy, open a
/// Chat channel, and send one `Message` frame. Returns the persisted record.
pub async fn send_message(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
) -> Result<ChatRecord, SendError> {
    let msg = ChatMessage::new(body)?; // enforces MAX_BODY
    let rec = ChatRecord::sent(peer, &msg);
    store.append(&rec)?;
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    let frame = msg.to_frame(channel)?;
    handle
        .send_on_channel(
            channel,
            ChatMessage::message_type(),
            frame.flags,
            frame.payload,
        )
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    Ok(rec)
}
