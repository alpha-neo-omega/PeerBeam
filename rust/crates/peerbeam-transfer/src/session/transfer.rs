//! File transfer as a PeerSession channel (M5).
//!
//! This is the first real capability carried over [`PeerSession`]: each transfer
//! runs on its **own** transfer-type stream channel, cryptographically isolated
//! from control and from every other channel (its own derived key, see
//! [`super::crypto`]). The transfer itself reuses the existing engine unchanged —
//! [`send_file`]/[`receive_file`] run over the channel's sealed stream exactly as
//! they run over a [`crate::SecureLink`].
//!
//! It runs entirely **caller-side**: the session pump hands over the sealed
//! stream (opener via [`SessionHandle::open_stream_channel`], accepter via the
//! session's incoming-streams receiver) and is never in the transfer's data
//! path. A transfer that fails, is cancelled, or panics therefore cannot stall
//! or tear down the session or any sibling channel — the pump keeps servicing
//! control traffic throughout.
//!
//! Legacy transfer ([`send_file`]/[`receive_file`] over a directly-dialed
//! [`crate::SecureLink`]) is untouched and remains the default; a caller opts
//! into session transfer by advertising [`ChannelType::TRANSFER`] as a stream
//! capability ([`SessionConfig::with_stream_channel_type`]) and using the
//! helpers here.
//!
//! [`PeerSession`]: super::PeerSession
//! [`SessionHandle::open_stream_channel`]: super::SessionHandle::open_stream_channel
//! [`SessionConfig::with_stream_channel_type`]: super::SessionConfig::with_stream_channel_type

use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::StorageProvider;
use peerbeam_domain::session::{ChannelType, SessionError};

use super::channel::IncomingStreamChannel;
use super::SessionHandle;
use crate::control::TransferControl;
use crate::stream::{receive_file, send_file, Received, SendRequest, TransferOutcome};

fn sess_to_dom(e: SessionError) -> DomainError {
    DomainError::Connection(e.to_string())
}

/// Open a dedicated transfer channel on `session` and send `req` over it,
/// reusing the file transfer engine unchanged.
///
/// The channel's stream is sealed with its own per-channel key; the transfer
/// runs caller-side, so its failure never touches the session pump. The channel
/// is closed (best-effort) when the transfer ends, whatever the outcome.
pub async fn send_file_on_session(
    session: &SessionHandle,
    storage: &dyn StorageProvider,
    req: SendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
) -> Result<TransferOutcome> {
    let (channel, mut link) = session
        .open_stream_channel(ChannelType::TRANSFER)
        .await
        .map_err(sess_to_dom)?;
    let outcome = send_file(link.as_mut(), storage, req, ctrl, progress, retries).await;
    session.close_channel(channel);
    outcome
}

/// Receive a file over an accepted incoming transfer channel, reusing the file
/// transfer engine unchanged.
///
/// `incoming` is the [`IncomingStreamChannel`] delivered by the session's
/// incoming-streams receiver for a peer-opened [`ChannelType::TRANSFER`]
/// channel. The channel is closed (best-effort) when the transfer ends.
pub async fn receive_file_on_channel(
    incoming: IncomingStreamChannel,
    session: &SessionHandle,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
) -> Result<Received> {
    let IncomingStreamChannel {
        channel, mut link, ..
    } = incoming;
    let received = receive_file(link.as_mut(), storage, dest_dir, ctrl, progress).await;
    session.close_channel(channel);
    received
}
