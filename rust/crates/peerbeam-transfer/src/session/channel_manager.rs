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

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
/// **or** refused — opened exactly one stream on the peer's side, so both are
/// queued: `try_pair` consumes one stream per entry, keeping the FIFO
/// open↔stream pairing aligned even when an open is refused.
enum PendingOpen {
    /// A permitted open, to be paired with its stream and spawned.
    Accept(ChannelId, ChannelType),
    /// `n` refused opens whose streams are still owed; each is discarded in turn
    /// so a refusal never desyncs later pairings.
    ///
    /// A count, not an id and a reason, because the `ChannelReject` for a
    /// refused open is sent the moment it is refused (see
    /// [`ChannelManager::refuse`]) — nothing about *which* channel it was is
    /// still needed here. That is what lets consecutive refusals collapse into
    /// one entry, and it is what bounds this queue: only accepted opens count
    /// toward the channel limit, so a peer whose opens are all refused used to
    /// grow one queue entry each, without limit, and be told nothing.
    Skip(usize),
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
    // Channels the application has been told it has and has not been told it
    // lost: the set to re-attach on resume. See `reattachable_channels`.
    resumable: BTreeMap<ChannelId, ChannelType>,
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
            resumable: BTreeMap::new(),
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

    /// The **message** channels (id + type) eligible for automatic re-attachment
    /// on resume. Stream channels are excluded — their capability (e.g. transfer)
    /// re-opens them and resumes their payload itself.
    ///
    /// **Read from `resumable`, not from the live channel map, because by the time
    /// this is asked the channels are usually already gone.** A transport loss is
    /// observed twice over: the session pump's control link fails, *and* every
    /// channel actor's stream hits EOF and reports `Closed`. Both arms of the
    /// pump's `select!` are ready at once and the winner is arbitrary, so when an
    /// actor won, `on_actor_event` had already removed the channel and a set
    /// derived from live state came back empty — the session resumed with no
    /// channels re-attached, for ever, on a coin flip. Losing a channel across a
    /// reconnect is exactly what resume exists to prevent.
    ///
    /// `resumable` instead tracks what the *application* has been told: a channel
    /// enters when it emits [`ChannelEvent::Opened`] and leaves only when
    /// something explicitly closes it (either side closing, a reject, a peer
    /// error). An actor dying does not remove it, because an actor dies precisely
    /// when its stream vanishes — the case this must survive.
    ///
    /// Still only channels the peer has *accepted*: one still `Opening` was never
    /// announced, so both sides re-opening it from scratch is correct.
    #[must_use]
    pub fn reattachable_channels(&self) -> Vec<(ChannelId, ChannelType)> {
        self.resumable
            .iter()
            .filter(|(_, ty)| !self.stream_types.contains(ty))
            .map(|(id, ty)| (*id, *ty))
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
    /// id that is currently live or pending.
    ///
    /// Only pending *accepts* carry an id to skip. A refusal used to be queued
    /// with its id, and had to be skipped here too — otherwise `try_pair` would
    /// later emit a `ChannelReject` naming the channel we had meanwhile opened
    /// under that id. Refusals are now answered immediately, before an
    /// allocation could collide, so a [`PendingOpen::Skip`] entry names no
    /// channel and this scan is bounded by the channel limit.
    fn allocate_id(&mut self) -> Result<ChannelId, SessionError> {
        let channels = &self.channels;
        let pending = &self.pending_opens;
        self.ids
            .allocate(|cid| {
                channels.contains_key(&cid)
                    || pending
                        .iter()
                        .any(|p| matches!(p, PendingOpen::Accept(pid, _) if *pid == cid))
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
        // Count only pending *accepts* toward the limit — a skip marker never
        // becomes a channel. Scanned rather than counted alongside because the
        // queue holds at most one entry per pending accept plus the skip runs
        // between them, and an accept cannot be queued past this very limit.
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
        self.announce_open(id, channel_type);
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
            return self.refuse(channel, "reserved control channel id");
        }
        if let Err(reason) = self.permit(channel_type) {
            return self.refuse(channel, &reason);
        }
        if self.channels.contains_key(&channel)
            || self
                .pending_opens
                .iter()
                .any(|p| matches!(p, PendingOpen::Accept(id, _) if *id == channel))
        {
            return self.refuse(channel, "duplicate channel id");
        }
        self.pending_opens
            .push_back(PendingOpen::Accept(channel, channel_type));
        self.try_pair()
    }

    /// Refuse a peer `ChannelOpen`: tell the peer now, and remember that one
    /// inbound stream is owed and must be discarded when it arrives.
    ///
    /// **Answering now is what bounds `pending_opens`.** A refusal used to be
    /// queued whole — id and reason — and only became a `ChannelReject` once a
    /// stream arrived to pair it with. A peer that sent `ChannelOpen`s and opened
    /// no streams therefore added an entry per open, for ever: only accepted
    /// opens count against the channel limit, so nothing refused it, and both
    /// [`permit`](ChannelManager::permit) and
    /// [`allocate_id`](ChannelManager::allocate_id) scan the queue, making the
    /// cost quadratic in what a peer sends. Any peer past the TOFU handshake
    /// could drive it, and the peer was never even told its opens had failed.
    ///
    /// The stream still has to be consumed in FIFO order — an orphan would
    /// desync every later pairing — but that needs nothing more than a count,
    /// which is why consecutive refusals collapse into one
    /// [`PendingOpen::Skip`] entry. The queue is then bounded by the channel
    /// limit: at most one accept per permitted open, and at most one skip run
    /// between neighbouring accepts.
    fn refuse(&mut self, channel: ChannelId, reason: &str) -> Vec<ControlMessage> {
        self.emit(ChannelEvent::Rejected {
            channel,
            reason: reason.to_string(),
        });
        // Exact, not saturating: a count that stopped rising would owe fewer
        // streams than the peer opened, and the surplus would pair with the next
        // accepted open — the desync this queue exists to prevent. At the
        // (unreachable) ceiling a second entry carries the overflow instead.
        match self.pending_opens.back_mut() {
            Some(PendingOpen::Skip(owed)) if *owed < usize::MAX => *owed += 1,
            _ => self.pending_opens.push_back(PendingOpen::Skip(1)),
        }
        // The refusal first, then whatever the queued stream owed to it (or to
        // an earlier open) let pair: the peer learns its open failed as soon as
        // this method decided so, not whenever a stream happens to turn up.
        let mut out = vec![ControlMessage::ChannelReject {
            channel,
            reason: reason.to_string(),
        }];
        out.extend(self.try_pair());
        out
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
                PendingOpen::Skip(owed) => {
                    // Consume (drop) the stream the peer opened for a refused
                    // channel so the next open still lines up with its own
                    // stream. Nothing is sent: `refuse` already told the peer.
                    drop(link);
                    if owed > 1 {
                        self.pending_opens.push_front(PendingOpen::Skip(owed - 1));
                    }
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
                self.announce_open(id, channel_type);
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
            self.announce_open(id, channel_type);
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
                self.announce_open(channel, channel_type);
            }
        }
    }

    /// Tell the application a channel is open, and remember it as re-attachable.
    ///
    /// One method rather than an `emit` beside an insert at each of the four
    /// sites that open a channel, so the record cannot drift from what the
    /// application was told — a channel announced but not recorded is one that
    /// silently fails to come back after a reconnect, which is not visible in
    /// any single-connection test.
    fn announce_open(&mut self, channel: ChannelId, channel_type: ChannelType) {
        self.resumable.insert(channel, channel_type);
        self.emit(ChannelEvent::Opened {
            channel,
            channel_type,
        });
    }

    /// Forget a channel the application has been told is gone, so resume does not
    /// bring it back.
    ///
    /// Unconditional, and deliberately not guarded on the channel still being in
    /// `channels`: a peer's close or error routinely arrives *after* the local
    /// actor already noticed the stream end and removed it, and a guarded forget
    /// would leave the record behind to be re-attached on some later reconnect.
    fn forget_resumable(&mut self, channel: ChannelId) {
        self.resumable.remove(&channel);
    }

    /// Handle a peer `ChannelReject`: drop the pending channel.
    ///
    /// The `Rejected` event is emitted unconditionally: rejecting a channel
    /// closes its stream, which can make the local actor emit `Closed`/`Error`
    /// and remove the channel *before* this `ChannelReject` arrives — gating the
    /// emit on presence would then swallow the reject a caller is waiting for. A
    /// benign duplicate lifecycle event is preferable to a lost one.
    pub fn handle_channel_reject(&mut self, channel: ChannelId, reason: String) {
        self.forget_resumable(channel);
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
        }
        self.emit(ChannelEvent::Rejected { channel, reason });
    }

