//! Production transfer execution over PeerSession.
//!
//! Every CLI file/folder transfer runs as a PeerSession transfer channel — the
//! sender dials a multiplexed connection and opens a channel
//! ([`send_file_on_session`]/[`send_folder_on_session`]); the receiver accepts the
//! session and dispatches the incoming channel with [`receive_on_channel`]. There
//! is no CLI-side legacy (direct-on-`Link`) execution path.
//!
//! This module only *establishes* the session and hands back its handle; the
//! transfer itself reuses the engine (`send_file`/`receive_file` via the channel
//! helpers) unchanged — no duplicated transfer logic.

use std::sync::Arc;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use peerbeam_domain::entity::{Device, Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::TransferId;
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType};
use peerbeam_engine::RouteManager;
use peerbeam_transfer::{
    Identity, IncomingStreamChannel, PeerSession, SessionConfig, SessionHandle, SessionRole,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};

use crate::exit::CliError;

const TRANSFER: ChannelType = ChannelType::TRANSFER;

/// The session config every CLI transfer uses: advertise + accept the Transfer
/// capability as a stream channel.
fn transfer_cfg() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(TRANSFER)))
        .with_stream_channel_type(TRANSFER)
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

/// Build a running `Session` over an established channel transport.
async fn establish(
    transport: Arc<dyn ChannelTransport>,
    role: SessionRole,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
) -> Result<Session, CliError> {
    // Event/channel-event sinks are unused by the CLI (diagnostics read the
    // engine registry, not these); their receivers drop and emits are ignored.
    let (ev, _ev) = unbounded_channel();
    let (ch, _ch) = unbounded_channel();
    let (inc, incoming) = unbounded_channel();
    let mut ps = PeerSession::open(
        transport,
        role,
        transfer_cfg(),
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
/// RouteManager's priority order (LAN → … → relay) until one connects.
pub async fn dial(
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &Device,
    id: &str,
    sc_ident: &Identity,
    sc_enc: &Arc<peerbeam_crypto::AeadCrypto>,
    sc_trust: &Arc<peerbeam_trust_fs::FsTrust>,
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
) -> Result<Session, CliError> {
    let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
    establish(
        transport,
        SessionRole::Responder,
        sc_ident.clone(),
        sc_enc.clone(),
        sc_trust.clone(),
    )
    .await
}
