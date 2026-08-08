//! The channel manager: owns a session's data channels and their lifecycle.
//!
//! One manager per session. It allocates channel ids, opens/accepts channels
//! over the [`ChannelTransport`], keeps the channel registry, routes inbound
//! streams to per-channel actors, enforces capability permissions and a channel
//! limit, and reports lifecycle via [`ChannelEvent`]s.
//!
//! It owns no control link: lifecycle methods **return** the control messages to
//! send, and the session pump writes them. Failure of one channel is isolated to
//! that channel — no method here tears down the session or unrelated channels.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::port::{ChannelTransport, Link};
use peerbeam_domain::session::{
    CapabilitySet, ChannelId, ChannelState, ChannelType, MessageFlags, MessageType, SessionError,
    SessionFrame, Version,
};

use super::channel::{
    spawn_channel, ActorEvent, Channel, ChannelEvent, ChannelInfo, IncomingStreamChannel,
    PROBE_MESSAGE_TYPE,
};
use super::control::ControlMessage;
use super::crypto::SessionCrypto;
use super::event::SessionRole;
use super::registry::HandlerRegistry;
use super::sealed_link::SealedLink;

/// Default cap on concurrently-open channels per session (prevents a peer from
/// exhausting resources by opening unboundedly).
pub const DEFAULT_CHANNEL_LIMIT: usize = 256;

/// A queued peer `ChannelOpen` awaiting its stream. Every peer open — accepted
/// **or** rejected — opened exactly one stream on the peer's side, so both are
/// queued: `try_pair` consumes one stream per entry, keeping the FIFO
/// open↔stream pairing aligned even when an open is rejected.
enum PendingOpen {
    /// A permitted open, to be paired with its stream and spawned.
    Accept(ChannelId, ChannelType),
    /// A rejected open; its stream is consumed (dropped) on pairing and the
    /// `ChannelReject` emitted, so the reject never desyncs later pairings.
    Reject(ChannelId, String),
}

/// Owns and coordinates one session's data channels.
pub struct ChannelManager {
    transport: Arc<dyn ChannelTransport>,
    crypto: SessionCrypto,
    version: Version,
    channels: HashMap<ChannelId, Channel>,
    handlers: HandlerRegistry,
    negotiated: CapabilitySet,
    next_id: u64,
    limit: usize,
    events: UnboundedSender<ChannelEvent>,
    actor_events_tx: UnboundedSender<ActorEvent>,
    // Channel types whose stream is owned by the caller (e.g. transfer) rather
    // than dispatched to a message handler.
    stream_types: HashSet<ChannelType>,
    incoming_stream_tx: UnboundedSender<IncomingStreamChannel>,
    // Responder-side FIFO pairing of received ChannelOpens with accepted streams.
    pending_opens: VecDeque<PendingOpen>,
    pending_streams: VecDeque<Box<dyn Link>>,
}