    /// Handle a peer `ChannelClose`: close locally and acknowledge.
    pub fn handle_channel_close(&mut self, channel: ChannelId) -> Vec<ControlMessage> {
        self.forget_resumable(channel);
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
            self.emit(ChannelEvent::Closed { channel });
            return vec![ControlMessage::ChannelClosed { channel }];
        }
        Vec::new()
    }

    /// Handle a peer `ChannelClosed` acknowledgement of a close we initiated.
    pub fn handle_channel_closed(&mut self, channel: ChannelId) {
        self.forget_resumable(channel);
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
        self.forget_resumable(channel);
        if let Some(ch) = self.channels.remove(&channel) {
            ch.signal_close();
        }
        self.emit(ChannelEvent::Error { channel, detail });
    }

    /// Close a channel locally, returning the `ChannelClose` to send.
    pub fn close_channel(&mut self, channel: ChannelId) -> Option<ControlMessage> {
        self.forget_resumable(channel);
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
        // Not a lost channel to be restored: shutdown is either a session closing
        // for good, or `capture_loss` tearing down dead actors *after* it has
        // already snapshotted this set.
        self.resumable.clear();
    }
}

#[cfg(test)]
mod pending_open_tests {
    //! The responder-side open queue. These drive [`ChannelManager`] directly,
    //! because the property under test is about frames a peer sends and streams
    //! it deliberately does *not* open — something no cooperating peer, and so
    //! no session-level test, can produce.

