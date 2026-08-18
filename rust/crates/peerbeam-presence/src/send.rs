//! Sharing this device's status over an established session.
//!
//! Presence is a message type on the session like any other (I2): it opens a
//! channel on the session it was given, sends on it, and owns no socket, no
//! discovery and no retry loop of its own. When the session dies the heartbeat
//! stops; reconnecting is the session layer's job, and a fresh session starts a
//! fresh heartbeat.

use std::sync::Arc;
use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelId, ChannelState, ChannelType};
use peerbeam_transfer::SessionHandle;

use crate::gate::may_share_status;
use crate::message::{PresenceError, Status};

/// How often a status goes out while the session stays open.
///
/// One minute is chosen against the two things it trades between: a dashboard
/// that is wrong for at most a minute, and a radio that wakes 60 times an hour.
/// It is *not* a wire constant — the two sides never compare it, so a future
/// build may change it without breaking anyone. What a receiver must not do is
/// treat a missed beat as offline; liveness is the session's business.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for the peer's accept before giving up on the channel.
/// Mirrors `peerbeam_chat::send`'s budget for the same reason: `open_channel`
/// resolves as soon as *we* have queued the open request, so sending
/// immediately would race the peer's accept on any real transport.
const CHANNEL_OPEN_BUDGET: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the channel to reach `Open`.
const CHANNEL_OPEN_POLL: Duration = Duration::from_millis(10);

/// Reads the current value of the *"Share device status with trusted devices"*
/// setting, **each time it is asked**.
///
/// A closure rather than a captured `bool` on purpose: turning the setting off
/// must stop an already-running session's heartbeats at the next tick, not at
/// the next reconnect.
pub type SharingSetting = Arc<dyn Fn() -> bool + Send + Sync>;

/// Collects this device's status as of right now.
///
/// Called per heartbeat rather than once, so a draining battery and a filling
/// disk are actually reported. See [`crate::collect`].
pub type StatusSource = Arc<dyn Fn() -> Status + Send + Sync>;

/// Failure sharing a status.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Presence(#[from] PresenceError),
    #[error("presence session error: {0}")]
    Session(String),
}

impl SendError {
    /// Whether this error means the session itself is gone, so the heartbeat
    /// should stop rather than retry into a closed pipe.
    fn is_terminal(&self) -> bool {
        matches!(self, SendError::Session(_))
    }
}

/// What one heartbeat did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    /// A status went out.
    Sent,
    /// The gate refused: sharing is off, the peer is not trusted, or it did not
    /// negotiate the feature. Nothing was opened and nothing was sent.
    Withheld,
}

/// Shares this device's status with one peer, for as long as their session
/// lives.
///
/// The gate is re-evaluated on **every** beat, never captured — see
/// [`may_share_status`] for why both privacy legs are read fresh.
pub struct PresenceSender {
    handle: SessionHandle,
    peer: DeviceId,
    /// The **negotiated** (already intersected) capability set.
    caps: CapabilitySet,
    trust: Arc<dyn TrustStore>,
    sharing: SharingSetting,
    source: StatusSource,
    /// Opened lazily, on the first beat the gate actually permits. A device
    /// with sharing off never opens a Presence channel at all: opening one is
    /// itself a signal, and a peer should not be able to tell "sharing off"
    /// from "does not know me".
    channel: Option<ChannelId>,
}

impl PresenceSender {
    /// Wire up a sender. Nothing is opened or sent until [`beat`](Self::beat).
    #[must_use]
    pub fn new(
        handle: SessionHandle,
        peer: DeviceId,
        caps: CapabilitySet,
        trust: Arc<dyn TrustStore>,
        sharing: SharingSetting,
        source: StatusSource,
    ) -> Self {
        PresenceSender {
            handle,
            peer,
            caps,
            trust,
            sharing,
            source,
            channel: None,
        }
    }

    /// The privacy decision, asked fresh. **This is the only path to the
    /// wire**: [`beat`](Self::beat) refuses before it opens a channel and
    /// before it sends, so removing either privacy leg from
    /// [`may_share_status`] silently starts leaking, and the round-trip tests
    /// in `tests/gates.rs` exist to catch exactly that.
    #[must_use]
    pub fn may_send(&self) -> bool {
        may_share_status(
            (self.sharing)(),
            self.trust.as_ref(),
            &self.peer,
            &self.caps,
        )
    }

    /// One heartbeat: check the gate, then (only then) open the channel if
    /// needed and send one `Status`.
    pub async fn beat(&mut self) -> Result<Beat, SendError> {
        if !self.may_send() {
            return Ok(Beat::Withheld);
        }
        let status = (self.source)();
        // Encode BEFORE opening anything: a collector that produced an
        // out-of-range battery or an unknown network word must fail here
        // rather than leave a channel open with nothing to say.
        let channel = self.ensure_channel().await?;
        let frame = status.to_frame(channel)?;
        self.handle
            .send_on_channel(channel, Status::message_type(), frame.flags, frame.payload)
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        Ok(Beat::Sent)
    }

    /// Ask the peer to make itself findable for `seconds`.
    ///
    /// Independent of [`may_send`](Self::may_send) and of the heartbeat: that
    /// gate is the *sharing* opt-in, which governs whether this device reveals
    /// its own battery and network. Ringing reveals nothing about this device —
    /// it asks something of the other one — so a user who shares no status can
    /// still find their phone.
    pub async fn ring(&mut self, seconds: u16) -> Result<(), SendError> {
        let channel = self.ensure_channel().await?;
        let r = crate::message::Ring::new(seconds);
        let frame = r.to_frame(channel)?;
        self.handle
            .send_on_channel(
                channel,
                crate::message::Ring::message_type(),
                frame.flags,
                frame.payload,
            )
            .await
            .map_err(|e| SendError::Session(e.to_string()))
    }

    /// Heartbeat until the session ends: once immediately (so a dashboard
    /// populates on connect rather than up to a minute later), then every
    /// `interval`.
    ///
    /// There is no polling when nothing is connected — this task only exists
    /// for the lifetime of a session, and it returns as soon as that session is
    /// gone.
    pub async fn run(mut self, interval: Duration) {
        loop {
            match self.beat().await {
                Ok(_) => {}
                Err(e) if e.is_terminal() => return, // session gone
                Err(_) => {
                    // A malformed status from our own collector. Skip this
                    // beat and try the next one rather than killing the
                    // heartbeat for a transient bad reading.
                }
            }
            tokio::time::sleep(interval).await;
            // Cheap liveness probe so a withheld (never-sending) heartbeat
            // still notices the session ending and stops. Local round trip to
            // the session pump; no network.
            if self.handle.channels().await.is_err() {
                return;
            }
        }
    }

    /// The Presence channel for this session, opening it on first use.
    async fn ensure_channel(&mut self) -> Result<ChannelId, SendError> {
        if let Some(c) = self.channel {
            return Ok(c);
        }
        let channel = self
            .handle
            .open_channel(ChannelType::PRESENCE)
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
                        "presence channel {channel:?} rejected by peer"
                    )))
                }
                PollOutcome::KeepWaiting => {}
            }
            if found.is_some() {
                seen = true;
            }
            if Instant::now() >= deadline {
                return Err(SendError::Session(format!(
                    "presence channel {channel:?} did not open within {CHANNEL_OPEN_BUDGET:?}"
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

    /// The cadence is "once on open, then every 60s". If this changes, a peer's
    /// dashboard changes how fast it goes stale, so it is worth pinning.
    #[test]
    fn the_heartbeat_interval_is_sixty_seconds() {
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(60));
    }
}
