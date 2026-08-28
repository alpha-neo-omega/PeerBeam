//! PeerSession establishment for the FFI transfer manager (production path).
//!
//! Mirrors the CLI's session helper: the sender dials a multiplexed channel
//! connection and opens an initiator session; the receiver accepts a responder
//! session on an inbound channel connection. The transfer itself runs over the
//! session's transfer channel via the existing engine helpers — no transfer
//! logic lives here.

use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use peerbeam_chat::{ChatHandler, ChatStore, ReceivedSink};
use peerbeam_clipboard::ClipboardHandler;
use peerbeam_domain::entity::{Device, RouteKind, TransferSession};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelId, ChannelState, ChannelType, MessageHandler, SessionFrame,
    CHAT_FEAT_FILEDECLINE, CHAT_FEAT_FILEREF, CHAT_FEAT_REACTION, CHAT_FEAT_RECEIPT,
    CLIPBOARD_FEAT_CLIP, NOTES_FEAT_SYNC, PIPE_FEAT_STREAM, PRESENCE_FEAT_RING,
    PRESENCE_FEAT_STATUS,
};
use peerbeam_domain::session::{BROWSE_FEAT_LIST, SYNC_FEAT_MANIFEST, TRANSFER_FEAT_FOLDER_ACK};
use peerbeam_engine::RouteManager;
use peerbeam_presence::{PresenceHandler, PresenceSender, PresenceSink, HEARTBEAT_INTERVAL};

use crate::presence::PresenceWiring;
use peerbeam_transfer::{
    HandlerRegistry, Identity, IncomingStreamChannel, PeerSession, SessionConfig, SessionHandle,
    SessionRole,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};

use crate::error::{from_domain, Code};

const TRANSFER: ChannelType = ChannelType::TRANSFER;
const CHAT: ChannelType = ChannelType::CHAT;
const CLIPBOARD: ChannelType = ChannelType::CLIPBOARD;
const PRESENCE: ChannelType = ChannelType::PRESENCE;
const PIPE: ChannelType = ChannelType::PIPE;
const NOTES: ChannelType = ChannelType::NOTES;
const BROWSE: ChannelType = ChannelType::BROWSE;
const SYNC: ChannelType = ChannelType::SYNC;

/// Chat wiring for a session: the store + a received-sink. Every dial AND
/// every accept call site in this codebase registers this (built via
/// `Manager::chat_wiring()`, never `None` in production) — a session with no
/// `ChatHandler` bound on a given side doesn't error on an inbound CHAT
/// frame, it silently drops it, so a message the peer pushes over an
/// established session (its own flush-on-connect, a reply, or anything else)
/// would be lost with no error on either side. See `Manager::chat_wiring`'s
/// doc comment for the full rationale. Any NEW dial or accept call site
/// added in the future must register this too, or it reintroduces exactly
/// this silent-message-loss bug.
///
/// `Clone` (both fields are: `ChatStore` derives it, `ReceivedSink` is an
/// `Arc`) so `dial`'s retry loop can hand each connection attempt its own
/// copy without giving up the multi-route retry behavior.
#[derive(Clone)]
pub struct ChatWiring {
    pub store: ChatStore,
    pub sink: ReceivedSink,
}

/// What a session needs to serve the Notes channel: the note store and the
/// trust store the permission is read from.
///
/// Registered on every dial and accept, for the reason `ChatWiring` documents:
/// a session with no `NotesHandler` does not refuse an inbound batch, it drops
/// it silently, so the peer believes it synced and nothing arrived.
///
/// Registering it is not sharing. `NotesHandler` re-reads
/// `Permission::Notes` per batch, and a peer that was never granted it writes
/// nothing and learns nothing.
pub struct NotesWiring {
    pub store: peerbeam_notes::NoteStore,
    pub trust: Arc<dyn TrustStore>,
}

/// A session config advertising both the TRANSFER (stream) and CHAT (message)
/// capabilities. `chat_handler`, when present, is registered to serve the Chat
/// channel. Every dial and every accept call site in this codebase passes
/// `Some(...)` (via `Manager::chat_wiring()`), so a message pushed from
/// either side of an established session can always be received — a new dial
/// or accept call site that passes `None` would silently drop any CHAT frame
/// pushed to it instead of erroring (see `ChatWiring`'s doc comment).
///
/// CHAT additionally advertises [`CHAT_FEAT_FILEREF`] (this build understands
/// the `FileRef` message and can correlate it with a transfer) and
/// [`CHAT_FEAT_FILEDECLINE`] (this build both sends a `FileDecline` when its
/// user turns a file down and settles its own outgoing row on receiving one),
/// mirroring the CLI's `session_transfer::session_cfg` — both frontends must
/// advertise the same set or a peer's behaviour would depend on which of ours
/// it happens to be talking to. Advertising a feature bit is not a wire
/// change — `Capability.features` is already on the wire and
/// `CapabilitySet::intersect` ANDs it — so a peer from before either feature
/// simply advertises `0`, the intersection clears the bit, and
/// [`Session::supports_file_ref`] / [`caps_support_file_decline`] report false
/// for it.
///
/// PRESENCE advertises [`PRESENCE_FEAT_STATUS`] on the same terms: this build
/// understands a device status heartbeat. Advertising it says nothing about
/// whether this device *sends* one — that is the opt-in setting's business, and
/// it is off by default. The bit asserts comprehension, so a device sharing
/// nothing still advertises it truthfully and still shows everyone else's
/// status.
///
/// CLIPBOARD advertises [`CLIPBOARD_FEAT_CLIP`] on those same terms, and here
/// the comprehension/behaviour split carries real weight: Android 10+ forbids
/// background clipboard reads, so a phone can never auto-send a clip and yet
/// advertises this bit truthfully, because it applies an incoming one in full.
/// Desktop sends, every platform receives.
///
/// PIPE advertises [`PIPE_FEAT_STREAM`] and is registered as a **stream**
/// channel type, and this is the widest comprehension/behaviour gap of the
/// four: **the GUI offers no pipe UI and refuses every pipe offered to it**,
/// because a pipe writes raw bytes to a shell's stdout and a GUI has none. The
/// refusal lives in `transfer::handle_incoming`, at the point of acceptance —
/// *not* in a quietly narrower advertisement — so that a peer's behaviour does
/// not depend on which of PeerBeam's frontends it reached, and so the refusal
/// can state a reason. Registering the stream type is what routes an inbound
/// pipe there to be refused, rather than leaving it to pair as a handler-less
/// message channel whose frames vanish in silence.
fn session_cfg(handlers: Vec<Arc<dyn MessageHandler>>) -> SessionConfig {
    let mut cfg = SessionConfig::new(advertised_caps())
        .with_stream_channel_type(TRANSFER)
        .with_stream_channel_type(PIPE);
    if !handlers.is_empty() {
        let mut reg = HandlerRegistry::new();
        for h in handlers {
            reg = reg.with(h);
        }
        cfg = cfg.with_handlers(reg);
    }
    cfg
}

