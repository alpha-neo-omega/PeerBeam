//! Production transfer execution over PeerSession.
//!
//! Every CLI file/folder transfer runs as a PeerSession transfer channel — the
//! sender dials a multiplexed connection and opens a channel
//! ([`send_file_on_session`]/[`send_folder_on_session`]); the receiver accepts the
//! session and dispatches the incoming channel with [`receive_on_channel`]. There
//! is no CLI-side legacy (direct-on-`Link`) execution path.
//!
//! Every session also advertises the Chat capability (`CHAT`) alongside
//! Transfer, so a `chat send` from either side is accepted regardless of what
//! the session was originally established for. When the caller passes a
//! `(ChatStore, ReceivedSink)` pair, a `ChatHandler` is registered to persist +
//! surface inbound Chat frames. Every CLI call site now passes `Some(...)` —
//! plain `send` (`secure_send_file`/`secure_send_folder`), `chat send`'s
//! opportunistic dial, the periodic drain tick (`chat::drain_tick`), and
//! `receive`/`daemon` (`serve_loop`)/`chat watch`'s accept — because a peer
//! can push a queued chat message back over *any* live session regardless of
//! what it was dialed/accepted for, and a side with no `ChatHandler`
//! registered doesn't error on an inbound CHAT frame, it silently drops it
//! (decoded, counted in stats, never dispatched) — see `crate::chat`'s module
//! doc for the bug class this avoids.
//!
//! This module only *establishes* the session and hands back its handle; the
//! transfer itself reuses the engine (`send_file`/`receive_file` via the channel
//! helpers) unchanged — no duplicated transfer logic.

use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use peerbeam_chat::{ChatHandler, ChatStore, ReceivedSink};
use peerbeam_clipboard::ClipboardHandler;
use peerbeam_domain::entity::{Device, Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, CHAT_FEAT_FILEDECLINE,
    CHAT_FEAT_FILEREF, CHAT_FEAT_REACTION, CLIPBOARD_FEAT_CLIP, PIPE_FEAT_STREAM,
    PRESENCE_FEAT_STATUS,
};
use peerbeam_engine::RouteManager;
use peerbeam_presence::{PresenceHandler, PresenceSender, HEARTBEAT_INTERVAL};
use peerbeam_transfer::{
    HandlerRegistry, Identity, IncomingStreamChannel, PeerSession, SessionConfig, SessionHandle,
    SessionRole,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};

use crate::exit::CliError;

const TRANSFER: ChannelType = ChannelType::TRANSFER;
const CHAT: ChannelType = ChannelType::CHAT;
const CLIPBOARD: ChannelType = ChannelType::CLIPBOARD;
const PRESENCE: ChannelType = ChannelType::PRESENCE;
const PIPE: ChannelType = ChannelType::PIPE;

