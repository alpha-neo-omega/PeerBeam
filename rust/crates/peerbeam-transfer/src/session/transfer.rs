//! File transfer as a PeerSession channel — the transfer transport.
//!
//! Every transfer runs on its **own** transfer-type stream channel,
//! cryptographically isolated from control and from every other channel (its own
//! derived key, see [`super::crypto`]). The transfer itself reuses the transfer
//! engine unchanged — [`send_file`]/[`receive_file`] run over the channel's
//! sealed stream, sealed by the same scheme as [`crate::SecureLink`].
//!
//! It runs entirely **caller-side**: the session pump hands over the sealed
//! stream (opener via [`SessionHandle::open_stream_channel`], accepter via the
//! session's incoming-streams receiver) and is never in the transfer's data
//! path. A transfer that fails, is cancelled, or panics therefore cannot stall
//! or tear down the session or any sibling channel — the pump keeps servicing
//! control traffic throughout.
//!
//! A caller enables transfer channels by advertising [`ChannelType::TRANSFER`]
//! as a stream capability ([`SessionConfig::with_stream_channel_type`]) and using
//! the helpers here.
//!
//! [`PeerSession`]: super::PeerSession
//! [`SessionHandle::open_stream_channel`]: super::SessionHandle::open_stream_channel
//! [`SessionConfig::with_stream_channel_type`]: super::SessionConfig::with_stream_channel_type

use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{FrameKind, StorageProvider};
use peerbeam_domain::session::{ChannelType, SessionError};

use super::channel::IncomingStreamChannel;
use super::SessionHandle;
use crate::control::TransferControl;
use crate::folder::{receive_folder, send_folder, FolderReceived, FolderSendRequest};
use crate::peek::PeekLink;
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

/// Open a dedicated transfer channel on `session` and send a **folder** over it,
/// reusing the folder transfer engine unchanged (same channel mechanism as
/// [`send_file_on_session`]).
pub async fn send_folder_on_session(
    session: &SessionHandle,
    storage: &dyn StorageProvider,
    req: FolderSendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
) -> Result<TransferOutcome> {
    let (channel, mut link) = session
        .open_stream_channel(ChannelType::TRANSFER)
        .await
        .map_err(sess_to_dom)?;
    let outcome = send_folder(link.as_mut(), storage, req, ctrl, progress, retries).await;
    session.close_channel(channel);
    outcome
}

/// Receive a **folder** over an accepted incoming transfer channel, reusing the
/// folder transfer engine unchanged.
pub async fn receive_folder_on_channel(
    incoming: IncomingStreamChannel,
    session: &SessionHandle,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
) -> Result<FolderReceived> {
    let IncomingStreamChannel {
        channel, mut link, ..
    } = incoming;
    let received = receive_folder(link.as_mut(), storage, dest_dir, ctrl, progress).await;
    session.close_channel(channel);
    received
}

/// What a channel receive produced — a single file or a folder.
#[derive(Debug)]
pub enum ChannelReceived {
    /// A single file arrived.
    File(Received),
    /// A folder arrived.
    Folder(FolderReceived),
}

/// Receive a file **or** a folder over an accepted incoming transfer channel,
/// dispatching by peeking the first frame (a folder opens with a `Control`
/// manifest frame; a file opens with a `Meta` frame) — the same discriminator the
/// direct receive path used, now over a per-channel sealed stream. Reuses
/// [`receive_file`] / [`receive_folder`] unchanged.
pub async fn receive_on_channel(
    incoming: IncomingStreamChannel,
    session: &SessionHandle,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
) -> Result<ChannelReceived> {
    let IncomingStreamChannel {
        channel, mut link, ..
    } = incoming;
    let result = async {
        let first = link
            .recv_frame()
            .await?
            .ok_or_else(|| DomainError::Connection("channel closed before data".into()))?;
        let is_folder = first.kind == FrameKind::Control;
        let mut peek = PeekLink::new(first, link.as_mut());
        if is_folder {
            receive_folder(&mut peek, storage, dest_dir, ctrl, progress)
                .await
                .map(ChannelReceived::Folder)
        } else {
            receive_file(&mut peek, storage, dest_dir, ctrl, progress)
                .await
                .map(ChannelReceived::File)
        }
    }
    .await;
    session.close_channel(channel);
    result
}
