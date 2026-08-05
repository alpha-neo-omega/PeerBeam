//! PeerSession establishment for the FFI transfer manager (production path).
//!
//! Mirrors the CLI's session helper: the sender dials a multiplexed channel
//! connection and opens an initiator session; the receiver accepts a responder
//! session on an inbound channel connection. The transfer itself runs over the
//! session's transfer channel via the existing engine helpers — no transfer
//! logic lives here.

use std::sync::Arc;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use peerbeam_domain::entity::{Device, TransferSession};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType};
use peerbeam_engine::RouteManager;
use peerbeam_transfer::{
    Identity, IncomingStreamChannel, PeerSession, SessionConfig, SessionHandle, SessionRole,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};

use crate::error::{from_domain, Code};

const TRANSFER: ChannelType = ChannelType::TRANSFER;

fn transfer_cfg() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(TRANSFER)))
        .with_stream_channel_type(TRANSFER)
}

/// A live PeerSession with its pump running. Holds the incoming-channel receiver
/// so the receiving side can await the peer's transfer channel.
pub struct Session {
    /// Control handle for opening channels / closing.
    pub handle: SessionHandle,
    /// The authenticated peer's device id.
    pub peer_id: String,
    /// The peer's presented human name (may be empty).
    pub peer_name: String,
    /// The authenticated peer's device id, typed (for trust lookups).
    pub peer_device: peerbeam_domain::id::DeviceId,
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

async fn establish(
    transport: Arc<dyn ChannelTransport>,
    role: SessionRole,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
) -> Result<Session, (Code, String)> {
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
    .map_err(|e| (Code::Connection, format!("session establish failed: {e}")))?;
    let peer_device = ps.peer().clone();
    let peer_id = peer_device.0.clone();
    let peer_name = ps.peer_name().to_string();
    let handle = ps.handle();
    let run = crate::runtime::spawn_handle(async move {
        let _ = ps.run().await;
    });
    Ok(Session {
        handle,
        peer_id,
        peer_name,
        peer_device,
        incoming,
        run,
    })
}

/// Dial `device` and establish an initiator PeerSession over the RouteManager's
/// best available route (one attempt across ranked candidates).
pub async fn dial(
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &Device,
    meta: &TransferSession,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
) -> Result<Session, (Code, String)> {
    let candidates = routes.candidates(device);
    if candidates.is_empty() {
        return Err((Code::Connection, format!("no routes to {}", device.name)));
    }
    let mut last: Option<(Code, String)> = None;
    for route in candidates {
        match quic.dial_channels(&route, meta).await {
            Ok(qc) => {
                let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
                match establish(
                    transport,
                    SessionRole::Initiator,
                    ident.clone(),
                    enc.clone(),
                    trust.clone(),
                )
                .await
                {
                    Ok(session) => return Ok(session),
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
pub async fn accept(
    qc: QuicChannels,
    ident: Identity,
    enc: Arc<dyn EncryptionProvider>,
    trust: Arc<dyn TrustStore>,
) -> Result<Session, (Code, String)> {
    let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
    establish(transport, SessionRole::Responder, ident, enc, trust).await
}
