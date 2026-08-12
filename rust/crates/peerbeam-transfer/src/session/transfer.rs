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

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};
use peerbeam_domain::session::{ChannelType, SessionError};

use super::channel::IncomingStreamChannel;
use super::SessionHandle;
use crate::control::TransferControl;
use crate::folder::{
    manifest_preview, receive_folder, send_folder, FolderReceived, FolderSendRequest,
};
use crate::peek::{OwnedPeekLink, PeekLink};
use crate::protocol::parse_meta;
use crate::stream::{
    receive_file, sanitize_file_name, send_file, Received, SendRequest, TransferOutcome,
};

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

/// What the first frame of an incoming transfer channel says about the
/// transfer, learned *without consuming* that frame — see
/// [`peek_incoming_meta`].
///
/// All four fields come straight off the wire and are therefore
/// **peer-controlled**. [`name`](Self::name) is already reduced to the exact
/// single path component the receive path would write, so it cannot render as
/// a path; [`transfer_id`](Self::transfer_id) is *not* interpreted here at all
/// and a caller that uses it as a registry key must satisfy itself that a peer
/// cannot collide with an existing entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferPreview {
    /// The sender's own transfer id, or empty when nothing could be decoded.
    /// Equal to the `transfer_id` the completed receive later reports on
    /// [`Received`]/[`FolderReceived`].
    pub transfer_id: String,
    /// The sanitized display name — the file's base name, or a folder's root
    /// name. Empty when nothing could be decoded.
    pub name: String,
    /// Total bytes: the file's size, or the sum of the folder manifest's file
    /// sizes. `0` when unknown.
    pub size: u64,
    /// Whether the first frame was a folder manifest rather than a file meta.
    pub is_folder: bool,
}

/// How long [`peek_incoming_meta`] waits for the first frame before giving up
/// and reporting that it learned nothing.
///
/// A channel is only delivered to the receiver once the sender has performed
/// its first application write on the stream (`open_stream_channel` sends no
/// probe frame), so in practice the opening frame is already in flight when
/// the peek starts and this bound is never approached. It exists solely so a
/// peer that opens a transfer channel and then writes nothing — or stalls
/// half-way through the first frame — cannot park the caller *before* it has
/// registered the transfer, where there would be no id to cancel and no prompt
/// to time out.
///
/// Timing out is harmless by construction: the peek is advisory, so the caller
/// simply falls back to whatever it did before this existed. Erring long
/// therefore only ever costs a stalled peer some patience, while erring short
/// costs a slow-but-legitimate peer its correlation — hence the generous
/// value, still well inside the caller's own approval timeout.
///
/// Cancelling the read is safe: `QuicLink::recv_frame` keeps all partial
/// progress in its own resumable state (documented cancellation-safety), so a
/// timed-out read leaves the stream exactly where it was rather than desynced.
const PEEK_GRACE: Duration = Duration::from_secs(30);

/// Read the first frame of an accepted incoming transfer channel and decode
/// what it says — the sender's transfer id, the display name and the size —
/// **without consuming it**.
///
/// The returned channel replays that frame on its very next `recv_frame`, so
/// handing it straight to [`receive_on_channel`] behaves exactly as if this
/// had never been called: the replay is the same mechanism `receive_on_channel`
/// already uses internally for its own file-vs-folder dispatch, in its owned
/// form ([`OwnedPeekLink`]) so the link can be given back.
///
/// **Infallible on purpose.** There is no error path a caller could usefully
/// act on: the channel must come back either way (a `Result` would have to
/// consume it to report failure), and the transfer must proceed either way.
/// A closed, empty, malformed, undecodable or merely slow first frame yields a
/// default [`TransferPreview`] — empty id, empty name, zero size — which the
/// caller reads as "learned nothing" and falls back on.
pub async fn peek_incoming_meta(
    incoming: IncomingStreamChannel,
) -> (IncomingStreamChannel, TransferPreview) {
    let IncomingStreamChannel {
        channel,
        channel_type,
        mut link,
    } = incoming;
    // Exactly ONE frame, decoded directly rather than via `recv_meta`/
    // `recv_manifest`: those loop until they find the frame they want, which
    // would consume frames this peek cannot put back (only the one it holds is
    // replayable).
    let first = match tokio::time::timeout(PEEK_GRACE, link.recv_frame()).await {
        Ok(Ok(Some(frame))) => frame,
        // Timed out, link error, or clean close before any data: nothing was
        // read, so the channel is handed back untouched.
        _ => {
            return (
                IncomingStreamChannel {
                    channel,
                    channel_type,
                    link,
                },
                TransferPreview::default(),
            )
        }
    };
    let preview = preview_of(&first);
    let link: Box<dyn Link> = Box::new(OwnedPeekLink::new(first, link));
    (
        IncomingStreamChannel {
            channel,
            channel_type,
            link,
        },
        preview,
    )
}

/// Decode one opening frame into a [`TransferPreview`], using the same
/// file-vs-folder discriminator as [`receive_on_channel`] (a folder opens with
/// a `Control` manifest; a file opens with a `Meta`). Anything else — an
/// unexpected frame kind, malformed JSON, a control frame that is not a
/// manifest — is "learned nothing", never an error.
fn preview_of(frame: &Frame) -> TransferPreview {
    match frame.kind {
        FrameKind::Control => match manifest_preview(frame) {
            Some((transfer_id, root, size)) => TransferPreview {
                transfer_id,
                name: root,
                size,
                is_folder: true,
            },
            None => TransferPreview::default(),
        },
        FrameKind::Meta => match parse_meta(frame) {
            Ok(meta) => TransferPreview {
                transfer_id: meta.transfer_id,
                name: sanitize_file_name(&meta.name),
                size: meta.size,
                is_folder: false,
            },
            Err(_) => TransferPreview::default(),
        },
        _ => TransferPreview::default(),
    }
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
