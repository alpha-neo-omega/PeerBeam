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

/// Allocates this side's channel ids, upholding two invariants the rest of the
/// session depends on:
///
/// 1. **Parity split.** The initiator allocates odd ids, the responder even, so
///    both sides can open channels concurrently without ever choosing the same
///    id. The parity is fixed for the session's lifetime.
/// 2. **No `(id, epoch)` reuse.** A channel's AEAD keys derive from
///    `(id, epoch)`, so handing out one id twice *within an epoch* would reuse a
///    `(key, nonce)` pair. Allocation is monotonic within an epoch; a resume
///    bumps the epoch, so an id may safely recur afterwards under fresh keys.
///
/// A small, pure unit so these invariants can be unit-tested in isolation — the
/// scattered allocator logic this replaces could not be, and drifted repeatedly.
#[derive(Debug)]
struct ChannelIdAllocator {
    /// Next candidate id; always this side's parity.
    next: u64,
    /// This side's parity (`0` even/responder, `1` odd/initiator). Fixed.
    parity: u64,
}

impl ChannelIdAllocator {
    fn new(role: SessionRole) -> Self {
        let next = match role {
            SessionRole::Initiator => 1,
            SessionRole::Responder => 2,
        };
        ChannelIdAllocator {
            next,
            parity: next % 2,
        }
    }

    /// Hand out the next own-parity id for which `is_taken` returns false.
    /// Monotonic — each result is strictly greater than the last — so no id
    /// recurs within the epoch. `None` only on id-space exhaustion (≈2^63 opens,
    /// unreachable in practice).
    fn allocate(&mut self, is_taken: impl Fn(ChannelId) -> bool) -> Option<ChannelId> {
        loop {
            let id = self.next;
            self.next = self.next.checked_add(2)?;
            let cid = ChannelId::new(id);
            if id != 0 && !is_taken(cid) {
                return Some(cid);
            }
        }
    }

    /// Reserve an id this side did **not** allocate (a peer-accepted channel, or
    /// a preserved channel re-attached on resume) so it is never handed out this
    /// epoch. Only same-parity ids can collide with our allocations; an
    /// opposite-parity id is the peer's space and is left alone (advancing past
    /// it would flip our parity and break the split). A hostile same-parity id
    /// near `u64::MAX` cannot advance the counter (checked_add) — so it can
    /// neither hang the allocator nor force a later collision.
    fn reserve(&mut self, id: ChannelId) {
        let id = id.get();
        if id % 2 == self.parity {
            if let Some(next) = id.checked_add(2) {
                if next > self.next {
                    self.next = next;
                }
            }
        }
    }
}