/// The session config every CLI PeerSession uses: advertise + accept the
/// Transfer capability as the stream channel, and advertise the Chat
/// capability alongside it. `chat_handler`, when present, is registered to
/// serve the Chat channel. Every call site in this file passes `Some(...)` —
/// on every dial AND every accept — so a message pushed from either side of
/// an established session is always received; see this module's top doc
/// comment for why that symmetry is a correctness requirement, not a nicety.
/// A future dial/accept call site that omits it would silently drop any CHAT
/// frame pushed to it instead of erroring.
///
/// CHAT additionally advertises [`CHAT_FEAT_FILEREF`] (this build understands
/// the `FileRef` message and can correlate it with a transfer) and
/// [`CHAT_FEAT_FILEDECLINE`] (this build's `ChatHandler` settles an outgoing
/// file row on a peer's `FileDecline`), mirroring the FFI's
/// `session_exec::session_cfg` — both frontends must advertise the same set or
/// a peer's behaviour would depend on which of ours it happens to be talking
/// to. Advertising a feature bit is not a wire change — `Capability.features`
/// is already on the wire and `CapabilitySet::intersect` ANDs it — so a peer
/// from before either feature simply advertises `0`, the intersection clears
/// the bit, and [`Session::supports_file_ref`] reports false for it.
///
/// PRESENCE advertises [`PRESENCE_FEAT_STATUS`] on the same terms, and again
/// identically to the FFI: 2a shipped with only one surface advertising
/// `CHAT_FEAT_FILEREF`, so a peer's behaviour depended on which of our own
/// frontends it reached. Each surface has its own test asserting its own
/// advertisement, which is what makes that impossible to repeat.
///
/// Advertising the presence bit says nothing about whether this build *sends*
/// a status — that is the opt-in setting's business, and it is off by default.
/// The bit asserts comprehension, so a device sharing nothing still advertises
/// it truthfully and still displays everyone else's status.
///
/// CLIPBOARD advertises [`CLIPBOARD_FEAT_CLIP`] on those same terms, and the
/// gap between comprehension and behaviour is widest here: the CLI never
/// *sends* a clip at all. Auto-sync needs a clipboard watcher, watching needs a
/// system-clipboard adapter this workspace does not have, and so the watcher
/// lives in the Flutter surface (`docs/CLI.md`); `send --clipboard` is
/// unchanged. The bit is still advertised truthfully because this build does
/// understand an inbound `Clip` and acknowledges it (`crate::clipboard`) —
/// and because a peer must not behave differently depending on which of our
/// own two frontends it reached.
///
/// PIPE advertises [`PIPE_FEAT_STREAM`] on those same terms and is registered
/// as a **stream** channel type alongside TRANSFER, on **every** session this
/// process builds — including `receive`, `daemon start` and `chat watch`, none
/// of which will accept a pipe. That is deliberate and is what makes the
/// refusal a refusal: registering the type routes an inbound pipe to the
/// caller's incoming-streams receiver, where it is dispatched on
/// `channel_type` and closed with a reason. Leaving it unregistered would
/// instead pair it as a message channel with no handler, and its frames would
/// be decoded, counted and dropped in silence — a hang for the sender, not a
/// refusal. Advertising uniformly is what stops a peer's behaviour depending
/// on which of our frontends (or which of our commands) it reached.
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
        .with(Capability::new(TRANSFER))
        .with(Capability::with_features(
            CHAT,
            CHAT_FEAT_FILEREF | CHAT_FEAT_FILEDECLINE | CHAT_FEAT_REACTION,
        ))
        .with(Capability::with_features(CLIPBOARD, CLIPBOARD_FEAT_CLIP))
        .with(Capability::with_features(PRESENCE, PRESENCE_FEAT_STATUS))
        .with(Capability::with_features(PIPE, PIPE_FEAT_STREAM))
}

/// Whether `caps` — an **already-negotiated** (intersected) set — carries the
/// chat `FileRef` feature. Split out of [`Session::supports_file_ref`] so the
/// decision is unit-testable without a live session (mirrors the FFI's
/// `session_exec::caps_support_file_ref`; kept as an independent copy rather
/// than shared, since the two frontends have no common crate to host one in).
fn caps_support_file_ref(caps: &CapabilitySet) -> bool {
    caps.features(CHAT)
        .is_some_and(|f| f & CHAT_FEAT_FILEREF != 0)
}