/// Exactly what this build puts on the wire, split out of [`session_cfg`] so a
/// test can read it without standing up a session. Keeping it as the single
/// definition — rather than restating it in the test module — is what makes an
/// "is the bit advertised?" assertion mean anything: a copy would keep passing
/// while `session_cfg` quietly stopped advertising.
fn advertised_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::with_features(
            TRANSFER,
            TRANSFER_FEAT_FOLDER_ACK,
        ))
        .with(Capability::with_features(
            CHAT,
            CHAT_FEAT_FILEREF | CHAT_FEAT_FILEDECLINE | CHAT_FEAT_REACTION | CHAT_FEAT_RECEIPT,
        ))
        .with(Capability::with_features(CLIPBOARD, CLIPBOARD_FEAT_CLIP))
        .with(Capability::with_features(
            PRESENCE,
            PRESENCE_FEAT_STATUS | PRESENCE_FEAT_RING,
        ))
        .with(Capability::with_features(PIPE, PIPE_FEAT_STREAM))
        .with(Capability::with_features(NOTES, NOTES_FEAT_SYNC))
        .with(Capability::with_features(BROWSE, BROWSE_FEAT_LIST))
        .with(Capability::with_features(SYNC, SYNC_FEAT_MANIFEST))
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// folder-acknowledgement feature, i.e. whether a completed folder send will be
/// confirmed by the peer.
///
/// False for a peer that predates the bit, and then nothing is sent, nothing is
/// waited for, and both ends behave exactly as they did before it existed.
pub fn caps_support_folder_ack(caps: &CapabilitySet) -> bool {
    caps.features(TRANSFER)
        .is_some_and(|f| f & TRANSFER_FEAT_FOLDER_ACK != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// chat `FileRef` feature.
///
/// Split out of [`Session::supports_file_ref`] so the decision is testable
/// without standing up a live session, and so there is exactly one place that
/// knows how the bit is read.
fn caps_support_file_ref(caps: &CapabilitySet) -> bool {
    caps.features(CHAT)
        .is_some_and(|f| f & CHAT_FEAT_FILEREF != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// chat `FileDecline` feature, i.e. whether telling this peer "I turned your
/// file down" would mean anything to it. Same shape and same reason as
/// [`caps_support_file_ref`]: exactly one place knows how the bit is read, and
/// it is testable without standing up a session.
///
/// Read by `transfer::should_send_decline` (which owns the full send decision),
/// not by a `Session` accessor — a peer that predates the feature advertises
/// `features: 0`, the intersection clears the bit, and it is sent nothing.
pub fn caps_support_file_decline(caps: &CapabilitySet) -> bool {
    caps.features(CHAT)
        .is_some_and(|f| f & CHAT_FEAT_FILEDECLINE != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// chat `Reaction` feature, i.e. whether an emoji attached to one of this
/// peer's messages would mean anything to it. Same shape and same reason as
/// [`caps_support_file_ref`].
///
/// The `Reaction` frame is OPTIONAL, so an older peer would drop it harmlessly
/// either way. The bit exists so a sender can decline to offer the gesture at
/// all, rather than let its user believe a reaction landed somewhere it was
/// never displayed.
pub fn caps_support_reaction(caps: &CapabilitySet) -> bool {
    caps.features(CHAT)
        .is_some_and(|f| f & CHAT_FEAT_REACTION != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// chat `Receipt` feature, i.e. whether telling this peer "I have read your
/// messages up to here" would mean anything to it.
///
/// Advertising the bit says only that a peer can *apply* a receipt. Whether
/// this device *sends* one is `DeviceConfig::share_read_receipts` — a privacy
/// choice, kept off the wire.
pub fn caps_support_receipt(caps: &CapabilitySet) -> bool {
    caps.features(CHAT)
        .is_some_and(|f| f & CHAT_FEAT_RECEIPT != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// presence `Ring` feature: whether asking this peer to make itself findable
/// would mean anything.
pub fn caps_support_ring(caps: &CapabilitySet) -> bool {
    caps.features(PRESENCE)
        .is_some_and(|f| f & PRESENCE_FEAT_RING != 0)
}

/// How long to wait for a peer's `ChannelAccept` before giving up on a lane.
///
/// Matches `peerbeam_chat::send`'s budget, and exists for the same reason: this
/// side's `open_channel` resolves before the peer has answered, so the wait is
/// one real network round trip, not a local one.
const CHANNEL_OPEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
/// Poll interval while waiting for a lane's channel to reach `Open`.
const CHANNEL_OPEN_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Which of a session's long-lived channels a frame travels on.
///
/// **One channel per lane, per session, for the session's life.** Every request
/// helper and every reply pump in this module used to open a fresh channel and
/// never close one, so a session that asked more than
/// `DEFAULT_CHANNEL_LIMIT` (256) times reached the limit and stopped asking —
/// silently, because the failures came back as `None` or a discarded `bool`. A
/// folder sync issues a chunk-map request and several chunk requests *per file*,
/// so a moderately sized folder was enough.
///
/// Reuse rather than close-after-each-request. Closing would also bound the
/// count, and would not lose the request (a dropped quinn `SendStream` is
/// `finish`ed, and the frame precedes the FIN on an ordered stream) — it is
/// simply the wrong shape for this traffic:
///
/// * **It costs a round trip per request.** An open is a `ChannelOpen`, the
///   peer's `ChannelAccept`, and — because a frame sent before that accept is
///   refused locally, see [`Channels::await_open`] — a wait for it, plus a
///   `ChannelClose`/`ChannelClosed` pair afterwards. A delta fetch is a chunk
///   map plus a chunk request per 64 chunks *per file*, so paying an extra RTT
///   on each roughly doubles the round trips of a folder sync. A lane pays it
///   once per session.
/// * **It only bounds the peer's count if the closes keep up.** The peer's
///   `permit` counts the channels we open, and nothing rate-limits our requests
///   against the close acknowledgements, so a burst can still reach its limit.
///   A lane cannot.
///
/// It is also the shape the rest of the codebase already uses for message
/// channels: `PresenceSender::ensure_channel` and its clipboard twin each cache
/// one channel per session, and [`send_note_batches`] already argued the same
/// for a batch sequence. `session/transfer.rs` and `session/pipe.rs` do close
/// theirs, and should: a **stream** channel carries exactly one payload by
/// design, so its lifetime *is* the transfer's and there is nothing to reuse.
///
/// Lanes rather than channel types, because a channel actor writes its queued
/// frames serially down one stream: everything sharing a channel shares a
/// head-of-line queue. One chunk request is answered with 64 frames of up to
/// half a megabyte each (a chunk is at most `MAX_CHUNK`, hex-encoded on the
/// wire), while a chunk map is the one small frame that tells the peer what to
/// ask for next — sharing a channel, the frame that unblocks the next round trip
/// would queue behind tens of megabytes of bytes. Keeping the SYNC reply lanes
/// and the SYNC request lane apart is what preserves the independence the
/// separate reply pumps were written for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Lane {
    /// Listings this side asks for.
    BrowseAsk,
    /// Listings this side answers with.
    BrowseAnswer,
    /// Manifest, chunk-map, chunk and file requests this side makes — all small.
    SyncAsk,
    /// Manifests this side answers with.
    SyncManifest,
    /// Chunk maps this side answers with.
    SyncChunkMap,
    /// Chunk bytes this side answers with: the big, slow lane, and the reason
    /// the other three are not it.
    SyncChunks,
    /// Note batches this side pushes.
    Notes,
}

impl Lane {
    fn channel_type(self) -> ChannelType {
        match self {
            Lane::BrowseAsk | Lane::BrowseAnswer => BROWSE,
            Lane::SyncAsk | Lane::SyncManifest | Lane::SyncChunkMap | Lane::SyncChunks => SYNC,
            Lane::Notes => NOTES,
        }
    }

    /// How the lane names itself in a log line or an error a UI may show.
    fn label(self) -> &'static str {
        match self {
            Lane::BrowseAsk => "browse request",
            Lane::BrowseAnswer => "browse answer",
            Lane::SyncAsk => "sync request",
            Lane::SyncManifest => "manifest answer",
            Lane::SyncChunkMap => "chunk map answer",
            Lane::SyncChunks => "chunk answer",
            Lane::Notes => "notes",
        }
    }
}

/// A session's long-lived channels, one per [`Lane`], each opened on first use.
///
/// Shared (via `Arc`) between the request helpers and the reply pumps, because
/// both speak on the same session and a cache per caller would be no cache at
/// all.
struct Channels {
    handle: SessionHandle,
    /// An **async** mutex, held across the open it guards: two callers racing a
    /// lane's first use must not each open a channel for it, which is the leak
    /// this type exists to prevent. Contention costs nothing after that — a lane
    /// is opened once per session, and every later send holds the lock only long
    /// enough to read an id.
    lanes: tokio::sync::Mutex<std::collections::HashMap<Lane, ChannelId>>,
}

impl Channels {
    fn new(handle: SessionHandle) -> Self {
        Channels {
            handle,
            lanes: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Send one frame on `lane`'s channel, opening it on first use.
    ///
    /// `build` takes the channel id because every wire type's `to_frame` does,
    /// and is called again on the retry so the frame names the channel it
    /// actually goes out on.
    ///
    /// Exactly two attempts. The first uses whatever channel the lane already
    /// holds; a send that fails means the channel is gone — the peer closed it,
    /// or its actor errored, and either way the manager has already dropped it —
    /// so the lane is reopened and the frame sent once more. Without that, one
    /// dead channel would end every later request on the lane, which is the same
    /// silent stop as leaking a channel per request, arrived at from the other
    /// direction.
    async fn send<F>(&self, lane: Lane, build: F) -> Result<(), (Code, String)>
    where
        F: Fn(ChannelId) -> Result<SessionFrame, String>,
    {
        let mut stale: Option<ChannelId> = None;
        loop {
            let channel = self.lane_channel(lane, stale).await?;
            let frame = build(channel).map_err(|e| (Code::Internal, e))?;
            match self
                .handle
                .send_on_channel(channel, frame.message_type, frame.flags, frame.payload)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if stale.is_none() => {
                    // Debug, not warn: a channel the peer closed is ordinary,
                    // and the retry below is expected to succeed. Logged at all
                    // so a lane that reopens on every send is visible rather
                    // than merely slow.
                    tracing::debug!(lane = lane.label(), error = %e, "reopening a lane");
                    stale = Some(channel);
                }
                Err(e) => return Err((Code::Connection, format!("{} channel: {e}", lane.label()))),
            }
        }
    }

    /// `lane`'s channel: the one it already holds, or a newly opened one when it
    /// holds none or holds `stale` — the id a caller has just failed to send on.
    ///
    /// The stale id is *compared* rather than trusted, because two callers can
    /// fail on the same dead channel at once and the second must not discard the
    /// replacement the first just opened.
    async fn lane_channel(
        &self,
        lane: Lane,
        stale: Option<ChannelId>,
    ) -> Result<ChannelId, (Code, String)> {
        let mut lanes = self.lanes.lock().await;
        match lanes.get(&lane).copied() {
            Some(live) if Some(live) != stale => return Ok(live),
            Some(dead) => {
                lanes.remove(&lane);
                // Best-effort, and lossless: the send on it already failed, so
                // there is nothing queued to lose, and a channel this session
                // will never speak on again must not go on counting against
                // either side's 256-channel limit.
                self.handle.close_channel(dead);
            }
            None => {}
        }
        let channel = self
            .handle
            .open_channel(lane.channel_type())
            .await
            .map_err(|e| {
                (
                    Code::Connection,
                    format!("open {} channel: {e}", lane.label()),
                )
            })?;
        self.await_open(lane, channel).await?;
        lanes.insert(lane, channel);
        Ok(channel)
    }

    /// Wait, bounded, for the peer to accept `channel`.
    ///
    /// [`SessionHandle::open_channel`] resolves as soon as *this* side has
    /// allocated the channel and queued the open on the wire — it does not wait
    /// for the peer's `ChannelAccept`. A frame sent straight after it therefore
    /// races that accept and hard-fails with "channel not open". Every request in
    /// this module did exactly that, so over any link with real latency the
    /// first frame on each freshly-opened channel — which, before lanes, was
    /// every frame — could be refused locally and reported as a peer that did
    /// not answer.
    ///
    /// The same wait, for the same reason, as
    /// `peerbeam_chat::send::wait_for_channel_open` and its clipboard and
    /// presence twins. A fourth copy rather than a shared helper only because
    /// the shared home is `SessionHandle` in `peerbeam-transfer`, and moving the
    /// other three there is a change to three crates this one merely consumes.
    async fn await_open(&self, lane: Lane, channel: ChannelId) -> Result<(), (Code, String)> {
        let deadline = std::time::Instant::now() + CHANNEL_OPEN_BUDGET;
        loop {
            let snapshot = self
                .handle
                .channels()
                .await
                .map_err(|e| (Code::Connection, format!("{}: {e}", lane.label())))?;
            match snapshot.iter().find(|c| c.id == channel).map(|c| c.state) {
                Some(ChannelState::Opening) => {}
                Some(s) if s.is_open() => return Ok(()),
                // Terminal, or absent — and absent *is* terminal here: the pump
                // registers the channel before `open_channel` returns and reads
                // the snapshot from the same registry over the same ordered
                // command queue, so the only way it can be missing is that it
                // was already removed (refused, or its actor errored).
                _ => {
                    return Err((
                        Code::Connection,
                        format!("{} channel was refused by the device", lane.label()),
                    ))
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err((
                    Code::Connection,
                    format!(
                        "{} channel did not open within {CHANNEL_OPEN_BUDGET:?}",
                        lane.label()
                    ),
                ));
            }
            tokio::time::sleep(CHANNEL_OPEN_POLL).await;
        }
    }
}

/// Send one `ListRequest` and wait for the answer, bounded.
///
/// Bounded because a peer that never answers must not hold a caller forever:
/// browsing is interactive, and a UI waiting on a silent device is worse than
/// one told the device did not answer.
pub async fn request_listing(
    session: &Session,
    path: &str,
) -> Option<peerbeam_browse::ListResponse> {
    use tokio::time::{timeout, Duration};

    let req = peerbeam_browse::ListRequest::new(path);
    // Logged rather than folded into the same `None` a silent peer produces:
    // "we could not ask" and "it did not answer" are different faults, and only
    // the first is ours. Discarding the error left a UI reporting an empty
    // folder for a request that never left the machine.
    if let Err((_, e)) = session
        .channels
        .send(Lane::BrowseAsk, |c| {
            req.to_frame(c).map_err(|e| e.to_string())
        })
        .await
    {
        tracing::warn!(%path, error = %e, "browse request not sent");
        return None;
    }

    // The answer arrives through this session's own Browse handler, which
    // pushes it onto the answers channel; `browse_answers` is drained by the
    // reply pump on the *serving* side, so an asker reads it here instead.
    let rx = session.browse_rx.clone();
    let answer = timeout(Duration::from_secs(10), async move {
        let mut guard = rx.lock().await;
        guard.recv().await
    })
    .await
    .ok()
    .flatten();
    if answer.is_none() {
        tracing::warn!(%path, "device did not answer a browse request");
    }
    answer
}

/// Ask how a file chunks. `None` when the peer cannot say — it predates delta
/// transfer, refused, or the file is too large to chunk — in which case the
/// caller asks for the whole file instead.
pub async fn request_chunk_map(
    session: &Session,
    path: &str,
) -> Option<peerbeam_sync::manifest_wire::ChunkMapResponse> {
    let req = peerbeam_sync::manifest_wire::ChunkMapRequest {
        path: path.to_string(),
    };
    if let Err((_, e)) = session
        .channels
        .send(Lane::SyncAsk, |c| {
            req.to_frame(c).map_err(|e| e.to_string())
        })
        .await
    {
        // A caller reads `None` as "no delta available, send the whole file",
        // which is the right fallback but the wrong diagnosis: without this the
        // whole-file path looks like the peer's choice rather than our failure.
        tracing::warn!(%path, error = %e, "chunk map request not sent");
        return None;
    }
    let rx = session.chunkmap_rx.clone();
    tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        rx.lock().await.recv().await
    })
    .await
    .ok()?
}

/// Ask for specific chunks and collect what arrives.
///
/// Batched, because the peer bounds how many it serves per message: asking for
/// a thousand in one frame would silently get sixty-four.
pub async fn request_chunks(
    session: &Session,
    path: &str,
    hashes: &[String],
) -> std::collections::HashMap<String, Vec<u8>> {
    const BATCH: usize = 64;
    let mut out = std::collections::HashMap::new();
    for group in hashes.chunks(BATCH) {
        let req = peerbeam_sync::manifest_wire::ChunkRequest {
            path: path.to_string(),
            hashes: group.to_vec(),
        };
        if let Err((_, e)) = session
            .channels
            .send(Lane::SyncAsk, |c| {
                req.to_frame(c).map_err(|e| e.to_string())
            })
            .await
        {
            // Returning what arrived so far is deliberate — the caller falls
            // back to the whole file — but a partial delta that came of *our*
            // failure must say so, or the fallback reads as the peer's doing.
            tracing::warn!(%path, error = %e, "chunk request not sent");
            break;
        }
        for _ in 0..group.len() {
            let rx = session.chunk_rx.clone();
            match tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                rx.lock().await.recv().await
            })
            .await
            {
                Ok(Some(d)) => {
                    out.insert(d.hash, d.bytes);
                }
                _ => break,
            }
        }
    }
    out
}

/// Ask a peer for a manifest and wait for it, bounded.
pub async fn request_manifest(session: &Session, path: &str) -> Option<peerbeam_sync::Manifest> {
    use tokio::time::{timeout, Duration};

    let req = peerbeam_sync::ManifestRequest {
        path: path.to_string(),
    };
    if let Err((_, e)) = session
        .channels
        .send(Lane::SyncAsk, |c| {
            req.to_frame(c).map_err(|e| e.to_string())
        })
        .await
    {
        tracing::warn!(%path, error = %e, "manifest request not sent");
        return None;
    }

    let rx = session.sync_rx.clone();
    // A manifest can describe thousands of files, so this waits longer than a
    // browse listing — but still bounded, because a peer that never answers
    // must not hold a caller forever.
    timeout(Duration::from_secs(30), async move {
        let mut guard = rx.lock().await;
        guard.recv().await
    })
    .await
    .ok()
    .flatten()
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries
/// folder sync.
pub fn caps_support_sync(caps: &CapabilitySet) -> bool {
    caps.features(SYNC)
        .is_some_and(|f| f & SYNC_FEAT_MANIFEST != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries
/// browsing.
pub fn caps_support_browse(caps: &CapabilitySet) -> bool {
    caps.features(BROWSE)
        .is_some_and(|f| f & BROWSE_FEAT_LIST != 0)
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// notes sync feature, i.e. whether sending this peer notes would mean
/// anything. A peer that predates notes advertises `features: 0` for the
/// channel it does not have, so this is false and nothing is sent.
pub fn caps_support_notes(caps: &CapabilitySet) -> bool {
    caps.features(NOTES)
        .is_some_and(|f| f & NOTES_FEAT_SYNC != 0)
}

/// Send a sequence of note batches over one Notes channel.
///
/// Opens the channel once for the whole sequence rather than once per batch: a
/// sync of a large set is several frames of the same conversation, and a
/// channel per frame would make the peer's side reassemble an exchange that was
/// never meant to be split.
///
/// Takes a bare handle, so it gets a [`Channels`] of its own rather than the
/// session's. Both callers dial a session for this one exchange and drop it, so
/// a lane cache that lives only as long as the call costs nothing — and it is
/// what buys the sequence a wait for the peer's accept (see
/// [`Channels::await_open`]), which the hand-rolled open here did not do. The
/// **reply pump**, whose session stays up, goes through the session's own lanes
/// so it does not open a channel per answer.
pub async fn send_note_batches(
    handle: &SessionHandle,
    batches: &[peerbeam_notes::NoteBatch],
) -> Result<(), (Code, String)> {
    send_notes_on(&Channels::new(handle.clone()), batches).await
}

/// The batch loop both note senders share.
async fn send_notes_on(
    channels: &Channels,
    batches: &[peerbeam_notes::NoteBatch],
) -> Result<(), (Code, String)> {
    for b in batches {
        channels
            .send(Lane::Notes, |c| b.to_frame(c).map_err(|e| e.to_string()))
            .await?;
    }
    Ok(())
}

/// Send one answer a handler produced, and say whether its pump should carry on.
///
/// `false` only for a session that is gone. An answer that cannot be *encoded*
/// is this one answer's problem and the pump keeps going, because the next
/// request the peer makes is still answerable — the distinction the four hand-
/// written pumps drew with `continue` versus `break`, kept here so one
/// unencodable manifest cannot stop a session answering anything again.
///
/// `what` names the answer in the log line. It is not [`Lane::label`] because
/// the lane is a channel and this is a message: "chunk map" reads better than
/// "chunk map answer channel" in "chunk map answer not delivered".
async fn answer_on<F>(channels: &Channels, lane: Lane, what: &str, build: F) -> bool
where
    F: Fn(ChannelId) -> Result<SessionFrame, String>,
{
    match channels.send(lane, build).await {
        Ok(()) => true,
        Err((Code::Internal, e)) => {
            tracing::debug!(error = %e, "{what} answer could not be encoded");
            true
        }
        Err((_, e)) => {
            tracing::debug!(error = %e, "{what} answers stopped");
            false
        }
    }
}

/// A live PeerSession with its pump running. Holds the incoming-channel receiver
/// so the receiving side can await the peer's transfer channel.
pub struct Session {
    /// Control handle for opening channels / closing.
    pub handle: SessionHandle,
    /// This session's long-lived channels, one per [`Lane`]. Shared with the
    /// reply pumps, which speak on the same session — see [`Channels`] for why
    /// there is one channel per lane rather than one per request.
    channels: Arc<Channels>,
    /// Answers to listings **this side asked for**, delivered by the Browse
    /// handler. Behind a lock because a session is shared and only one caller
    /// waits on an answer at a time.
    pub browse_rx: Arc<tokio::sync::Mutex<UnboundedReceiver<peerbeam_browse::ListResponse>>>,
    /// Manifests **this side asked for**.
    pub sync_rx: Arc<tokio::sync::Mutex<UnboundedReceiver<peerbeam_sync::Manifest>>>,
    /// Chunk maps **this side asked for**.
    pub chunkmap_rx:
        Arc<tokio::sync::Mutex<UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkMapResponse>>>,
    /// Chunk bytes **this side asked for**.
    pub chunk_rx:
        Arc<tokio::sync::Mutex<UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkData>>>,
    /// The authenticated peer's device id.
    pub peer_id: String,
    /// The peer's presented human name (may be empty).
    pub peer_name: String,
    /// The authenticated peer's device id, typed (for trust lookups).
    pub peer_device: DeviceId,
    /// Whether the peer was newly TOFU-pinned during this session's handshake.
    pub newly_trusted: bool,
    /// The first-contact pairing code from this session's handshake (empty for
    /// a resumed session, which has no handshake).
    pub pairing_code: String,
    /// The capabilities both sides agreed on (already intersected).
    ///
    /// Read via [`supports_file_ref`](Self::supports_file_ref). Captured here
    /// rather than fetched on demand because the negotiated value has to be
    /// read before `ps` is consumed by the run loop.
    pub capabilities: CapabilitySet,
    incoming: UnboundedReceiver<IncomingStreamChannel>,
    run: tokio::task::JoinHandle<()>,
    /// The presence heartbeat for this session, if presence was wired.
    ///
    /// Held so [`close`](Self::close) can stop it: the loop's own exit is a
    /// liveness probe on the next tick, which for a withheld heartbeat is up to
    /// a minute away. Aborting is not a shortcut around the gate — the task
    /// re-checks it on every beat and cannot send after this point anyway, its
    /// session being closed.
    presence: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    /// Whether the peer negotiated the chat `FileRef` feature. A peer from
    /// before this feature advertises `features: 0`, so this is false and we
    /// must not offer it a file in chat.
    ///
    /// Checked by `Manager::run_chat_file_send` before it sends anything: a
    /// false here is a hard refusal, never a fallback to a plain transfer. The
    /// receive side needs no such check — it correlates purely off the id it
    /// peeks from the transfer itself.
    #[must_use]
    pub fn supports_file_ref(&self) -> bool {
        caps_support_file_ref(&self.capabilities)
    }

    /// Await the next incoming transfer channel the peer opens (receiver side).
    pub async fn next_incoming(&mut self) -> Option<IncomingStreamChannel> {
        self.incoming.recv().await
    }

    /// Close the session and wait for its pump to finish. The pump task removes
    /// this session from the diagnostics registry when `run` returns.
    pub async fn close(self) {
        if let Some(p) = self.presence {
            p.abort();
        }
        self.handle.close();
        let _ = self.run.await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn establish(
    transport: Arc<dyn ChannelTransport>,
    role: SessionRole,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
    chat: Option<ChatWiring>,
    presence: Option<PresenceWiring>,
    route: Option<RouteKind>,
) -> Result<Session, (Code, String)> {
    // Build the optional chat handler + its peer slot. The slot is bound to
    // the authenticated peer right after `PeerSession::open` returns, before
    // the run loop is spawned, so no Chat frame can be dispatched to an
    // unbound handler.
    let mut handlers: Vec<Arc<dyn MessageHandler>> = Vec::new();
    let mut peer_slot: Option<Arc<OnceLock<DeviceId>>> = None;
    if let Some(w) = chat {
        // Group membership frames ride the Chat channel, so they arrive through
        // the chat handler or not at all — a session maps one handler per
        // channel. The sink is read from the running engine rather than passed
        // in, for the reason the notes wiring below gives: an argument is one
        // more thing a call site can forget, and a dropped group frame is
        // silent. That is not hypothetical — the first cut of groups forwarded
        // nothing, so every invitation that arrived was discarded.
        let (h, slot) = match crate::groups_sync::sink() {
            Some(foreign) => ChatHandler::with_foreign(w.store, w.sink, foreign),
            None => ChatHandler::new(w.store, w.sink),
        };
        peer_slot = Some(slot);
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Notes are wired from the running engine rather than passed in, for the
    // reason clipboard is registered unconditionally: a wiring argument is one
    // more thing a new dial or accept call site can forget, and a session
    // without a `NotesHandler` does not refuse an inbound batch — it drops it
    // silently, so the peer believes it synced and nothing arrived.
    //
    // Registering it is not sharing: `NotesHandler` re-reads
    // `Permission::Notes` per batch, so a device the user never granted it
    // writes nothing here and learns nothing about what is here.
    //
    // The handler's replies cannot be sent at construction time — there is no
    // session yet — so they are queued and drained by a pump spawned once the
    // handshake has completed and the handle exists.
    let mut notes_slot: Option<Arc<OnceLock<DeviceId>>> = None;
    let mut notes_replies: Option<UnboundedReceiver<Vec<peerbeam_notes::NoteBatch>>> = None;
    if let Some(w) = crate::notes_sync::wiring() {
        let (tx, rx) = unbounded_channel();
        let sink: peerbeam_notes::ReplySink = Arc::new(move |batches| {
            // A closed receiver means the session is gone; the reply is simply
            // not sent, which is what an unreachable peer looks like anyway.
            let _ = tx.send(batches);
        });
        let (h, slot) = peerbeam_notes::NotesHandler::new(w.store, w.trust, sink);
        notes_slot = Some(slot);
        notes_replies = Some(rx);
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Browse, wired from the running engine for the same reason notes are: a
    // session with no `BrowseHandler` drops a request silently, leaving the
    // asker waiting rather than told "nothing". Registering it shares nothing —
    // the handler re-reads `Permission::Browse` per request, and the share list
    // is empty until the user configures one.
    // Registered unconditionally, so these are always set — unlike chat and
    // notes, which are optional wirings.
    let browse_slot: Arc<OnceLock<DeviceId>>;
    let browse_answers: UnboundedReceiver<peerbeam_browse::ListResponse>;
    let browse_incoming: UnboundedReceiver<peerbeam_browse::ListResponse>;
    {
        let (tx, rx) = unbounded_channel();
        let sink: peerbeam_browse::AnswerSink = Arc::new(move |r| {
            let _ = tx.send(r);
        });
        let (itx, irx) = unbounded_channel();
        let incoming: peerbeam_browse::IncomingSink = Arc::new(move |r| {
            let _ = itx.send(r);
        });
        let (h, slot) = peerbeam_browse::BrowseHandler::new(
            crate::browse::shares(),
            trust.clone(),
            sink,
            incoming,
        );
        browse_slot = slot;
        browse_answers = rx;
        browse_incoming = irx;
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Folder sync, registered unconditionally alongside browsing: it serves
    // the same shares and answers the same "may you look" question, and a
    // session without it drops a manifest request silently.
    let sync_slot: Arc<OnceLock<DeviceId>>;
    let sync_answers: UnboundedReceiver<peerbeam_sync::Manifest>;
    let sync_incoming: UnboundedReceiver<peerbeam_sync::Manifest>;
    let chunkmap_out: UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkMapResponse>;
    let chunk_out: UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkData>;
    let sync_files: UnboundedReceiver<std::path::PathBuf>;
    let chunkmap_answers: UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkMapResponse>;
    let chunk_answers: UnboundedReceiver<peerbeam_sync::manifest_wire::ChunkData>;
    {
        let (tx, rx) = unbounded_channel();
        let (itx, irx) = unbounded_channel();
        let (ftx, frx) = unbounded_channel();
        // Answers this device *sends* go out over the session; answers it
        // *receives* are routed to whoever asked.
        let (cm_out, cm_out_rx) = unbounded_channel();
        let (cd_out, cd_out_rx) = unbounded_channel();
        let (cm_in, cm_in_rx) = unbounded_channel();
        let (cd_in, cd_in_rx) = unbounded_channel();
        let (h, slot) = peerbeam_sync::SyncHandler::with_chunks(
            crate::browse::shares(),
            trust.clone(),
            Arc::new(move |m| {
                let _ = tx.send(m);
            }),
            Arc::new(move |p| {
                let _ = ftx.send(p);
            }),
            Arc::new(move |m| {
                let _ = itx.send(m);
            }),
            Arc::new(move |r| {
                let _ = cm_out.send(r);
            }),
            Arc::new(move |d| {
                let _ = cd_out.send(d);
            }),
            Arc::new(move |r| {
                let _ = cm_in.send(r);
            }),
            Arc::new(move |d| {
                let _ = cd_in.send(d);
            }),
        );
        chunkmap_answers = cm_in_rx;
        chunk_answers = cd_in_rx;
        chunkmap_out = cm_out_rx;
        chunk_out = cd_out_rx;
        sync_slot = slot;
        sync_answers = rx;
        sync_incoming = irx;
        sync_files = frx;
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Presence gets the same treatment for the same reason: an unregistered
    // handler means an inbound Status is silently dropped, not refused.
    let mut presence_slot: Option<Arc<OnceLock<DeviceId>>> = None;
    if let Some(w) = &presence {
        let sink: PresenceSink =
            Arc::new(|peer, entry| crate::presence::emit_updated(&peer, &entry));
        // Ringing is gated where it is acted on, not here: the handler only
        // reports that a peer asked, and `crate::presence::ring_sink` decides
        // whether this device answers.
        let (h, slot) =
            PresenceHandler::new(w.registry.clone(), sink, crate::presence::ring_sink());
        presence_slot = Some(slot);
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Clipboard is registered **unconditionally** — there is no `Option`, no
    // wiring struct and therefore no call site that can forget it. It needs no
    // per-session state (no store, no registry), so the shape `ChatWiring` and
    // `PresenceWiring` exist to make explicit has nothing to carry here, and
    // the missed-call-site bug they guard against cannot occur.
    //
    // Registering it is not sharing. This side only *receives* through the
    // handler; whether anything leaves is decided per push by
    // `may_share_clip`, and with the opt-in off (the default) no clipboard
    // channel is ever opened.
    let (clipboard_handler, clipboard_slot) =
        ClipboardHandler::new(crate::clipboard::sink(), trust.clone());
    handlers.push(clipboard_handler as Arc<dyn MessageHandler>);
    let (ev, _ev) = unbounded_channel();
    let (ch, _ch) = unbounded_channel();
    let (inc, incoming) = unbounded_channel();
    // Register into the shared diagnostics registry so pb_session_* / transport /
    // recovery report this live session (the registry is the seam the engine's
    // SessionDiagnostics reads). Absent only if the runtime is not initialised.
    let diag = crate::runtime::diagnostics().ok();
    let registry = diag.as_ref().map(|d| d.registry());
    let mut ps = PeerSession::open(
        transport,
        role,
        session_cfg(handlers),
        ev,
        ch,
        inc,
        registry,
        ident,
        enc,
        trust.clone(),
    )
    .await
    .map_err(|e| (Code::Connection, format!("session establish failed: {e}")))?;
    // Bind the chat peer before the run loop dispatches any frame.
    if let Some(slot) = peer_slot {
        let _ = slot.set(ps.peer().clone());
    }
    if let Some(slot) = notes_slot {
        let _ = slot.set(ps.peer().clone());
    }
    let _ = browse_slot.set(ps.peer().clone());
    let _ = sync_slot.set(ps.peer().clone());
    if let Some(slot) = presence_slot {
        let _ = slot.set(ps.peer().clone());
    }
    let _ = clipboard_slot.set(ps.peer().clone());
    let id = ps.id();
    let peer_device = ps.peer().clone();
    let peer_id = peer_device.0.clone();
    let peer_name = ps.peer_name().to_string();
    let newly_trusted = ps.newly_trusted();
    let pairing_code = ps.pairing_code().to_string();
    // Read alongside the other post-handshake fields, before `ps` moves into
    // the run closure below — this is the negotiated (intersected) set, so it
    // already reflects what the *peer* advertised, not just what we asked for.
    let capabilities = ps.capabilities().clone();
    let handle = ps.handle();
    // One lane set for the whole session, shared by the request helpers and
    // every reply pump below. Built here, before any pump is spawned, so no pump
    // can be given a cache of its own and quietly go back to a channel per
    // answer.
    let channels = Arc::new(Channels::new(handle.clone()));
    // Drain the notes handler's replies onto this session. Spawned rather than
    // awaited: an answer is owed *after* frames start arriving, which is long
    // after this function returns.
    {
        let mut rx = sync_answers;
        let lanes = channels.clone();
        crate::runtime::spawn(async move {
            while let Some(m) = rx.recv().await {
                if !answer_on(&lanes, Lane::SyncManifest, "manifest", |c| {
                    m.to_frame(c).map_err(|e| e.to_string())
                })
                .await
                {
                    break;
                }
            }
        });
    }
    // The same drain for chunk answers. Two loops rather than one over an enum:
    // a chunk map is one small message per request and chunk bytes are many
    // larger ones, so a slow chunk send must not hold up the map that tells the
    // peer what to ask for next.
    {
        let mut rx = chunkmap_out;
        let lanes = channels.clone();
        crate::runtime::spawn(async move {
            while let Some(m) = rx.recv().await {
                if !answer_on(&lanes, Lane::SyncChunkMap, "chunk map", |c| {
                    m.to_frame(c).map_err(|e| e.to_string())
                })
                .await
                {
                    break;
                }
            }
        });
    }
    {
        let mut rx = chunk_out;
        let lanes = channels.clone();
        crate::runtime::spawn(async move {
            while let Some(d) = rx.recv().await {
                if !answer_on(&lanes, Lane::SyncChunks, "chunk", |c| {
                    d.to_frame(c).map_err(|e| e.to_string())
                })
                .await
                {
                    break;
                }
            }
        });
    }
    // A peer asked for a file it is allowed to have. Sending it is the Transfer
    // channel's job, so the request surfaces as an event rather than being
    // served here: this crate owns no second bulk path.
    {
        let mut rx = sync_files;
        let peer_for_files = peer_device.clone();
        crate::runtime::spawn(async move {
            while let Some(path) = rx.recv().await {
                // **This does not send the file, and nothing else does.**
                // `sync_file_requested` is emitted and no code in this
                // repository consumes it — not the Flutter event model, not the
                // SDK, not the docs. A peer that asks for a whole file gets
                // nothing back and no error, so it is logged here where the
                // request is known.
                //
                // Nothing asks any more: both delta paths stream, so a large
                // file no longer falls back to a whole-file request. The
                // message and its handler stay because an older peer may still
                // send one, and answering with silence is at least consistent
                // with what it has always done.
                tracing::warn!(
                    path = %path.to_string_lossy(),
                    "a peer asked for a whole file over the sync channel; \
                     this build does not serve that request"
                );
                crate::events::emit(&serde_json::json!({
                    "type": "sync_file_requested",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "payload": {
                        "device_id": peer_for_files.0,
                        "path": path.to_string_lossy(),
                    },
                }));
            }
        });
    }
    {
        let mut rx = browse_answers;
        let lanes = channels.clone();
        crate::runtime::spawn(async move {
            while let Some(answer) = rx.recv().await {
                if !answer_on(&lanes, Lane::BrowseAnswer, "browse listing", |c| {
                    answer.to_frame(c).map_err(|e| e.to_string())
                })
                .await
                {
                    break;
                }
            }
        });
    }
    if let Some(mut rx) = notes_replies {
        let lanes = channels.clone();
        crate::runtime::spawn(async move {
            while let Some(batches) = rx.recv().await {
                // The session's own Notes lane, not `send_note_batches`' bare
                // handle: this pump answers every batch the peer pushes over a
                // session that stays up, so a channel per answer is exactly the
                // leak that stopped a long-lived session syncing.
                if send_notes_on(&lanes, &batches).await.is_err() {
                    // The session is gone. A reply that cannot be delivered is
                    // exactly what an unreachable peer looks like, and the next
                    // sync starts from both sides' stored sets anyway.
                    break;
                }
            }
        });
    }
    if let Some(d) = &diag {
        d.register_handle(id, handle.clone());
    }
    let run = crate::runtime::spawn_handle(async move {
        let _ = ps.run().await;
        // The session has ended — a clean close, or a transport loss that the
        // FFI does not recover (it runs the pump directly, with no
        // RecoveryManager, and discards RunExit::Lost). capture_loss marked the
        // registry entry `Recovering`; with nothing to finish it, remove the
        // entry + handle here so diagnostics don't leak a permanently-recovering
        // session (and the recovering count stays accurate).
        if let Some(d) = diag {
            d.registry().remove(id);
            d.unregister_handle(id);
        }
    });
    // Start the heartbeat only if presence was wired. The task itself decides
    // whether anything actually goes out: `PresenceSender::beat` consults
    // `may_share_status` before it opens a channel or sends a frame, re-reading
    // the opt-in setting and the trust store on every beat. Spawning it for a
    // peer we may not share with is therefore not a leak — a withheld beat
    // opens nothing and sends nothing — and it is what lets turning the setting
    // on take effect on an already-live session.
    let presence_task = presence.as_ref().map(|w| {
        let sender = PresenceSender::new(
            handle.clone(),
            peer_device.clone(),
            capabilities.clone(),
            trust.clone(),
            Arc::new(crate::presence::sharing_enabled),
            w.source(route),
        );
        crate::runtime::spawn_handle(sender.run(HEARTBEAT_INTERVAL))
    });
    Ok(Session {
        handle,
        channels,
        browse_rx: Arc::new(tokio::sync::Mutex::new(browse_incoming)),
        sync_rx: Arc::new(tokio::sync::Mutex::new(sync_incoming)),
        chunkmap_rx: Arc::new(tokio::sync::Mutex::new(chunkmap_answers)),
        chunk_rx: Arc::new(tokio::sync::Mutex::new(chunk_answers)),
        peer_id,
        peer_name,
        peer_device,
        newly_trusted,
        pairing_code,
        capabilities,
        incoming,
        run,
        presence: presence_task,
    })
}

/// Dial `device` and establish an initiator PeerSession over the RouteManager's
/// best available route (one attempt across ranked candidates).
#[allow(clippy::too_many_arguments)]
pub async fn dial(
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &Device,
    meta: &TransferSession,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
    chat: Option<ChatWiring>,
    presence: Option<PresenceWiring>,
) -> Result<Session, (Code, String)> {
    let candidates = routes.candidates(device);
    if candidates.is_empty() {
        return Err((Code::Connection, format!("no routes to {}", device.name)));
    }
    let mut last: Option<(Code, String)> = None;
    for route in candidates {
        // The route class the RouteManager already assigned this candidate —
        // reused verbatim rather than reclassified, so presence reports exactly
        // the class route selection acted on.
        let kind = route.kind;
        match quic.dial_channels(&route, meta).await {
            Ok(qc) => {
                // Kept as a concrete `Arc<QuicChannels>` alongside the erased
                // transport so the link's round-trip time can still be read
                // after the session is up. `Arc<dyn ChannelTransport>` cannot
                // answer that question, and re-measuring by any other means
                // would be a probe this connection does not need.
                let qc = Arc::new(qc);
                let transport: Arc<dyn ChannelTransport> = qc.clone();
                match establish(
                    transport,
                    SessionRole::Initiator,
                    ident.clone(),
                    enc.clone(),
                    trust.clone(),
                    chat.clone(),
                    presence.clone(),
                    Some(kind),
                )
                .await
                {
                    Ok(session) => {
                        // Read here rather than straight after the dial: by
                        // now the QUIC handshake *and* the PeerSession auth
                        // exchange have both completed, so quinn's estimator
                        // is running on several real samples instead of the
                        // one the transport handshake alone would give it.
                        //
                        // Keyed by the **discovery** id, not the authenticated
                        // one: this is the row the user acted on, and for a
                        // Tailscale-discovered peer the two genuinely differ
                        // (`ts:<node>` against the device's own id).
                        routes.record_link_rtt(&device.id, qc.rtt());
                        return Ok(session);
                    }
                    Err(e) => last = Some(e),
                }
            }
            Err(e) => last = Some(from_domain(e)),
        }
    }
    Err(last.unwrap_or((
        Code::Connection,
        format!("all routes to {} failed", device.name),
    )))
}

/// Accept a responder PeerSession over an inbound channel connection.
///
/// `routes` is used only to classify the inbound connection's remote address
/// for presence — through the RouteManager's own classifier, so an accepted
/// session reports the same vocabulary a dialled one does instead of leaving
/// `network` absent on half of all sessions.
#[allow(clippy::too_many_arguments)]
pub async fn accept(
    qc: QuicChannels,
    routes: &RouteManager,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
    chat: Option<ChatWiring>,
    presence: Option<PresenceWiring>,
) -> Result<Session, (Code, String)> {
    let qc = Arc::new(qc);
    let route = presence
        .is_some()
        .then(|| routes.classify(&qc.remote().ip().to_string()));
    let transport: Arc<dyn ChannelTransport> = qc.clone();
    let session = establish(
        transport,
        SessionRole::Responder,
        ident,
        enc,
        trust,
        chat,
        presence,
        route,
    )
    .await?;
    // Keyed by the authenticated peer id, because it is the only id an inbound
    // connection carries — there is no discovery record behind it — and it is
    // the id the LAN and mDNS rows are keyed by. A Tailscale-discovered row is
    // keyed by `ts:<node>` instead and is simply not updated by this side,
    // which is honest: nothing here can prove the two name one machine.
    routes.record_link_rtt(&session.peer_device, qc.rtt());
    Ok(session)
}

#[cfg(test)]
mod lane_tests {
    //! The session's long-lived channels, over a real QUIC session pair.
    //!
    //! Real QUIC and not the in-memory transport on purpose: both properties
    //! under test are about a peer that answers over a network round trip
    //! rather than instantly, and an in-memory pair hides exactly that.

    use super::*;
    use futures::StreamExt;
    use peerbeam_crypto::AeadCrypto;
    use peerbeam_domain::entity::{Direction, TransferStatus};
    use peerbeam_domain::id::TransferId;
    use peerbeam_transfer_quic::direct_route;

    fn security(name: &str) -> (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>) {
        let enc = AeadCrypto::new();
        let keypair = enc.generate_keypair();
        let identity = Identity {
            device_id: DeviceId::from(name),
            name: name.to_string(),
            keypair,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let trust =
            peerbeam_trust_fs::FsTrust::open(dir.path().join("trust.json")).expect("trust store");
        // The temp dir must outlive the session; leaking in a test is fine.
        std::mem::forget(dir);
        (identity, Arc::new(enc), Arc::new(trust))
    }

    fn meta() -> TransferSession {
        TransferSession {
            id: TransferId::from("lanes"),
            peer: DeviceId::from("peer"),
            direction: Direction::Sending,
            status: TransferStatus::Transferring,
            files: Vec::new(),
            total_bytes: 0,
            transferred_bytes: 0,
            started_at: chrono::Utc::now(),
            completed_at: None,
            is_resume: false,
            accepted: true,
        }
    }

    /// A running session pair over loopback QUIC, yielding the dialling side's
    /// handle. Both sides advertise this build's real capability set, and
    /// neither registers a handler: these tests count channels, not answers, and
    /// a frame with no handler is decoded and dropped exactly as one on a
    /// capability the peer does not serve would be.
    async fn dialling_handle() -> SessionHandle {
        let (server_id, server_enc, server_trust) = security("pb-server");
        let server_quic = QuicTransport::new().expect("server quic");
        let (addr, mut incoming) = server_quic
            .serve_channels_on("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("listen");

        let accepted = tokio::spawn(async move {
            let qc = incoming.next().await.expect("a connection").expect("qc");
            let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
            let (ev, _ev) = unbounded_channel();
            let (ch, _ch) = unbounded_channel();
            let (inc, _inc) = unbounded_channel();
            let mut ps = PeerSession::open(
                transport,
                SessionRole::Responder,
                session_cfg(Vec::new()),
                ev,
                ch,
                inc,
                None,
                server_id,
                server_enc,
                server_trust,
            )
            .await
            .expect("responder session");
            // The receivers are held for the pump's whole life: dropping them
            // would make the manager treat a paired channel as unreceivable and
            // refuse it, which is a different test.
            let held = (_ev, _ch, _inc);
            let _ = ps.run().await;
            drop(held);
        });

        let (client_id, client_enc, client_trust) = security("pb-client");
        let client_quic = QuicTransport::new().expect("client quic");
        let qc = client_quic
            .dial_channels(&direct_route("127.0.0.1", addr.port()), &meta())
            .await
            .expect("dial");
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let (ev, _ev) = unbounded_channel();
        let (ch, _ch) = unbounded_channel();
        let (inc, _inc) = unbounded_channel();
        let mut ps = PeerSession::open(
            transport,
            SessionRole::Initiator,
            session_cfg(Vec::new()),
            ev,
            ch,
            inc,
            None,
            client_id,
            client_enc,
            client_trust,
        )
        .await
        .expect("initiator session");
        let handle = ps.handle();
        tokio::spawn(async move {
            let held = (_ev, _ch, _inc, accepted, client_quic, server_quic);
            let _ = ps.run().await;
            drop(held);
        });
        handle
    }

    fn listing_request(path: &str) -> peerbeam_browse::ListRequest {
        peerbeam_browse::ListRequest::new(path)
    }

    /// **A long-lived session must keep asking.** Every request helper here
    /// opened a channel and closed none, so past `DEFAULT_CHANNEL_LIMIT` (256)
    /// requests the session's own `permit` refused to open another and folder
    /// sync stopped fetching — silently, the failure arriving as the `None` a
    /// peer that did not answer produces. A folder sync issues a chunk-map
    /// request plus several chunk requests per file, so this is a few hundred
    /// files, not a pathological peer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lane_reuses_one_channel_so_a_long_session_never_hits_the_limit() {
        let handle = dialling_handle().await;
        let channels = Channels::new(handle.clone());

        // Comfortably past the 256-channel limit a channel-per-request hits.
        for i in 0..400 {
            let req = listing_request("share");
            channels
                .send(Lane::BrowseAsk, |c| {
                    req.to_frame(c).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|(_, e)| panic!("request {i} was not sent: {e}"));
        }

        let live = handle.channels().await.expect("channel snapshot");
        assert_eq!(
            live.len(),
            1,
            "400 requests opened {} channels: {live:?}",
            live.len()
        );
        assert_eq!(live[0].channel_type, BROWSE);
        // And all 400 really rode that one channel. Awaited rather than read
        // once: `send_on_channel` resolves when the frame is queued on the
        // channel actor, and the actor counts it after the write, so a single
        // read races the tail of the queue rather than measuring anything.
        wait_for_frames_sent(&handle, live[0].id, 401).await;
    }

    /// Wait, bounded, until `channel` has sent `want` frames — the requests plus
    /// the one probe frame `open_channel` uses to materialise the stream.
    async fn wait_for_frames_sent(handle: &SessionHandle, channel: ChannelId, want: u64) {
        let deadline = std::time::Instant::now() + CHANNEL_OPEN_BUDGET;
        loop {
            let sent = handle
                .channels()
                .await
                .expect("channel snapshot")
                .iter()
                .find(|c| c.id == channel)
                .map_or(0, |c| c.stats.frames_sent);
            if sent >= want {
                assert_eq!(sent, want, "an unexpected extra frame went out");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {sent} of {want} frames went out"
            );
            tokio::time::sleep(CHANNEL_OPEN_POLL).await;
        }
    }

    /// Lanes are separate channels, which is what keeps a lane's traffic from
    /// queueing behind another's: a channel actor writes its frames serially, so
    /// megabytes of chunk bytes sharing a channel with the chunk map that tells
    /// the peer what to ask for next would hold that map up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lanes_of_the_same_capability_get_separate_channels() {
        let handle = dialling_handle().await;
        let channels = Channels::new(handle.clone());
        let req = listing_request("share");
        for lane in [Lane::BrowseAsk, Lane::BrowseAnswer] {
            channels
                .send(lane, |c| req.to_frame(c).map_err(|e| e.to_string()))
                .await
                .expect("sent");
        }
        let live = handle.channels().await.expect("channel snapshot");
        assert_eq!(live.len(), 2, "two lanes shared one channel: {live:?}");
    }

    /// A lane whose channel dies must reopen rather than fail for ever. Without
    /// it, one closed channel ends every later request on the lane — the same
    /// silent stop the limit produced, reached from the other direction.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lane_whose_channel_dies_reopens_on_the_next_send() {
        let handle = dialling_handle().await;
        let channels = Channels::new(handle.clone());
        let req = listing_request("share");

        channels
            .send(Lane::BrowseAsk, |c| {
                req.to_frame(c).map_err(|e| e.to_string())
            })
            .await
            .expect("first request sent");
        let first = handle.channels().await.expect("snapshot");
        assert_eq!(first.len(), 1);
        let dead = first[0].id;

        // Close it out from under the lane, exactly as a peer hanging up would.
        handle.close_channel(dead);
        let deadline = std::time::Instant::now() + CHANNEL_OPEN_BUDGET;
        while handle
            .channels()
            .await
            .expect("snapshot")
            .iter()
            .any(|c| c.id == dead)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the channel never closed"
            );
            tokio::time::sleep(CHANNEL_OPEN_POLL).await;
        }

        channels
            .send(Lane::BrowseAsk, |c| {
                req.to_frame(c).map_err(|e| e.to_string())
            })
            .await
            .expect("the lane reopened and the request went out");
        let live = handle.channels().await.expect("snapshot");
        assert_eq!(
            live.len(),
            1,
            "the lane holds exactly one channel: {live:?}"
        );
        assert_ne!(live[0].id, dead, "and it is a new one");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a peer that predates file-in-chat advertises for CHAT.
    fn legacy_peer_caps() -> CapabilitySet {
        CapabilitySet::new().with(Capability::new(CHAT))
    }

    /// What this build advertises, read out of the **actual `SessionConfig`**
    /// every session is built from — not `advertised_caps()`, and certainly not
    /// a restatement in this module. Both weaker forms leave a hop where
    /// `session_cfg` could stop advertising while these tests stayed green.
    fn our_caps() -> CapabilitySet {
        session_cfg(Vec::new()).capabilities
    }

    /// **The folder-acknowledgement bit must be advertised by this frontend.**
    ///
    /// One test per surface, for the reason `both_chat_feature_bits_are_advertised`
    /// gives: 2a shipped with only one of the two frontends advertising
    /// `CHAT_FEAT_FILEREF`, so a peer's behaviour depended on which of our own
    /// surfaces it happened to reach. Here the cost of that mistake is higher —
    /// an unadvertised bit means folder sends silently go back to reporting
    /// success for bytes that were never confirmed.
    #[test]
    fn the_folder_ack_bit_is_advertised() {
        let caps = our_caps();
        let f = caps
            .features(ChannelType::TRANSFER)
            .expect("TRANSFER advertised");
        assert!(
            f & TRANSFER_FEAT_FOLDER_ACK != 0,
            "folder sends will not be confirmed"
        );
        assert!(caps_support_folder_ack(&caps), "and the predicate agrees");
    }

    /// A peer that predates the bit negotiates it away, and then nothing is
    /// expected and nothing is sent — which is what keeps this additive.
    #[test]
    fn an_older_peer_negotiates_the_folder_ack_away() {
        let legacy = CapabilitySet::new().with(Capability::new(ChannelType::TRANSFER));
        let negotiated = our_caps().intersect(&legacy);
        assert!(
            !caps_support_folder_ack(&negotiated),
            "we would wait for a confirmation an older peer never sends"
        );
    }

    /// Both chat feature bits must be advertised by **this** frontend. The CLI
    /// has the identical test: 2a shipped with only one of the two surfaces
    /// advertising `CHAT_FEAT_FILEREF`, so a peer's behaviour depended on which
    /// of our own frontends it reached. One test per surface is what makes that
    /// impossible to repeat.
    #[test]
    fn both_chat_feature_bits_are_advertised() {
        let caps = session_cfg(Vec::new()).capabilities;
        let f = caps.features(ChannelType::CHAT).expect("CHAT advertised");
        assert!(f & CHAT_FEAT_FILEREF != 0, "file sharing");
        assert!(f & CHAT_FEAT_FILEDECLINE != 0, "decline signalling");
    }

    /// A 2a-era peer negotiates the decline bit away, so we must never send it
    /// one — the same ANDing that gates `FileRef`, checked independently
    /// because the two bits gate two different sends.
    #[test]
    fn a_legacy_peer_negotiates_the_decline_bit_away() {
        let negotiated = our_caps().intersect(&legacy_peer_caps());
        assert!(!caps_support_file_decline(&negotiated));
        assert!(caps_support_file_decline(
            &our_caps().intersect(&our_caps())
        ));
    }

    /// The bits are independent: a peer that speaks `FileRef` but not
    /// `FileDecline` (exactly what a 2a build advertises) must read as
    /// file-sharing-capable and decline-incapable, not both or neither.
    #[test]
    fn the_two_chat_feature_bits_are_read_independently() {
        let file_ref_only =
            CapabilitySet::new().with(Capability::with_features(CHAT, CHAT_FEAT_FILEREF));
        let negotiated = our_caps().intersect(&file_ref_only);
        assert!(caps_support_file_ref(&negotiated));
        assert!(!caps_support_file_decline(&negotiated));
        assert_ne!(
            CHAT_FEAT_FILEREF, CHAT_FEAT_FILEDECLINE,
            "two features sharing a bit would make the check meaningless"
        );
    }

    /// The negotiation contract the whole feature rests on: `intersect` ANDs
    /// the feature bits, so advertising `CHAT_FEAT_FILEREF` against a peer that
    /// advertises `features: 0` yields a *negotiated* set without the bit — and
    /// we therefore never offer that peer a file in chat.
    #[test]
    fn a_peer_without_the_feature_bit_negotiates_to_unsupported() {
        let negotiated = our_caps().intersect(&legacy_peer_caps());
        assert!(
            negotiated.supports(CHAT),
            "CHAT itself still negotiates — only the feature is absent"
        );
        assert_eq!(negotiated.features(CHAT), Some(0), "the bit is ANDed away");
        assert!(!caps_support_file_ref(&negotiated));
    }

    /// Two builds that both advertise it keep it after intersection.
    #[test]
    fn two_peers_with_the_feature_bit_negotiate_to_supported() {
        let negotiated = our_caps().intersect(&our_caps());
        assert!(caps_support_file_ref(&negotiated));
    }

    /// The PRESENCE bit must be advertised by **this** frontend. The other
    /// surface has the identical test: 2a shipped with only one of the two
    /// advertising `CHAT_FEAT_FILEREF`, so a peer's behaviour depended on which
    /// of our own frontends it reached. One test per surface is what makes that
    /// impossible to repeat — a shared helper would go green for both the
    /// moment either stopped advertising.
    #[test]
    fn the_presence_feature_bit_is_advertised() {
        let caps = session_cfg(Vec::new()).capabilities;
        let f = caps
            .features(ChannelType::PRESENCE)
            .expect("PRESENCE advertised");
        assert!(f & PRESENCE_FEAT_STATUS != 0, "device status heartbeats");
    }

    /// Advertising PRESENCE is a claim about comprehension, not about
    /// behaviour: this build understands a Status. Whether it ever *sends* one
    /// is the opt-in setting's business, and that setting is off by default.
    /// Conflating the two would mean a device could only receive status if it
    /// also shared its own.
    #[test]
    fn advertising_presence_does_not_imply_sharing() {
        let ours = session_cfg(Vec::new()).capabilities;
        let negotiated = ours.intersect(&ours);
        assert!(
            peerbeam_presence::caps_support_status(&negotiated),
            "two of our builds negotiate the capability"
        );
        // ...and the capability alone opens nothing: the two privacy gates are
        // separate inputs to the same decision.
        assert!(!peerbeam_presence::may_share_status(
            false, // sharing off
            &NeverTrusts,
            &DeviceId::from("pb-bob"),
            &negotiated,
        ));
    }

    /// A 1b/2a-era peer advertises no PRESENCE at all, so the intersection
    /// drops it and it is never sent a Status.
    #[test]
    fn a_peer_without_presence_negotiates_to_unsupported() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(TRANSFER))
            .with(Capability::new(CHAT));
        let negotiated = session_cfg(Vec::new()).capabilities.intersect(&legacy);
        assert!(!negotiated.supports(ChannelType::PRESENCE));
        assert!(!peerbeam_presence::caps_support_status(&negotiated));
    }

    /// An unrelated future bit on PRESENCE must not be read as this one.
    #[test]
    fn an_unrelated_future_presence_bit_does_not_imply_status() {
        let future = CapabilitySet::new().with(Capability::with_features(PRESENCE, 1 << 4));
        let negotiated = session_cfg(Vec::new()).capabilities.intersect(&future);
        assert!(!peerbeam_presence::caps_support_status(&negotiated));
    }

    /// The CLIPBOARD bit must be advertised by **this** frontend. The CLI has
    /// the identical test against its own `session_cfg`, for the reason spelled
    /// out on `the_presence_feature_bit_is_advertised`: a shared helper would go
    /// green for both surfaces the moment either stopped advertising.
    #[test]
    fn the_clipboard_feature_bit_is_advertised() {
        let f = our_caps()
            .features(ChannelType::CLIPBOARD)
            .expect("CLIPBOARD advertised");
        assert!(f & CLIPBOARD_FEAT_CLIP != 0, "clipboard sync");
    }

    /// Advertising CLIPBOARD is a claim about comprehension, not about
    /// behaviour: this build understands a `Clip`. Whether it ever *sends* one
    /// is the opt-in setting's business, and that setting is off by default.
    /// Conflating the two would mean a device could only receive a clip if it
    /// also synced its own — which on Android, where auto-send is impossible,
    /// would mean never receiving one at all.
    #[test]
    fn advertising_clipboard_does_not_imply_syncing() {
        let negotiated = our_caps().intersect(&our_caps());
        assert!(
            peerbeam_clipboard::caps_support_clip(&negotiated),
            "two of our builds negotiate the capability"
        );
        assert!(!peerbeam_clipboard::may_share_clip(
            false, // sync off
            &NeverTrusts,
            &DeviceId::from("pb-bob"),
            &negotiated,
        ));
    }

    /// A peer from before clipboard sync advertises no CLIPBOARD at all, so the
    /// intersection drops it and it is never sent a Clip.
    #[test]
    fn a_peer_without_clipboard_negotiates_to_unsupported() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(TRANSFER))
            .with(Capability::new(CHAT));
        let negotiated = our_caps().intersect(&legacy);
        assert!(!negotiated.supports(ChannelType::CLIPBOARD));
        assert!(!peerbeam_clipboard::caps_support_clip(&negotiated));
    }

    /// An unrelated future bit on CLIPBOARD must not be read as this one.
    #[test]
    fn an_unrelated_future_clipboard_bit_does_not_imply_clip() {
        let future = CapabilitySet::new().with(Capability::with_features(CLIPBOARD, 1 << 4));
        let negotiated = our_caps().intersect(&future);
        assert!(!peerbeam_clipboard::caps_support_clip(&negotiated));
    }

    /// The PIPE bit must be advertised by **this** frontend. The CLI has the
    /// identical test against its own `session_cfg`, for the reason spelled out
    /// on `the_presence_feature_bit_is_advertised`: a shared helper would go
    /// green for both surfaces the moment either stopped advertising.
    #[test]
    fn the_pipe_feature_bit_is_advertised() {
        let f = our_caps().features(PIPE).expect("PIPE advertised");
        assert!(f & PIPE_FEAT_STREAM != 0, "byte pipes");
    }

    /// PIPE must be a **stream** channel type here too. Unregistered, an
    /// inbound pipe would pair as a handler-less message channel and its frames
    /// would be decoded, counted and dropped in silence — the sender would hang
    /// rather than be refused. Registered, it reaches `handle_incoming`, which
    /// refuses it with a reason.
    #[test]
    fn pipe_is_a_stream_channel_type() {
        let cfg = session_cfg(Vec::new());
        assert!(cfg.stream_channel_types.contains(&PIPE));
        assert!(
            cfg.stream_channel_types.contains(&TRANSFER),
            "and transfer still is"
        );
    }

    /// **The GUI advertises the capability and accepts no pipe**, which is the
    /// whole point of putting the refusal in the handler rather than in a
    /// narrower advertisement: comprehension is claimed truthfully, acceptance
    /// is refused locally, and a peer's behaviour does not depend on which of
    /// our frontends it reached.
    #[test]
    fn advertising_pipe_does_not_imply_accepting_one() {
        let negotiated = our_caps().intersect(&our_caps());
        assert!(
            peerbeam_transfer::caps_support_stream(&negotiated),
            "two of our builds negotiate the capability"
        );
        assert!(
            !peerbeam_transfer::may_accept_pipe(
                false, // the GUI is never `pipe --listen`
                &AlwaysTrusts,
                &DeviceId::from("pb-bob"),
                None,
                &negotiated,
            ),
            "the GUI must refuse even a trusted, fully capable peer"
        );
    }

    /// A peer from before `peerbeam pipe` advertises no PIPE at all, so the
    /// intersection drops it.
    #[test]
    fn a_peer_without_pipe_negotiates_to_unsupported() {
        let legacy = CapabilitySet::new()
            .with(Capability::new(TRANSFER))
            .with(Capability::new(CHAT));
        let negotiated = our_caps().intersect(&legacy);
        assert!(!negotiated.supports(PIPE));
        assert!(!peerbeam_transfer::caps_support_stream(&negotiated));
    }

    /// An unrelated future bit on PIPE must not be read as this one.
    #[test]
    fn an_unrelated_future_pipe_bit_does_not_imply_stream() {
        let future = CapabilitySet::new().with(Capability::with_features(PIPE, 1 << 4));
        let negotiated = our_caps().intersect(&future);
        assert!(!peerbeam_transfer::caps_support_stream(&negotiated));
    }

    /// A trust store that trusts everyone, so the GUI assertion above can only
    /// be failing on the listen leg.
    struct AlwaysTrusts;
    impl peerbeam_domain::port::TrustStore for AlwaysTrusts {
        fn record(
            &self,
            _r: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            _d: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            Ok(None)
        }
        fn is_trusted(&self, _d: &DeviceId) -> bool {
            true
        }
    }

    /// A trust store that trusts nobody, for the gate assertions above.
    struct NeverTrusts;
    impl peerbeam_domain::port::TrustStore for NeverTrusts {
        fn record(
            &self,
            _r: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            _d: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            Ok(None)
        }
        fn is_trusted(&self, _d: &DeviceId) -> bool {
            false
        }
    }

    /// A peer with no CHAT capability at all is unsupported, not a panic — the
    /// intersection simply drops the channel.
    #[test]
    fn a_peer_without_chat_at_all_is_unsupported() {
        let transfer_only = CapabilitySet::new().with(Capability::new(TRANSFER));
        let negotiated = our_caps().intersect(&transfer_only);
        assert!(!negotiated.supports(CHAT));
        assert!(!caps_support_file_ref(&negotiated));
    }

    /// Unknown future feature bits from a newer peer must not be mistaken for
    /// this one: only bit 0 answers `supports_file_ref`.
    #[test]
    fn an_unrelated_future_feature_bit_does_not_imply_file_ref() {
        let future_peer = CapabilitySet::new().with(Capability::with_features(CHAT, 1 << 3));
        let negotiated = our_caps().intersect(&future_peer);
        assert!(!caps_support_file_ref(&negotiated));
    }
}
