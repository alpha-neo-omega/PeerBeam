//! Sending a chat message over an established session.

use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelId, ChannelState, ChannelType};
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

/// How long to wait for the peer's `ChannelAccept` before giving up.
///
/// `SessionHandle::open_channel` resolves as soon as *we* have locally
/// allocated the channel (state `Opening`) and queued the open request on the
/// wire — it does not wait for the peer to accept. On the in-memory test
/// transport that round trip is effectively instantaneous, but over any real
/// transport (QUIC over LAN/WiFi/Tailscale/Internet) it is a nonzero network
/// round trip, so sending immediately races the peer's accept and can hard-fail
/// with `SessionError::Channel("channel not open")`.
const CHANNEL_OPEN_BUDGET: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the channel to reach `Open`.
const CHANNEL_OPEN_POLL: Duration = Duration::from_millis(10);

/// Send `body` to `peer` over an established session: open a Chat channel,
/// wait for it to actually reach `Open` (the peer's accept), send one
/// `Message` frame, and only then persist + return the sent record.
///
/// Persisting happens strictly after a successful send: on any failure (the
/// channel never opens, or the send itself errors) no record is written, so
/// local history never shows a message as sent when it was not.
pub async fn send_message(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
) -> Result<ChatRecord, SendError> {
    let msg = ChatMessage::new(body)?; // enforces MAX_BODY
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
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
    let rec = ChatRecord::sent(peer, &msg);
    store.append(&rec)?;
    Ok(rec)
}

/// Block (with a bounded poll loop, never indefinitely) until `channel`
/// reaches [`ChannelState::Open`] on our side, or return an error once the
/// channel reaches a terminal/failure state or the budget is exhausted.
async fn wait_for_channel_open(
    handle: &SessionHandle,
    channel: ChannelId,
) -> Result<(), SendError> {
    let deadline = Instant::now() + CHANNEL_OPEN_BUDGET;
    loop {
        let channels = handle
            .channels()
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        if let Some(info) = channels.iter().find(|c| c.id == channel) {
            if info.state.is_open() {
                return Ok(());
            }
            if info.state.is_terminal() || matches!(info.state, ChannelState::Errored) {
                return Err(SendError::Session(format!(
                    "chat channel {channel:?} failed to open: {:?}",
                    info.state
                )));
            }
            // ChannelState::Closing or Opening: keep waiting.
        }
        if Instant::now() >= deadline {
            return Err(SendError::Session(format!(
                "chat channel {channel:?} did not open within {CHANNEL_OPEN_BUDGET:?}"
            )));
        }
        tokio::time::sleep(CHANNEL_OPEN_POLL).await;
    }
}
