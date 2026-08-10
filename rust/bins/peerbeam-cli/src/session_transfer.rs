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
//! surface inbound Chat frames. Today only `chat watch` does this; `send`,
//! `chat send`, and `receive`/`daemon` (`serve_loop`) all pass `None` — CHAT is
//! still advertised on every session (so a peer's `chat send` is accepted
//! regardless), but nothing handles it on those paths, since none of them
//! currently surface inbound chat.
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
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, MessageHandler};
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
/// serve the Chat channel; absent, CHAT is still advertised but nothing
/// handles it (the sending side, which never receives a Chat frame).
fn session_cfg(chat_handler: Option<Arc<dyn MessageHandler>>) -> SessionConfig {
    let caps = CapabilitySet::new()
        .with(Capability::new(TRANSFER))
        .with(Capability::new(CHAT));
    let mut cfg = SessionConfig::new(caps).with_stream_channel_type(TRANSFER);
    if let Some(h) = chat_handler {
        cfg = cfg.with_handlers(HandlerRegistry::new().with(h));
    }
    cfg
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
    incoming: UnboundedReceiver<IncomingStreamChannel>,
    run: tokio::task::JoinHandle<()>,
}

impl Session {
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
    let handle = ps.handle();
    let run = tokio::spawn(async move {
        let _ = ps.run().await;
    });
    Ok(Session {
        handle,
        peer_id,
        newly_trusted,
        pairing_code,
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
