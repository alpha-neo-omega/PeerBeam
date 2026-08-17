//! Sharing this device's clipboard over an established session.
//!
//! Clipboard is a message type on the session like any other (I2): it opens a
//! channel on the session it was given, sends on it, and owns no socket, no
//! discovery and no retry loop of its own. When the session dies the sender
//! goes with it; reconnecting is the session layer's job.
//!
//! Unlike Presence there is no timer here. A clip is pushed when the user
//! copies something, and the *watcher* that notices that lives in the Flutter
//! surface (Android forbids background clipboard reads, and there is no
//! system-clipboard adapter in this workspace). What this module owns is the
//! part that must not live in a UI: the gate, and the wire.

use std::sync::Arc;
use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelId, ChannelState, ChannelType};
use peerbeam_transfer::SessionHandle;

use crate::gate::may_share_clip;
use crate::message::{Clip, ClipboardError};

/// How long to wait for the peer's accept before giving up on the channel.
/// Mirrors `peerbeam_chat::send`'s budget for the same reason: `open_channel`
/// resolves as soon as *we* have queued the open request, so sending
/// immediately would race the peer's accept on any real transport.
const CHANNEL_OPEN_BUDGET: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the channel to reach `Open`.
const CHANNEL_OPEN_POLL: Duration = Duration::from_millis(10);

/// Reads the current value of the *"Sync clipboard with trusted devices"*
/// setting, **each time it is asked**.
///
/// A closure rather than a captured `bool` on purpose: turning the setting off
/// must stop the next clip, not the next reconnect.
pub type SyncSetting = Arc<dyn Fn() -> bool + Send + Sync>;

/// Failure sharing a clip.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
    #[error("clipboard session error: {0}")]
    Session(String),
}

/// What one push did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    /// A clip went out.
    Sent,
    /// The gate refused: sync is off, the peer is not trusted, or it did not
    /// negotiate the feature. Nothing was opened and nothing was sent.
    Withheld,
}

/// Pushes this device's clipboard to one peer over their session.
///
/// The gate is re-evaluated on **every** push, never captured — see
/// [`may_share_clip`] for why both privacy legs are read fresh.
pub struct ClipboardSender {
    handle: SessionHandle,
    peer: DeviceId,
    /// The **negotiated** (already intersected) capability set.
    caps: CapabilitySet,
    trust: Arc<dyn TrustStore>,
    sync: SyncSetting,
    /// Opened lazily, on the first push the gate actually permits. A device
    /// with sync off never opens a Clipboard channel at all: opening one is
    /// itself a signal, and a peer should not be able to tell "sync off" from
    /// "does not know me".
    channel: Option<ChannelId>,
}

impl ClipboardSender {
    /// Wire up a sender. Nothing is opened or sent until [`send`](Self::send).
    #[must_use]
    pub fn new(
        handle: SessionHandle,
        peer: DeviceId,
        caps: CapabilitySet,
        trust: Arc<dyn TrustStore>,
        sync: SyncSetting,
    ) -> Self {
        ClipboardSender {
            handle,
            peer,
            caps,
            trust,
            sync,
            channel: None,
        }
    }

    /// The privacy decision, asked fresh. **This is the only path to the
    /// wire**: [`send`](Self::send) refuses before it opens a channel and
    /// before it sends, so removing any leg from [`may_share_clip`] silently
    /// starts leaking, and the round-trip tests in `tests/gates.rs` exist to
    /// catch exactly that.
    #[must_use]
    pub fn may_send(&self) -> bool {
        may_share_clip((self.sync)(), self.trust.as_ref(), &self.peer, &self.caps)
    }

    /// Push one clip: check the gate, then (only then) open the channel if
    /// needed and send.
    ///
    /// The clip is built — and therefore validated — **before** anything is
    /// opened, so an over-cap or empty payload fails here rather than leaving a
    /// channel open with nothing to say. The gate is checked before even that:
    /// with sync off, a copy causes no work of any kind.
    pub async fn send(&mut self, text: &str) -> Result<Push, SendError> {
        if !self.may_send() {
            return Ok(Push::Withheld);
        }
        let clip = Clip::new(text)?;
        let channel = self.ensure_channel().await?;
        let frame = clip.to_frame(channel)?;
        self.handle
            .send_on_channel(channel, Clip::message_type(), frame.flags, frame.payload)
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        Ok(Push::Sent)
    }

    /// The Clipboard channel for this session, opening it on first use.
    async fn ensure_channel(&mut self) -> Result<ChannelId, SendError> {
        if let Some(c) = self.channel {
            return Ok(c);
        }
        let channel = self
            .handle
            .open_channel(ChannelType::CLIPBOARD)
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        self.wait_for_channel_open(channel).await?;
        self.channel = Some(channel);
        Ok(channel)
    }

    /// Wait for the peer to actually accept the channel. Same shape and same
    /// rationale as `peerbeam_chat::send::wait_for_channel_open`.
    async fn wait_for_channel_open(&self, channel: ChannelId) -> Result<(), SendError> {
        let deadline = Instant::now() + CHANNEL_OPEN_BUDGET;
        let mut seen = false;
        loop {
            let channels = self
                .handle
                .channels()
                .await
                .map_err(|e| SendError::Session(e.to_string()))?;
            let found = channels.iter().find(|c| c.id == channel).map(|c| c.state);
            match decide(found, seen) {
                PollOutcome::Open => return Ok(()),
                PollOutcome::Rejected => {
                    return Err(SendError::Session(format!(
                        "clipboard channel {channel:?} rejected by peer"
                    )))
                }
                PollOutcome::KeepWaiting => {}
            }
            if found.is_some() {
                seen = true;
            }
            if Instant::now() >= deadline {
                return Err(SendError::Session(format!(
                    "clipboard channel {channel:?} did not open within {CHANNEL_OPEN_BUDGET:?}"
                )));
            }
            tokio::time::sleep(CHANNEL_OPEN_POLL).await;
        }
    }
}

/// What one poll of `handle.channels()` tells us to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    Open,
    KeepWaiting,
    Rejected,
}

/// Pure decision for one poll, factored out so it is unit-testable without a
/// session. A rejected channel is *removed* from the registry rather than left
/// in a terminal state (see `channel_manager.rs`), so the real rejection signal
/// is the channel disappearing after having been seen.
fn decide(found: Option<ChannelState>, seen_before: bool) -> PollOutcome {
    match found {
        Some(state) if state.is_open() => PollOutcome::Open,
        Some(_) => PollOutcome::KeepWaiting,
        None if seen_before => PollOutcome::Rejected,
        None => PollOutcome::KeepWaiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_decides_open_waiting_and_rejected() {
        assert_eq!(decide(Some(ChannelState::Open), false), PollOutcome::Open);
        assert_eq!(
            decide(Some(ChannelState::Opening), false),
            PollOutcome::KeepWaiting
        );
        // Not yet registered on the very first poll: keep waiting.
        assert_eq!(decide(None, false), PollOutcome::KeepWaiting);
        // Seen, then gone: the peer rejected it — fail fast.
        assert_eq!(decide(None, true), PollOutcome::Rejected);
    }
}