    use super::*;
    use peerbeam_domain::error::Result as DomainResult;
    use peerbeam_domain::port::Frame;
    use peerbeam_domain::session::Capability;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    /// A transport that opens nothing. Every method here is unreachable for the
    /// queue methods under test — they neither open nor accept — and a stub that
    /// says so is better than one that pretends to work.
    pub(super) struct NoTransport;

    #[async_trait::async_trait]
    impl ChannelTransport for NoTransport {
        async fn open_stream(&self) -> DomainResult<Box<dyn Link>> {
            Err(peerbeam_domain::error::DomainError::Transfer(
                "stub transport opens nothing".into(),
            ))
        }
        async fn accept_stream(&self) -> DomainResult<Option<Box<dyn Link>>> {
            Ok(None)
        }
        async fn close(&self) -> DomainResult<()> {
            Ok(())
        }
    }

    /// A stream that is already at EOF: enough to be *paired*, which is all
    /// these tests do with one.
    pub(super) struct DeadLink;

    #[async_trait::async_trait]
    impl Link for DeadLink {
        async fn send_frame(&mut self, _frame: Frame) -> DomainResult<()> {
            Ok(())
        }
        async fn recv_frame(&mut self) -> DomainResult<Option<Frame>> {
            Ok(None)
        }
        async fn close(&mut self) -> DomainResult<()> {
            Ok(())
        }
    }

    pub(super) const OK_TYPE: ChannelType = ChannelType::CHAT;
    /// A capability the manager below never negotiates, so every open naming it
    /// is refused — the cheapest way to make a peer's open fail `permit`.
    fn refused_type() -> ChannelType {
        ChannelType::new(0x0f0f)
    }

    pub(super) fn manager(limit: usize) -> (ChannelManager, UnboundedReceiver<ChannelEvent>) {
        let enc: Arc<dyn peerbeam_domain::port::EncryptionProvider> =
            Arc::new(peerbeam_crypto::AeadCrypto::new());
        // A zero master secret: these tests never seal or open a frame, and a
        // real handshake would only make what they assert harder to see.
        let auth = crate::auth::Session {
            send_key: [0u8; 32],
            recv_key: [0u8; 32],
            send_prefix: [0u8; 4],
            recv_prefix: [0u8; 4],
            peer_id: peerbeam_domain::id::DeviceId::from("pb-peer"),
            peer_name: String::new(),
            newly_trusted: false,
            pairing_code: String::new(),
            transcript: Vec::new(),
        };
        let crypto = SessionCrypto::from_session(&auth, SessionRole::Responder, enc);
        let (events, event_rx) = unbounded_channel();
        let (actor_events, _actor_rx) = unbounded_channel();
        let (incoming, _incoming_rx) = unbounded_channel();
        // Leaked deliberately: a dropped receiver would make the manager treat
        // every paired stream channel as unreceivable, which is a different test.
        std::mem::forget(_actor_rx);
        std::mem::forget(_incoming_rx);
        let m = ChannelManager::new(
            Arc::new(NoTransport),
            crypto,
            Version::CURRENT,
            SessionRole::Responder,
            HandlerRegistry::new(),
            CapabilitySet::new().with(Capability::new(OK_TYPE)),
            limit,
            HashSet::new(),
            events,
            actor_events,
            incoming,
        );
        (m, event_rx)
    }