/// Transfer-session metadata used to dial (routing/telemetry only).
fn dial_meta(device: &Device, id: &str) -> TransferSession {
    TransferSession {
        id: TransferId::from(id),
        peer: device.id.clone(),
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

/// A live PeerSession with its pump running. Holds the incoming-channel receiver
/// so the receiving side can await the peer's transfer channel.
pub struct Session {
    /// Control handle for opening channels / closing.
    pub handle: SessionHandle,
    /// The authenticated peer id.
    pub peer_id: String,
    /// Whether the peer was newly TOFU-pinned during this handshake.
    pub newly_trusted: bool,
    /// The first-contact pairing code from this session's handshake (empty
    /// for a resumed session — there is no handshake to derive it from).
    pub pairing_code: String,
    /// The capabilities both sides agreed on (already intersected). Read via
    /// [`supports_file_ref`](Self::supports_file_ref). Captured here — rather
    /// than fetched on demand — because the negotiated value has to be read
    /// before the `PeerSession` is consumed by the run loop.
    capabilities: CapabilitySet,
    incoming: UnboundedReceiver<IncomingStreamChannel>,
    run: tokio::task::JoinHandle<()>,
    /// This session's presence heartbeat, if presence was configured. Held so
    /// `close` can stop it rather than wait out the next tick's liveness probe.
    presence: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    /// Whether the peer negotiated the chat `FileRef` feature. A peer from
    /// before this feature advertises `features: 0`, so this is false and
    /// `chat send --file` must refuse rather than silently sending the bytes
    /// with no way for the peer to place them in a conversation.
    #[must_use]
    pub fn supports_file_ref(&self) -> bool {
        caps_support_file_ref(&self.capabilities)
    }

    /// Whether the peer negotiated the chat `Reaction` feature. A peer from
    /// before reactions advertises `features: 0`, so this is false and the
    /// gesture is kept local rather than reported as delivered to a screen
    /// that never showed it.
    #[must_use]
    pub fn supports_reaction(&self) -> bool {
        self.capabilities
            .features(ChannelType::CHAT)
            .is_some_and(|f| f & CHAT_FEAT_REACTION != 0)
    }

    /// Whether the peer negotiated the pipe stream capability. A peer from
    /// before `peerbeam pipe` advertises no PIPE at all, so this is false and
    /// `pipe --to` must refuse **before reading stdin** rather than streaming
    /// bytes into a channel that peer would reject.
    #[must_use]
    pub fn supports_pipe(&self) -> bool {
        peerbeam_transfer::caps_support_stream(&self.capabilities)
    }

    /// The negotiated (already intersected) capability set — the set a gate
    /// must be asked about, since it reflects what the *peer* advertised and
    /// not merely what this build asked for.
    #[must_use]
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// Await the next incoming transfer channel the peer opens (receiver side).
    pub async fn next_incoming(&mut self) -> Option<IncomingStreamChannel> {
        self.incoming.recv().await
    }

    /// Close the session and wait for its pump to finish.
    pub async fn close(self) {
        if let Some(p) = self.presence {
            p.abort();
        }
        self.handle.close();
        let _ = self.run.await;
    }
}

/// Build a running `Session` over an established channel transport. When
/// `chat` is `Some`, a `ChatHandler` is built over its `(ChatStore,
/// ReceivedSink)` and registered on the session; the handler's peer slot is
/// bound to the authenticated peer right after `PeerSession::open` returns and
/// before the run loop is spawned, so no Chat frame can ever be dispatched to
/// an unbound handler.
async fn establish(
    transport: Arc<dyn ChannelTransport>,
    role: SessionRole,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
    chat: Option<(ChatStore, ReceivedSink)>,
    route: Option<peerbeam_domain::entity::RouteKind>,
) -> Result<Session, CliError> {
    let mut handlers: Vec<Arc<dyn MessageHandler>> = Vec::new();
    let mut peer_slot: Option<Arc<OnceLock<DeviceId>>> = None;
    if let Some((store, sink)) = chat {
        let (h, slot) = ChatHandler::new(store, sink);
        peer_slot = Some(slot);
        handlers.push(h as Arc<dyn MessageHandler>);
    }
    // Presence is wired on EVERY session, unconditionally — receiving a peer's
    // status is never gated, and an unregistered handler would drop it
    // silently rather than refuse it. Sending is a separate decision made per
    // heartbeat below.
    let (presence_handler, presence_slot) = PresenceHandler::new(
        crate::presence::registry().clone(),
        Arc::new(|_peer, _entry| {}),
    );
    handlers.push(presence_handler as Arc<dyn MessageHandler>);
    // Clipboard, for the same reason and on the same unconditional terms: this
    // build advertises `CLIPBOARD_FEAT_CLIP`, and an unregistered handler would
    // make that advertisement a lie — the dispatch loop drops an inbound frame
    // silently rather than refusing it, so the peer would believe it synced.
    // Receiving is never gated; the CLI simply has no system clipboard to apply
    // the clip to, and says so.
    let (clipboard_handler, clipboard_slot) = ClipboardHandler::new(crate::clipboard::sink());
    handlers.push(clipboard_handler as Arc<dyn MessageHandler>);

    // Event/channel-event sinks are unused by the CLI (diagnostics read the
    // engine registry, not these); their receivers drop and emits are ignored.
    let (ev, _ev) = unbounded_channel();
    let (ch, _ch) = unbounded_channel();
    let (inc, incoming) = unbounded_channel();
    let trust_for_presence = trust.clone();
    let mut ps = PeerSession::open(
        transport,
        role,
        session_cfg(handlers),
        ev,
        ch,
        inc,
        None,
        ident,
        enc,
        trust,
    )
    .await
    .map_err(|e| CliError::Other(format!("session establish failed: {e}")))?;
    // Bind the chat peer before the run loop dispatches any frame.
    if let Some(slot) = peer_slot {
        let _ = slot.set(ps.peer().clone());
    }
    let _ = presence_slot.set(ps.peer().clone());
    let _ = clipboard_slot.set(ps.peer().clone());
    let peer_device = ps.peer().clone();
    let peer_id = ps.peer().0.clone();
    let newly_trusted = ps.newly_trusted();
    let pairing_code = ps.pairing_code().to_string();
    // Read alongside the other post-handshake fields, before `ps` moves into
    // the run closure below — this is the negotiated (intersected) set, so it
    // already reflects what the *peer* advertised, not just what we asked for.
    let capabilities = ps.capabilities().clone();
    let handle = ps.handle();
    let run = tokio::spawn(async move {
        let _ = ps.run().await;
    });
    // Heartbeat this device's own status, if configured. The task decides on
    // every beat whether anything may go out — `may_share_status` re-reads the
    // opt-in setting and the trust store — so spawning it for a peer we do not
    // share with opens no channel and sends no frame.
    let presence = crate::presence::sharing().map(|sh| {
        let dir = sh.save_dir.clone();
        let sender = PresenceSender::new(
            handle.clone(),
            peer_device,
            capabilities.clone(),
            trust_for_presence,
            Arc::new(crate::presence::enabled),
            Arc::new(move || peerbeam_presence::collect(&dir, route, env!("CARGO_PKG_VERSION"))),
        );
        tokio::spawn(sender.run(HEARTBEAT_INTERVAL))
    });
    Ok(Session {
        handle,
        peer_id,
        newly_trusted,
        pairing_code,
        capabilities,
        incoming,
        run,
        presence,
    })
}

/// Dial `device` and establish an **initiator** PeerSession, trying routes in the
/// RouteManager's priority order (LAN → … → relay) until one connects. `chat`
/// is cloned per route attempt so a retry across candidates doesn't consume it.
#[allow(clippy::too_many_arguments)]
pub async fn dial(
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &Device,
    id: &str,
    sc_ident: &Identity,
    sc_enc: &Arc<peerbeam_crypto::AeadCrypto>,
    sc_trust: &Arc<peerbeam_trust_fs::FsTrust>,
    chat: Option<(ChatStore, ReceivedSink)>,
) -> Result<Session, CliError> {
    let meta = dial_meta(device, id);
    let candidates = routes.candidates(device);
    if candidates.is_empty() {
        return Err(CliError::NotFound(format!("no routes to {}", device.name)));
    }
    let mut last: Option<CliError> = None;
    for route in candidates {
        // The class route selection already assigned this candidate, reused
        // verbatim rather than reclassified.
        let kind = route.kind;
        match quic.dial_channels(&route, &meta).await {
            Ok(qc) => {
                let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
                match establish(
                    transport,
                    SessionRole::Initiator,
                    sc_ident.clone(),
                    sc_enc.clone(),
                    sc_trust.clone(),
                    chat.clone(),
                    Some(kind),
                )
                .await
                {
                    Ok(session) => return Ok(session),
                    Err(e) => last = Some(e),
                }
            }
            Err(e) => last = Some(CliError::from(e)),
        }
    }
    Err(last.unwrap_or_else(|| CliError::Other(format!("all routes to {} failed", device.name))))
}

/// Accept a **responder** PeerSession over an inbound channel connection.
///
/// `routes` classifies the inbound connection's remote address for presence
/// through the RouteManager's own classifier, so an accepted session reports
/// the same vocabulary a dialled one does instead of leaving `network` absent
/// on every session the peer initiated.
pub async fn accept(
    qc: QuicChannels,
    routes: &RouteManager,
    sc_ident: &Identity,
    sc_enc: &Arc<peerbeam_crypto::AeadCrypto>,
    sc_trust: &Arc<peerbeam_trust_fs::FsTrust>,
    chat: Option<(ChatStore, ReceivedSink)>,
) -> Result<Session, CliError> {
    let route = Some(routes.classify(&qc.remote().ip().to_string()));
    let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
    establish(
        transport,
        SessionRole::Responder,
        sc_ident.clone(),
        sc_enc.clone(),
        sc_trust.clone(),
        chat,
        route,
    )
    .await
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

    /// Both chat feature bits must be advertised by **this** frontend. The FFI
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

    /// The negotiation contract `chat send --file`'s refusal rests on:
    /// `intersect` ANDs the feature bits, so advertising `CHAT_FEAT_FILEREF`
    /// against a peer that advertises `features: 0` yields a *negotiated* set
    /// without the bit — and this build must therefore never offer that peer
    /// a file in chat.
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

    /// The CLIPBOARD bit must be advertised by **this** frontend, for exactly
    /// the reason spelled out on `the_presence_feature_bit_is_advertised`: the
    /// other surface has its own copy of this test, and a shared helper would
    /// go green for both the moment either stopped advertising.
    #[test]
    fn the_clipboard_feature_bit_is_advertised() {
        let caps = session_cfg(Vec::new()).capabilities;
        let f = caps
            .features(ChannelType::CLIPBOARD)
            .expect("CLIPBOARD advertised");
        assert!(f & CLIPBOARD_FEAT_CLIP != 0, "clipboard sync");
    }

    /// Advertising CLIPBOARD is a claim about comprehension, not behaviour —
    /// and here the gap is total: the CLI understands an inbound `Clip` and
    /// never sends one. Whether anything leaves is the opt-in setting's and the
    /// trust store's business, and this asserts the capability alone opens
    /// nothing.
    #[test]
    fn advertising_clipboard_does_not_imply_syncing() {
        let ours = session_cfg(Vec::new()).capabilities;
        let negotiated = ours.intersect(&ours);
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
        let negotiated = session_cfg(Vec::new()).capabilities.intersect(&legacy);
        assert!(!negotiated.supports(ChannelType::CLIPBOARD));
        assert!(!peerbeam_clipboard::caps_support_clip(&negotiated));
    }

    /// An unrelated future bit on CLIPBOARD must not be read as this one.
    #[test]
    fn an_unrelated_future_clipboard_bit_does_not_imply_clip() {
        let future = CapabilitySet::new().with(Capability::with_features(CLIPBOARD, 1 << 4));
        let negotiated = session_cfg(Vec::new()).capabilities.intersect(&future);
        assert!(!peerbeam_clipboard::caps_support_clip(&negotiated));
    }

    /// The PIPE bit must be advertised by **this** frontend, for the reason
    /// spelled out on `the_presence_feature_bit_is_advertised`: the other
    /// surface has its own copy of this test, and a shared helper would go
    /// green for both the moment either stopped advertising.
    #[test]
    fn the_pipe_feature_bit_is_advertised() {
        let f = our_caps().features(PIPE).expect("PIPE advertised");
        assert!(f & PIPE_FEAT_STREAM != 0, "byte pipes");
    }

    /// PIPE must be a **stream** channel type on every session this process
    /// builds, not only on a `pipe --listen` one.
    ///
    /// This is what makes a refusal a refusal. An unregistered stream type
    /// pairs an inbound pipe as a *message* channel with no handler, whose
    /// frames are decoded, counted and dropped in silence — the sender hangs
    /// instead of failing. Registered, the channel is delivered to the caller,
    /// which dispatches on its type and closes it with a reason.
    #[test]
    fn pipe_is_a_stream_channel_type_on_every_session() {
        let cfg = session_cfg(Vec::new());
        assert!(cfg.stream_channel_types.contains(&PIPE));
        assert!(
            cfg.stream_channel_types.contains(&TRANSFER),
            "and transfer still is"
        );
    }

    /// Advertising PIPE is a claim about comprehension, not about behaviour,
    /// and here the gap is at its widest: `receive`, `daemon start` and `chat
    /// watch` all advertise it and all refuse every pipe. The capability alone
    /// opens nothing — the listen gate is a separate, local input.
    #[test]
    fn advertising_pipe_does_not_imply_accepting_one() {
        let negotiated = our_caps().intersect(&our_caps());
        assert!(
            peerbeam_transfer::caps_support_stream(&negotiated),
            "two of our builds negotiate the capability"
        );
        assert!(
            !peerbeam_transfer::may_accept_pipe(
                false, // not `pipe --listen` — a daemon, say
                &AlwaysTrusts,
                &DeviceId::from("pb-bob"),
                None,
                &negotiated,
            ),
            "a non-listening process must refuse even a trusted, capable peer"
        );
    }

    /// A peer from before `peerbeam pipe` advertises no PIPE at all, so the
    /// intersection drops it and `pipe --to` refuses before reading stdin.
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

    /// A trust store that trusts everyone, so the assertion above can only be
    /// failing on the listen leg.
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

    /// A trust store that trusts nobody, for the gate assertion above.
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

    /// A peer with no CHAT capability at all is unsupported, not a panic —
    /// the intersection simply drops the channel.
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
