//! A single data channel: its statistics, lifecycle events, and the per-channel
//! actor task that owns the channel's stream.
//!
//! Each channel runs its own actor so that reads and writes on its stream happen
//! concurrently and independently of every other channel — a stuck or failed
//! channel cannot block or tear down its neighbours.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link};
use peerbeam_domain::session::{
    ChannelId, ChannelState, ChannelType, MessageHandler, SessionFrame,
};

use super::crypto::ChannelCrypto;

/// Reserved message type (0) for a probe frame sent when a channel opens, purely
/// to materialise the underlying stream on a lazy transport (e.g. QUIC opens a
/// stream on first write). The peer counts it but never dispatches it.
pub(crate) const PROBE_MESSAGE_TYPE: u16 = 0;

/// Cumulative per-channel counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelStats {
    /// Frames sent on this channel.
    pub frames_sent: u64,
    /// Frames received on this channel.
    pub frames_recv: u64,
    /// Payload bytes sent.
    pub bytes_sent: u64,
    /// Payload bytes received.
    pub bytes_recv: u64,
}

/// A snapshot of one channel's metadata and statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInfo {
    /// The channel id.
    pub id: ChannelId,
    /// The capability it carries.
    pub channel_type: ChannelType,
    /// Its current lifecycle state.
    pub state: ChannelState,
    /// Its statistics.
    pub stats: ChannelStats,
}

/// A per-channel lifecycle event emitted by the [`ChannelManager`].
///
/// [`ChannelManager`]: super::ChannelManager
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelEvent {
    /// The channel is established (locally accepted or peer-accepted).
    Opened {
        /// The channel id.
        channel: ChannelId,
        /// The capability it carries.
        channel_type: ChannelType,
    },
    /// The peer refused an open we requested.
    Rejected {
        /// The refused channel id.
        channel: ChannelId,
        /// Why.
        reason: String,
    },
    /// The channel closed.
    Closed {
        /// The closed channel id.
        channel: ChannelId,
    },
    /// A channel-scoped error occurred (isolated to this channel).
    Error {
        /// The affected channel.
        channel: ChannelId,
        /// What went wrong.
        detail: String,
    },
}

/// A command from the manager to a channel's actor.
pub(crate) enum ActorCommand {
    /// Send this frame on the channel's stream.
    Send(SessionFrame),
    /// Finish and close the stream.
    Close,
}

/// An event from a channel's actor back to the manager.
pub(crate) enum ActorEvent {
    /// The peer closed the stream cleanly.
    Closed { channel: ChannelId },
    /// The stream failed or a frame was invalid (channel-scoped).
    Errored { channel: ChannelId, detail: String },
}

/// A live channel handle held in the manager's registry: metadata plus the
/// actor's command sender. The actor owns the stream.
pub struct Channel {
    id: ChannelId,
    channel_type: ChannelType,
    state: ChannelState,
    stats: Arc<Mutex<ChannelStats>>,
    commands: UnboundedSender<ActorCommand>,
}

impl Channel {
    /// The channel id.
    #[must_use]
    pub fn id(&self) -> ChannelId {
        self.id
    }

    /// The capability this channel carries.
    #[must_use]
    pub fn channel_type(&self) -> ChannelType {
        self.channel_type
    }

    /// The current lifecycle state.
    #[must_use]
    pub fn state(&self) -> ChannelState {
        self.state
    }

    /// A snapshot of the channel's statistics.
    #[must_use]
    pub fn stats(&self) -> ChannelStats {
        *lock(&self.stats)
    }

    /// Set the lifecycle state (manager-only).
    pub(crate) fn set_state(&mut self, state: ChannelState) {
        self.state = state;
    }

    /// Queue a frame to send on this channel. An error means the actor has
    /// stopped (channel gone).
    pub(crate) fn send(&self, frame: SessionFrame) -> bool {
        self.commands.send(ActorCommand::Send(frame)).is_ok()
    }

    /// Signal the actor to close the stream (best-effort).
    pub(crate) fn signal_close(&self) {
        let _ = self.commands.send(ActorCommand::Close);
    }
}

/// Recover a poisoned lock rather than panicking.
fn lock(stats: &Arc<Mutex<ChannelStats>>) -> std::sync::MutexGuard<'_, ChannelStats> {
    stats.lock().unwrap_or_else(|p| p.into_inner())
}

/// Create a channel by spawning its actor over `link`. The actor reads inbound
/// frames (dispatching to `handler` if present) and writes queued outbound
/// frames, reporting lifecycle changes via `actor_events`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_channel(
    id: ChannelId,
    channel_type: ChannelType,
    state: ChannelState,
    mut link: Box<dyn Link>,
    handler: Option<Arc<dyn MessageHandler>>,
    actor_events: UnboundedSender<ActorEvent>,
    mut crypto: ChannelCrypto,
    enc: Arc<dyn EncryptionProvider>,
) -> Channel {
    let (commands, mut command_rx) = unbounded_channel::<ActorCommand>();
    let stats = Arc::new(Mutex::new(ChannelStats::default()));
    let actor_stats = stats.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                cmd = command_rx.recv() => match cmd {
                    Some(ActorCommand::Send(sf)) => {
                        let bytes = sf.payload.len() as u64;
                        // Seal with this channel's key; advance the counter only
                        // after a successful send (retry-safe).
                        let sealed = match crypto.seal(&*enc, &sf.encode()) {
                            Ok(sealed) => sealed,
                            Err(e) => {
                                let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                                break;
                            }
                        };
                        let frame = Frame { kind: FrameKind::Control, payload: Bytes::from(sealed) };
                        if link.send_frame(frame).await.is_err() {
                            let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: "send failed".into() });
                            break;
                        }
                        if let Err(e) = crypto.advance_send() {
                            let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                            break;
                        }
                        let mut s = lock(&actor_stats);
                        s.frames_sent += 1;
                        s.bytes_sent += bytes;
                    }
                    // Close requested, or the manager dropped the channel handle.
                    // Dropping the stream (loop break) closes only this channel;
                    // the whole connection is closed via ChannelTransport::close.
                    Some(ActorCommand::Close) | None => break,
                },
                inbound = link.recv_frame() => match inbound {
                    Ok(Some(frame)) => {
                        // Open with this channel's key; any failure (tamper,
                        // replay, wrong key) is channel-scoped — it closes only
                        // this channel, never the session or its neighbours.
                        let plain = match crypto.open(&*enc, &frame.payload) {
                            Ok(plain) => plain,
                            Err(e) => {
                                let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                                break;
                            }
                        };
                        match SessionFrame::decode(&plain) {
                            Ok(sf) => {
                                {
                                    let mut s = lock(&actor_stats);
                                    s.frames_recv += 1;
                                    s.bytes_recv += sf.payload.len() as u64;
                                }
                                // The probe frame (message type 0) only
                                // materialises the stream; never dispatch it.
                                if sf.message_type.get() != PROBE_MESSAGE_TYPE {
                                    if let Some(h) = &handler {
                                        if let Err(e) = h.handle(sf).await {
                                            let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = actor_events.send(ActorEvent::Closed { channel: id });
                        break;
                    }
                    Err(e) => {
                        let _ = actor_events.send(ActorEvent::Errored { channel: id, detail: e.to_string() });
                        break;
                    }
                },
            }
        }
    });

    Channel {
        id,
        channel_type,
        state,
        stats,
        commands,
    }
}
