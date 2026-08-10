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

/// What one poll of `handle.channels()` tells us to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    /// The channel is `Open`: proceed to send.
    Open,
    /// Not decided yet; poll again after the interval.
    KeepWaiting,
    /// The peer rejected (or otherwise killed) the channel: fail fast rather
    /// than waiting out the full budget.
    Rejected,
}

/// Pure decision for one poll, factored out of [`wait_for_channel_open`] so it
/// can be unit-tested without spinning up a session.
///
/// `found` is this channel's state in the current snapshot, if it is present
/// at all. `seen_before` is whether an *earlier* poll found it present (in any
/// state) — `open_channel` guarantees the channel is registered locally
/// (`Opening`) before it returns, so it is present on the very first poll in
/// the normal case.
///
/// The channel manager does not transition a rejected/closed/errored channel
/// through a terminal [`ChannelState`] that stays queryable — `handle_channel_
/// reject`/`_close`/`_closed`/`_error` all *remove* the entry from the
/// registry outright (see `peerbeam-transfer/src/session/channel_manager.rs`).
/// So the real rejection signal is the channel *disappearing* from the
/// snapshot after having been seen present: that is what actually fires below.
/// The terminal-state arm is kept as a belt-and-suspenders check in case a
/// future revision of the channel manager starts leaving rejected/closed
/// channels queryable in a terminal state instead of removing them.
fn decide(found: Option<ChannelState>, seen_before: bool) -> PollOutcome {
    match found {
        Some(state) if state.is_open() => PollOutcome::Open,
        Some(state) if state.is_terminal() || matches!(state, ChannelState::Errored) => {
            PollOutcome::Rejected
        }
        Some(_) => PollOutcome::KeepWaiting, // Opening (or Closing, before removal).
        None if seen_before => PollOutcome::Rejected, // was present, now gone: rejected.
        None => PollOutcome::KeepWaiting,    // not registered yet (brief lag right after open).
    }
}

/// Block (with a bounded poll loop, never indefinitely) until `channel`
/// reaches [`ChannelState::Open`] on our side, or fail once the peer's
/// rejection is detected (the channel disappearing after having been seen) or
/// the budget is exhausted.
async fn wait_for_channel_open(
    handle: &SessionHandle,
    channel: ChannelId,
) -> Result<(), SendError> {
    let deadline = Instant::now() + CHANNEL_OPEN_BUDGET;
    let mut seen = false;
    loop {
        let channels = handle
            .channels()
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        let found = channels.iter().find(|c| c.id == channel).map(|c| c.state);
        match decide(found, seen) {
            PollOutcome::Open => return Ok(()),
            PollOutcome::Rejected => {
                return Err(SendError::Session(format!(
                    "chat channel {channel:?} rejected by peer"
                )));
            }
            PollOutcome::KeepWaiting => {}
        }
        if found.is_some() {
            seen = true;
        }
        if Instant::now() >= deadline {
            return Err(SendError::Session(format!(
                "chat channel {channel:?} did not open within {CHANNEL_OPEN_BUDGET:?}"
            )));
        }
        tokio::time::sleep(CHANNEL_OPEN_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_opens_once_state_is_open() {
        assert_eq!(decide(Some(ChannelState::Open), false), PollOutcome::Open);
        assert_eq!(decide(Some(ChannelState::Open), true), PollOutcome::Open);
    }

    #[test]
    fn decide_keeps_waiting_while_opening_or_not_yet_registered() {
        assert_eq!(
            decide(Some(ChannelState::Opening), false),
            PollOutcome::KeepWaiting
        );
        assert_eq!(
            decide(Some(ChannelState::Opening), true),
            PollOutcome::KeepWaiting
        );
        // Not present yet and never seen: could just be registration lag right
        // after `open_channel` returned — keep waiting rather than giving up.
        assert_eq!(decide(None, false), PollOutcome::KeepWaiting);
    }

    #[test]
    fn decide_rejects_fast_when_a_previously_seen_channel_disappears() {
        // This is the real-world rejection signal: the channel manager removes
        // a rejected/closed/errored channel from the registry outright instead
        // of leaving it in a terminal state, so "gone after being seen" is what
        // actually fires.
        assert_eq!(decide(None, true), PollOutcome::Rejected);
    }

    #[test]
    fn decide_rejects_on_a_terminal_state_belt_and_suspenders() {
        // Currently dead in production (the manager never leaves a channel
        // queryable in these states — it removes it), kept as a safety net.
        assert_eq!(
            decide(Some(ChannelState::Rejected), false),
            PollOutcome::Rejected
        );
        assert_eq!(
            decide(Some(ChannelState::Closed), true),
            PollOutcome::Rejected
        );
        assert_eq!(
            decide(Some(ChannelState::Errored), false),
            PollOutcome::Rejected
        );
    }
}