/// Owns and coordinates one session's data channels.
pub struct ChannelManager {
    transport: Arc<dyn ChannelTransport>,
    crypto: SessionCrypto,
    version: Version,
    channels: HashMap<ChannelId, Channel>,
    handlers: HandlerRegistry,
    negotiated: CapabilitySet,
    ids: ChannelIdAllocator,
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
        ChannelManager {
            transport,
            crypto,
            version,
            channels: HashMap::new(),
            handlers,
            negotiated,
            ids: ChannelIdAllocator::new(role),
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
        // Reserve with the parity guard, NOT an unconditional bump: a preserved
        // channel may be the peer's (opposite-parity) id — re-attaching it must
        // not advance (and flip the parity of) our own allocator, or both sides
        // would start allocating the same ids after resume.
        self.ids.reserve(id);
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

    /// Allocate the next own-parity id for a locally-opened channel, skipping any
    /// id that is currently live or pending (as an Accept **or** a Reject — a
    /// queued Reject for an our-parity id must not be reissued, or `try_pair`
    /// would later reject the channel we just opened under it).
    fn allocate_id(&mut self) -> Result<ChannelId, SessionError> {
        let channels = &self.channels;
        let pending = &self.pending_opens;
        self.ids
            .allocate(|cid| {
                channels.contains_key(&cid)
                    || pending.iter().any(|p| match p {
                        PendingOpen::Accept(pid, _) | PendingOpen::Reject(pid, _) => *pid == cid,
                    })
            })
            .ok_or_else(|| SessionError::Channel("channel id space exhausted".into()))
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
        let id = self.allocate_id()?;
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
        let id = self.allocate_id()?;
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
            self.ids.reserve(id);
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

#[cfg(test)]
mod id_allocator_tests {
    use super::{ChannelIdAllocator, SessionRole};
    use peerbeam_domain::session::ChannelId;
    use std::collections::HashSet;

    fn free(_: ChannelId) -> bool {
        false
    }

    #[test]
    fn allocates_role_parity_and_steps_by_two() {
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        let a = init.allocate(free).unwrap().get();
        let b = init.allocate(free).unwrap().get();
        let c = init.allocate(free).unwrap().get();
        assert_eq!((a, b, c), (1, 3, 5), "initiator: odd, ascending");

        let mut resp = ChannelIdAllocator::new(SessionRole::Responder);
        let a = resp.allocate(free).unwrap().get();
        let b = resp.allocate(free).unwrap().get();
        assert_eq!((a, b), (2, 4), "responder: even, ascending");
    }

    #[test]
    fn allocate_skips_taken_and_never_yields_zero() {
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        let taken: HashSet<u64> = [1, 3].into_iter().collect();
        let id = init.allocate(|c| taken.contains(&c.get())).unwrap().get();
        assert_eq!(id, 5, "skips live 1 and 3");
    }

    #[test]
    fn allocation_is_monotonic_so_no_reissue_within_epoch_even_after_close() {
        // Allocate 1,3,5; then "close" 3 (no longer live). The monotonic counter
        // must not hand 3 back out this epoch — that would reuse its (id, epoch)
        // AEAD key. This is the invariant rounds 4/8 kept violating.
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        let mut live: HashSet<u64> = HashSet::new();
        for _ in 0..3 {
            live.insert(init.allocate(|c| live.contains(&c.get())).unwrap().get());
        }
        assert_eq!(live, [1, 3, 5].into_iter().collect());
        live.remove(&3); // channel 3 closes
        let next = init.allocate(|c| live.contains(&c.get())).unwrap().get();
        assert_eq!(next, 7, "must not reissue the closed id 3");
    }

    #[test]
    fn reserve_same_parity_prevents_reissue() {
        // Simulates resume re-attaching our own (same-parity) id 5 into a fresh
        // allocator: reserving it must push the counter past it.
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        init.reserve(ChannelId::new(5));
        assert_eq!(init.allocate(free).unwrap().get(), 7, "past reserved 5");
    }

    #[test]
    fn reserve_opposite_parity_is_ignored_preserving_parity() {
        // An initiator re-attaching the peer's even id 2 must NOT flip its
        // allocator to even (the round-8 parity-flip regression).
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        init.reserve(ChannelId::new(2));
        let id = init.allocate(free).unwrap().get();
        assert_eq!(id, 1, "still odd; opposite-parity reserve ignored");
    }

    #[test]
    fn reserve_hostile_max_id_neither_hangs_nor_forces_collision() {
        // A peer sending a same-parity id near u64::MAX must not advance the
        // counter to an overflow point (the round-7 infinite-loop / collision).
        let mut init = ChannelIdAllocator::new(SessionRole::Initiator);
        init.reserve(ChannelId::new(u64::MAX)); // MAX is odd = initiator parity
                                                // Still hands out small odd ids, promptly, with no hang.
        assert_eq!(init.allocate(free).unwrap().get(), 1);
        assert_eq!(init.allocate(free).unwrap().get(), 3);

        let mut resp = ChannelIdAllocator::new(SessionRole::Responder);
        resp.reserve(ChannelId::new(u64::MAX - 1)); // even = responder parity
        assert_eq!(resp.allocate(free).unwrap().get(), 2);
    }
}