impl ChannelManager {
    /// Create a manager. `actor_events_tx` is the sink channel actors report
    /// lifecycle on; the session pump reads the matching receiver and feeds
    /// events back via [`on_actor_event`](ChannelManager::on_actor_event).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport: Arc<dyn ChannelTransport>,
        crypto: SessionCrypto,
        version: Version,
        role: SessionRole,
        handlers: HandlerRegistry,
        negotiated: CapabilitySet,
        limit: usize,
        stream_types: HashSet<ChannelType>,
        events: UnboundedSender<ChannelEvent>,
        actor_events_tx: UnboundedSender<ActorEvent>,
        incoming_stream_tx: UnboundedSender<IncomingStreamChannel>,
    ) -> Self {
        // Parity split: the initiator uses odd ids, the responder even, so
        // concurrently-opened channels never collide on an id. 0 is control.
        let next_id = match role {
            SessionRole::Initiator => 1,
            SessionRole::Responder => 2,
        };
        ChannelManager {
            transport,
            crypto,
            version,
            channels: HashMap::new(),
            handlers,
            negotiated,
            next_id,
            limit,
            events,
            actor_events_tx,
            stream_types,
            incoming_stream_tx,
            pending_opens: VecDeque::new(),
            pending_streams: VecDeque::new(),
        }
    }

    /// The shared transport (for the pump's accept loop).
    #[must_use]
    pub fn transport(&self) -> Arc<dyn ChannelTransport> {
        self.transport.clone()
    }

    /// Number of live channels.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// The current crypto epoch (reconnect generation, M6).
    #[must_use]
    pub fn crypto_epoch(&self) -> u64 {
        self.crypto.epoch()
    }

    /// A copy of the session crypto at the current epoch, for preserving the
    /// master secret across a reconnect.
    #[must_use]
    pub fn crypto_clone(&self) -> SessionCrypto {
        self.crypto.with_epoch(self.crypto.epoch())
    }

    /// The open **message** channels (id + type) eligible for automatic
    /// re-attachment on resume. Stream channels are excluded — their capability
    /// (e.g. transfer) re-opens them and resumes their payload itself.
    #[must_use]
    pub fn reattachable_channels(&self) -> Vec<(ChannelId, ChannelType)> {
        self.channels
            .values()
            .filter(|ch| {
                ch.state() == ChannelState::Open && !self.stream_types.contains(&ch.channel_type())
            })
            .map(|ch| (ch.id(), ch.channel_type()))
            .collect()
    }

    /// Re-open a preserved channel with its **original** id (resume re-attach).
    /// Like [`open_channel`](ChannelManager::open_channel) but the id is fixed, not
    /// freshly allocated; future allocations are bumped past it to avoid collision.
    pub async fn reopen_channel(
        &mut self,
        id: ChannelId,
        channel_type: ChannelType,
    ) -> Result<ControlMessage, SessionError> {
        self.permit(channel_type).map_err(SessionError::Channel)?;
        let crypto = self.crypto.derive(id, self.version)?;
        let link = self.transport.open_stream().await?;
        let handler = self.handlers.get(channel_type);
        let channel = spawn_channel(
            id,
            channel_type,
            ChannelState::Opening,
            link,
            handler,
            self.actor_events_tx.clone(),
            crypto,
            self.crypto.enc(),
        );
        self.channels.insert(id, channel);
        self.bump_next_id_past(id);
        if let Some(ch) = self.channels.get(&id) {
            ch.send(SessionFrame::new(
                id,
                MessageType::new(PROBE_MESSAGE_TYPE),
                MessageFlags::OPTIONAL,
                Bytes::new(),
            ));
        }
        Ok(ControlMessage::ChannelOpen {
            channel: id,
            channel_type,
        })
    }

    /// Keep the id allocator from ever handing out a re-attached id, preserving
    /// role parity (the allocator steps by 2).
    ///
    /// Computed directly, never in a loop: callers only pass ids sharing our
    /// allocation parity (so `id + 2` stays in-parity), and `id` may be
    /// peer-supplied — a `while next_id <= id` loop would spin (forever for
    /// `u64::MAX`) on a hostile value. `saturating_add` caps at `u64::MAX`.
    fn bump_next_id_past(&mut self, id: ChannelId) {
        let next = id.get().saturating_add(2);
        if next > self.next_id {
            self.next_id = next;
        }
    }

    /// After accepting a peer-opened channel, advance our allocator past its id
    /// if it shares our parity, so we never re-issue that id within this epoch —
    /// **even after the channel closes**. On resume the initiator re-attaches our
    /// own (same-parity) channels into our freshly-reset allocator; without this
    /// high-water mark, a re-attached id that later closes would be handed out
    /// again at the same `(id, epoch)`, reusing its AEAD `(key, nonce)`. Only
    /// same-parity ids can collide with our allocations; opposite-parity ids are
    /// the peer's space and are left alone (bumping past them would waste ours).
    fn reserve_incoming_id(&mut self, id: ChannelId) {
        if id.get() % 2 == self.next_id % 2 {
            self.bump_next_id_past(id);
        }
    }

    /// A snapshot of every live channel's metadata and statistics.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ChannelInfo> {
        self.channels
            .values()
            .map(|ch| ChannelInfo {
                id: ch.id(),
                channel_type: ch.channel_type(),
                state: ch.state(),
                stats: ch.stats(),
            })
            .collect()
    }

    fn allocate_id(&mut self) -> ChannelId {
        // Step by 2 to preserve the role parity; never yields 0 (control). Skip
        // any id already live or pending: on resume the initiator re-attaches the
        // responder's own (even-parity) channels, so this side's fresh allocator
        // (reset to its parity start) would otherwise re-hand-out an occupied id —
        // clobbering the live channel and reusing its (key, nonce). Skipping keeps
        // every live channel's id unique. `permit` guarantees a free id exists.
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(2);
            let cid = ChannelId::new(id);
            // Exclude ids that are live OR pending as *either* an Accept or a
            // Reject: a queued Reject for an our-parity id (a peer opened it with
            // a bad/unnegotiated capability) must not be reissued, or `try_pair`
            // would later reject the channel we just opened under that id.
            let taken = self.channels.contains_key(&cid)
                || self.pending_opens.iter().any(|p| match p {
                    PendingOpen::Accept(pid, _) | PendingOpen::Reject(pid, _) => *pid == cid,
                });
            if id != 0 && !taken {
                return cid;
            }
        }
    }

    fn emit(&self, event: ChannelEvent) {
        let _ = self.events.send(event);
    }

    fn permit(&self, channel_type: ChannelType) -> Result<(), String> {
        if !self.negotiated.supports(channel_type) {
            return Err(format!(
                "capability {:#06x} not negotiated",
                channel_type.get()
            ));
        }
        // Count only pending *accepts* toward the limit — reject markers never
        // become channels.
        let pending_accepts = self
            .pending_opens
            .iter()
            .filter(|p| matches!(p, PendingOpen::Accept(..)))
            .count();
        if self.channels.len() + pending_accepts >= self.limit {
            return Err("channel limit reached".into());
        }
        Ok(())
    }

    /// Open a data channel of `channel_type`. Opens a stream, spawns its actor
    /// (state `Opening`), and returns the id plus the `ChannelOpen` to send. The
    /// channel becomes `Open` when the peer's `ChannelAccept` arrives.
    pub async fn open_channel(
        &mut self,
        channel_type: ChannelType,
    ) -> Result<(ChannelId, ControlMessage), SessionError> {
        self.permit(channel_type).map_err(SessionError::Channel)?;
        let id = self.allocate_id();
        let crypto = self.crypto.derive(id, self.version)?;
        let link = self.transport.open_stream().await?;
        let handler = self.handlers.get(channel_type);
        let channel = spawn_channel(
            id,
            channel_type,
            ChannelState::Opening,
            link,
            handler,
            self.actor_events_tx.clone(),
            crypto,
            self.crypto.enc(),
        );
        self.channels.insert(id, channel);
        // Probe frame: materialises the stream on lazy transports (e.g. QUIC
        // creates a stream only on first write) so the peer can accept and pair
        // it before any application data flows. The peer never dispatches it.
        if let Some(ch) = self.channels.get(&id) {
            ch.send(SessionFrame::new(
                id,
                MessageType::new(PROBE_MESSAGE_TYPE),
                MessageFlags::OPTIONAL,
                Bytes::new(),
            ));
        }
        Ok((
            id,
            ControlMessage::ChannelOpen {
                channel: id,
                channel_type,
            },
        ))
    }

    /// Open a **stream** channel: the caller owns the sealed stream and runs its
    /// own protocol (e.g. `send_file`) over it. Returns the id, the sealed stream,
    /// and the `ChannelOpen` to send. No dispatch actor and no probe — the
    /// caller's first write materialises the stream on the peer.
    pub async fn open_stream_channel(
        &mut self,
        channel_type: ChannelType,
    ) -> Result<(ChannelId, Box<dyn Link>, ControlMessage), SessionError> {
        self.permit(channel_type).map_err(SessionError::Channel)?;
        let id = self.allocate_id();
        let crypto = self.crypto.derive(id, self.version)?;
        let link = self.transport.open_stream().await?;
        let sealed: Box<dyn Link> = Box::new(SealedLink::new(link, crypto, self.crypto.enc()));
        self.channels
            .insert(id, Channel::new_stream(id, channel_type));
        self.emit(ChannelEvent::Opened {
            channel: id,
            channel_type,
        });
        Ok((
            id,
            sealed,
            ControlMessage::ChannelOpen {
                channel: id,
                channel_type,
            },
        ))
    }

    /// Handle a peer `ChannelOpen` (responder side): permission-check, then queue
    /// for FIFO pairing with an accepted stream. Returns control messages to send
    /// (a `ChannelReject`, or `ChannelAccept`s for any channels that paired).
    pub fn handle_channel_open(
        &mut self,
        channel: ChannelId,
        channel_type: ChannelType,
    ) -> Vec<ControlMessage> {
        // Reserved id 0 is the control channel — it lives outside `channels`, so
        // the duplicate-id guard below would not catch it. Accepting a peer open
        // on id 0 would derive crypto identical to the live control channel (same
        // id/version/epoch → same HKDF key + nonce prefix) with counters reset to
        // 0, reusing an AEAD (key, nonce) pair the control channel already
        // consumed — a two-time-pad break enabling control-channel forgery.
        // Refuse it before any crypto derivation.
        if channel.is_control() {
            self.pending_opens.push_back(PendingOpen::Reject(
                channel,
                "reserved control channel id".to_string(),
            ));
            return self.try_pair();
        }
        // Even a rejected open is queued (as a Reject marker): the peer opened a
        // stream for it, and only by consuming that stream in FIFO order does the
        // reject avoid orphaning a stream and desyncing every later pairing.
        if let Err(reason) = self.permit(channel_type) {
            self.pending_opens
                .push_back(PendingOpen::Reject(channel, reason));
            return self.try_pair();
        }
        if self.channels.contains_key(&channel)
            || self
                .pending_opens
                .iter()
                .any(|p| matches!(p, PendingOpen::Accept(id, _) if *id == channel))
        {
            self.pending_opens.push_back(PendingOpen::Reject(
                channel,
                "duplicate channel id".to_string(),
            ));
            return self.try_pair();
        }
        self.pending_opens
            .push_back(PendingOpen::Accept(channel, channel_type));
        self.try_pair()
    }

    /// Handle a newly-accepted inbound stream (responder side): queue for pairing.
    pub fn on_stream_accepted(&mut self, link: Box<dyn Link>) -> Vec<ControlMessage> {
        self.pending_streams.push_back(link);
        self.try_pair()
    }

    /// Pair queued ChannelOpens with accepted streams (FIFO), spawning a channel
    /// actor for each pair and yielding the `ChannelAccept`s to send.
    fn try_pair(&mut self) -> Vec<ControlMessage> {
        let mut out = Vec::new();
        while !self.pending_opens.is_empty() && !self.pending_streams.is_empty() {
            let Some(pending) = self.pending_opens.pop_front() else {
                break;
            };
            let Some(link) = self.pending_streams.pop_front() else {
                break;
            };
            let (id, channel_type) = match pending {
                PendingOpen::Reject(id, reason) => {
                    // Consume (drop) the stream the peer opened for this rejected
                    // channel so the next open still lines up with its own stream.
                    drop(link);
                    self.emit(ChannelEvent::Rejected {
                        channel: id,
                        reason: reason.clone(),
                    });
                    out.push(ControlMessage::ChannelReject {
                        channel: id,
                        reason,
                    });
                    continue;
                }
                PendingOpen::Accept(id, channel_type) => (id, channel_type),
            };
            // Reserve this id in our allocator so we never re-issue it this epoch
            // (closes the resume nonce-reuse case for re-attached same-parity ids).
            self.reserve_incoming_id(id);
            let crypto = match self.crypto.derive(id, self.version) {
                Ok(crypto) => crypto,
                Err(e) => {
                    // Key derivation failure (should be impossible): reject the
                    // channel and drop the accepted stream. Isolated.
                    self.emit(ChannelEvent::Error {
                        channel: id,
                        detail: e.to_string(),
                    });
                    drop(link);
                    out.push(ControlMessage::ChannelReject {
                        channel: id,
                        reason: "key derivation failed".into(),
                    });
                    continue;
                }
            };
            if self.stream_types.contains(&channel_type) {
                // Stream capability (e.g. transfer): the caller owns the sealed
                // stream and runs its own protocol over it. No dispatch actor.
                let sealed: Box<dyn Link> =
                    Box::new(SealedLink::new(link, crypto, self.crypto.enc()));
                if self
                    .incoming_stream_tx
                    .send(IncomingStreamChannel {
                        channel: id,
                        channel_type,
                        link: sealed,
                    })
                    .is_err()
                {
                    // Nobody is receiving incoming streams: reject, isolated.
                    let reason = "no stream receiver".to_string();
                    self.emit(ChannelEvent::Rejected {
                        channel: id,
                        reason: reason.clone(),
                    });
                    out.push(ControlMessage::ChannelReject {
                        channel: id,
                        reason,
                    });
                    continue;
                }
                self.channels
                    .insert(id, Channel::new_stream(id, channel_type));
                self.emit(ChannelEvent::Opened {
                    channel: id,
                    channel_type,
                });
                out.push(ControlMessage::ChannelAccept { channel: id });
                continue;
            }
            let handler = self.handlers.get(channel_type);
            let channel = spawn_channel(
                id,
                channel_type,
                ChannelState::Open,
                link,
                handler,
                self.actor_events_tx.clone(),
                crypto,
                self.crypto.enc(),
            );
            self.channels.insert(id, channel);
            self.emit(ChannelEvent::Opened {
                channel: id,
                channel_type,
            });
            out.push(ControlMessage::ChannelAccept { channel: id });
        }
        out
    }

    /// Handle a peer `ChannelAccept` (initiator side): mark the channel open.
    pub fn handle_channel_accept(&mut self, channel: ChannelId) {
        if let Some(ch) = self.channels.get_mut(&channel) {
            if ch.state() == ChannelState::Opening {
                ch.set_state(ChannelState::Open);
                let channel_type = ch.channel_type();
                self.emit(ChannelEvent::Opened {
                    channel,
                    channel_type,
                });
            }
        }
    }

    /// Handle a peer `ChannelReject`: drop the pending channel.
    ///
    /// The `Rejected` event is emitted unconditionally: rejecting a channel
    /// closes its stream, which can make the local actor emit `Closed`/`Error`
    /// and remove the channel *before* this `ChannelReject` arrives — gating the
    /// emit on presence would then swallow the reject a caller is waiting for. A
    /// benign duplicate lifecycle event is preferable to a lost one.
    pub fn handle_channel_reject(&mut self, channel: ChannelId, reason: String) {
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
        }
        self.emit(ChannelEvent::Rejected { channel, reason });
    }

    /// Handle a peer `ChannelClose`: close locally and acknowledge.
    pub fn handle_channel_close(&mut self, channel: ChannelId) -> Vec<ControlMessage> {
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
            self.emit(ChannelEvent::Closed { channel });
            return vec![ControlMessage::ChannelClosed { channel }];
        }
        Vec::new()
    }

    /// Handle a peer `ChannelClosed` acknowledgement of a close we initiated.
    pub fn handle_channel_closed(&mut self, channel: ChannelId) {
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
            self.emit(ChannelEvent::Closed { channel });
        }
    }

    /// Handle a peer `ChannelError`: close the channel (isolated). Emits
    /// unconditionally for the same reason as [`handle_channel_reject`] — a peer
    /// error can arrive after the local actor already closed the channel, and a
    /// waiter must still observe it.
    pub fn handle_channel_error(&mut self, channel: ChannelId, detail: String) {
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
        }
        self.emit(ChannelEvent::Error { channel, detail });
    }

    /// Close a channel locally, returning the `ChannelClose` to send.
    pub fn close_channel(&mut self, channel: ChannelId) -> Option<ControlMessage> {
        let ch = self.channels.remove(&channel)?;
        ch.signal_close();
        self.emit(ChannelEvent::Closed { channel });
        Some(ControlMessage::ChannelClose { channel })
    }

    /// Handle an actor's lifecycle event (peer hung up or a channel-scoped error),
    /// returning any control message to inform the peer.
    pub fn on_actor_event(&mut self, event: ActorEvent) -> Vec<ControlMessage> {
        match event {
            ActorEvent::Closed { channel } => {
                if self.channels.remove(&channel).is_some() {
                    self.emit(ChannelEvent::Closed { channel });
                }
                Vec::new()
            }
            ActorEvent::Errored { channel, detail } => {
                if self.channels.remove(&channel).is_some() {
                    self.emit(ChannelEvent::Error {
                        channel,
                        detail: detail.clone(),
                    });
                    return vec![ControlMessage::ChannelError { channel, detail }];
                }
                Vec::new()
            }
        }
    }

    /// Send an application frame on an open channel.
    pub fn send_on_channel(
        &self,
        channel: ChannelId,
        message_type: MessageType,
        flags: MessageFlags,
        payload: Bytes,
    ) -> Result<(), SessionError> {
        let ch = self
            .channels
            .get(&channel)
            .ok_or_else(|| SessionError::Channel(format!("no such channel {}", channel.get())))?;
        if !ch.state().is_open() {
            return Err(SessionError::Channel("channel not open".into()));
        }
        let frame = SessionFrame::new(channel, message_type, flags, payload);
        if ch.send(frame) {
            Ok(())
        } else {
            Err(SessionError::Channel("channel actor stopped".into()))
        }
    }

    /// Close every channel (session shutdown). Signals all actors and clears the
    /// registry; the transport itself is closed by the session.
    pub fn shutdown_all(&mut self) {
        for (_, ch) in self.channels.drain() {
            ch.signal_close();
        }
        self.pending_opens.clear();
        self.pending_streams.clear();
    }
}
