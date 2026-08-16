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
use peerbeam_domain::entity::{Device, Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, CHAT_FEAT_FILEDECLINE,
    CHAT_FEAT_FILEREF,
};
use peerbeam_engine::RouteManager;
use peerbeam_transfer::{
    HandlerRegistry, Identity, IncomingStreamChannel, PeerSession, SessionConfig, SessionHandle,
    SessionRole,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};

use crate::exit::CliError;

const TRANSFER: ChannelType = ChannelType::TRANSFER;
const CHAT: ChannelType = ChannelType::CHAT;

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
fn session_cfg(chat_handler: Option<Arc<dyn MessageHandler>>) -> SessionConfig {
    let mut cfg = SessionConfig::new(advertised_caps()).with_stream_channel_type(TRANSFER);
    if let Some(h) = chat_handler {
        cfg = cfg.with_handlers(HandlerRegistry::new().with(h));
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
            CHAT_FEAT_FILEREF | CHAT_FEAT_FILEDECLINE,
        ))
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

    /// Await the next incoming transfer channel the peer opens (receiver side).
    pub async fn next_incoming(&mut self) -> Option<IncomingStreamChannel> {
        self.incoming.recv().await
    }

    /// Close the session and wait for its pump to finish.
    pub async fn close(self) {
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
) -> Result<Session, CliError> {
    let mut peer_slot: Option<Arc<OnceLock<DeviceId>>> = None;
    let chat_handler: Option<Arc<dyn MessageHandler>> = chat.map(|(store, sink)| {
        let (h, slot) = ChatHandler::new(store, sink);
        peer_slot = Some(slot);
        h as Arc<dyn MessageHandler>
    });

    // Event/channel-event sinks are unused by the CLI (diagnostics read the
    // engine registry, not these); their receivers drop and emits are ignored.
    let (ev, _ev) = unbounded_channel();
    let (ch, _ch) = unbounded_channel();
    let (inc, incoming) = unbounded_channel();
    let mut ps = PeerSession::open(
        transport,
        role,
        session_cfg(chat_handler),
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
    Ok(Session {
        handle,
        peer_id,
        newly_trusted,
        pairing_code,
        capabilities,
        incoming,
        run,
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
pub async fn accept(
    qc: QuicChannels,
    sc_ident: &Identity,
    sc_enc: &Arc<peerbeam_crypto::AeadCrypto>,
    sc_trust: &Arc<peerbeam_trust_fs::FsTrust>,
    chat: Option<(ChatStore, ReceivedSink)>,
) -> Result<Session, CliError> {
    let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
    establish(
        transport,
        SessionRole::Responder,
        sc_ident.clone(),
        sc_enc.clone(),
        sc_trust.clone(),
        chat,
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
        session_cfg(None).capabilities
    }

    /// Both chat feature bits must be advertised by **this** frontend. The FFI
    /// has the identical test: 2a shipped with only one of the two surfaces
    /// advertising `CHAT_FEAT_FILEREF`, so a peer's behaviour depended on which
    /// of our own frontends it reached. One test per surface is what makes that
    /// impossible to repeat.
    #[test]
    fn both_chat_feature_bits_are_advertised() {
        let caps = session_cfg(None).capabilities;
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