    /// **The queue must not grow with what a peer refuses to follow through on.**
    /// A refused open used to be parked until a stream arrived to pair it with,
    /// so a peer that sent `ChannelOpen`s and opened no streams grew the queue by
    /// one entry each, unboundedly — and was told nothing, because the
    /// `ChannelReject` was only minted at pairing time. Only *accepted* opens
    /// count against the channel limit, so nothing else stopped it either.
    #[test]
    fn a_peer_that_opens_no_stream_can_neither_grow_the_queue_nor_go_unanswered() {
        let (mut m, _events) = manager(DEFAULT_CHANNEL_LIMIT);
        const OPENS: u64 = 5_000;
        for id in 0..OPENS {
            // Even ids: the peer's own parity for an initiator peer, so nothing
            // here is refused merely for being ours.
            let out = m.handle_channel_open(ChannelId::new((id + 1) * 2), refused_type());
            assert!(
                matches!(out.as_slice(), [ControlMessage::ChannelReject { .. }]),
                "open {id} was neither accepted nor refused: {out:?}"
            );
        }
        assert_eq!(
            m.pending_opens.len(),
            1,
            "{OPENS} refusals must collapse into one owed-stream run"
        );
    }

    /// The same, mixed with opens that *are* accepted: the run splits around
    /// each accept, so the queue stays bounded by the channel limit rather than
    /// by what the peer chooses to send.
    #[test]
    fn interleaving_accepted_opens_keeps_the_queue_bounded_by_the_channel_limit() {
        let (mut m, _events) = manager(4);
        let mut id = 0u64;
        let mut next = || {
            id += 2;
            ChannelId::new(id)
        };
        for _ in 0..1_000 {
            m.handle_channel_open(next(), refused_type());
            m.handle_channel_open(next(), OK_TYPE);
        }
        // 4 accepts fill the limit; every later open — of either type — is
        // refused and folds into the run beside it.
        assert!(
            m.pending_opens.len() <= 9,
            "queue grew to {} entries",
            m.pending_opens.len()
        );
    }

    /// A refusal still owes one stream, and discarding it in FIFO order is what
    /// keeps a later accepted open paired with its *own* stream. Collapsing
    /// refusals into a count must not lose one.
    #[tokio::test]
    async fn every_refused_open_still_consumes_exactly_one_stream() {
        let (mut m, _events) = manager(DEFAULT_CHANNEL_LIMIT);
        m.handle_channel_open(ChannelId::new(2), refused_type());
        m.handle_channel_open(ChannelId::new(4), refused_type());
        let accepted = ChannelId::new(6);
        assert!(
            m.handle_channel_open(accepted, OK_TYPE).is_empty(),
            "nothing to send until a stream arrives"
        );

        // The first two streams belong to the refusals and are discarded.
        assert!(m.on_stream_accepted(Box::new(DeadLink)).is_empty());
        assert!(m.on_stream_accepted(Box::new(DeadLink)).is_empty());
        // Only the third pairs with the accepted open.
        let out = m.on_stream_accepted(Box::new(DeadLink));
        assert!(
            matches!(
                out.as_slice(),
                [ControlMessage::ChannelAccept { channel }] if *channel == accepted
            ),
            "the accepted open paired with the wrong stream: {out:?}"
        );
        assert!(m.pending_opens.is_empty());
    }
}

#[cfg(test)]
mod resume_set_tests {
    //! What resume brings back. Driven against [`ChannelManager`] directly
    //! because the property is about the *order* two consequences of one
    //! transport loss are processed in — an ordering a session-level test can
    //! only reach by luck (it did, on CI, roughly one run in ten).

    use super::pending_open_tests::{manager, DeadLink, OK_TYPE};
    use super::*;

    /// One open message channel, opened by the peer and paired.
    async fn with_one_open_channel() -> (ChannelManager, ChannelId) {
        let (mut m, events) = manager(DEFAULT_CHANNEL_LIMIT);
        // Leaked for the same reason the harness leaks the others: a dropped
        // receiver would make `emit` fail, which is a different test.
        std::mem::forget(events);
        let channel = ChannelId::new(2);
        m.handle_channel_open(channel, OK_TYPE);
        m.on_stream_accepted(Box::new(DeadLink));
        assert_eq!(
            m.reattachable_channels(),
            vec![(channel, OK_TYPE)],
            "a paired open should be re-attachable straight away"
        );
        (m, channel)
    }

    /// **The bug this set exists for.** A transport loss reaches the session
    /// twice: its control link fails, and every channel actor's stream ends. The
    /// pump's `select!` has both arms ready and picks arbitrarily, so when an
    /// actor won, the channel was already removed by the time `preserve()` asked
    /// what to re-attach — and the session resumed with nothing, permanently, on
    /// a coin flip.
    #[tokio::test]
    async fn a_channel_whose_actor_died_is_still_re_attached() {
        let (mut m, channel) = with_one_open_channel().await;

        m.on_actor_event(ActorEvent::Closed { channel });

        assert!(
            !m.channels.contains_key(&channel),
            "the actor's close should still remove the live channel"
        );
        assert_eq!(
            m.reattachable_channels(),
            vec![(channel, OK_TYPE)],
            "an actor dying with its transport must not cancel re-attachment"
        );
    }

    /// The same for an actor that failed rather than ended: a stream torn down
    /// mid-frame reports `Errored`, and a transport loss is as likely to look
    /// like that as like a clean end.
    #[tokio::test]
    async fn a_channel_whose_actor_errored_is_still_re_attached() {
        let (mut m, channel) = with_one_open_channel().await;

        m.on_actor_event(ActorEvent::Errored {
            channel,
            detail: "stream reset".into(),
        });

        assert_eq!(
            m.reattachable_channels(),
            vec![(channel, OK_TYPE)],
            "a failed actor must not cancel re-attachment either"
        );
    }

    /// The other half of the contract: a channel *deliberately* closed must stay
    /// closed. Without this, resume would resurrect channels the application
    /// already let go — each one costing an id and a slot against the limit.
    #[tokio::test]
    async fn every_deliberate_close_is_final() {
        for (name, close) in [
            (
                "we closed it",
                Box::new(|m: &mut ChannelManager, c| {
                    m.close_channel(c);
                }) as Box<dyn Fn(&mut ChannelManager, ChannelId)>,
            ),
            (
                "the peer closed it",
                Box::new(|m: &mut ChannelManager, c| {
                    m.handle_channel_close(c);
                }),
            ),
            (
                "the peer acknowledged our close",
                Box::new(|m: &mut ChannelManager, c| m.handle_channel_closed(c)),
            ),
            (
                "the peer rejected it",
                Box::new(|m: &mut ChannelManager, c| m.handle_channel_reject(c, "no".into())),
            ),
            (
                "the peer reported it failed",
                Box::new(|m: &mut ChannelManager, c| m.handle_channel_error(c, "boom".into())),
            ),
        ] {
            let (mut m, channel) = with_one_open_channel().await;
            close(&mut m, channel);
            assert!(
                m.reattachable_channels().is_empty(),
                "re-attached a channel after {name}"
            );
        }
    }

    /// A close that arrives *after* the local actor already noticed the stream
    /// end — the routine order over a real transport, and the reason
    /// `forget_resumable` is not guarded on the channel still being live.
    #[tokio::test]
    async fn a_close_arriving_after_the_actor_died_is_still_final() {
        let (mut m, channel) = with_one_open_channel().await;

        m.on_actor_event(ActorEvent::Closed { channel });
        m.handle_channel_close(channel);

        assert!(
            m.reattachable_channels().is_empty(),
            "a peer's close was lost because the actor got there first"
        );
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
