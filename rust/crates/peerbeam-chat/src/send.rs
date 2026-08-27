//! Sending a chat message over an established session.

use std::path::Path;
use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelId, ChannelState, ChannelType};
use peerbeam_transfer::{SessionHandle, TransferControl};
use tokio::sync::mpsc::UnboundedSender;

use crate::message::{ChatError, ChatMessage, FileDecline, FileRef, Reaction, Receipt};
use crate::record::{ChatRecord, FileMeta, Kind, Status};
use crate::staging::{StagingLimits, StagingStore};
use crate::store::{ChatStore, OutboxEntry, StagedFile};

/// Send one already-built [`ChatMessage`] over a channel that has already
/// reached [`ChannelState::Open`] (factored out of [`send_message`] so
/// [`flush_to_session`] can reuse the same wire-send step for each queued
/// entry without re-minting a message or re-opening the channel).
/// Reuse this session's CHAT channel, opening one only when there is none.
///
/// **A channel per message is not safe to close after sending.**
/// `send_on_channel` resolves once the frame is queued to the channel's actor,
/// not once it is on the wire — and a `ChannelClose` travels on the session's
/// *control* stream while the message travels on the channel's own stream.
/// Nothing orders one against the other, so the peer could see the close first,
/// tear the channel down and discard a message the sender had already reported
/// as sent. Intermittent by nature, and worse the slower the machine.
///
/// So the channel is kept for the life of the session instead — which is also
/// what fixes the leak that the per-message close was introduced for, and what
/// presence and clipboard already do. It is bounded by one channel per session
/// rather than one per message, and the session closing takes it with it.
///
/// A channel this side opened earlier is reused only while it is still open; a
/// peer that closed it, or a reconnect that took it, simply means opening
/// another.
async fn chat_lane(handle: &SessionHandle) -> Result<ChannelId, SendError> {
    let existing = handle
        .channels()
        .await
        .map_err(|e| SendError::Session(e.to_string()))?
        .into_iter()
        .find(|c| c.channel_type == ChannelType::CHAT && c.state.is_open())
        .map(|c| c.id);
    if let Some(channel) = existing {
        return Ok(channel);
    }
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
    Ok(channel)
}

async fn send_on_open_channel(
    handle: &SessionHandle,
    channel: ChannelId,
    msg: &ChatMessage,
) -> Result<(), SendError> {
    let frame = msg.to_frame(channel)?;
    handle
        .send_on_channel(
            channel,
            ChatMessage::message_type(),
            frame.flags,
            frame.payload,
        )
        .await
        .map_err(|e| SendError::Session(e.to_string()))
}

/// Failure sending a chat message.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("chat session error: {0}")]
    Session(String),
}

/// How long to wait for the peer's `ChannelAccept` before giving up.
///
/// `SessionHandle::open_channel` resolves as soon as *we* have locally
/// allocated the channel (state `Opening`) and queued the open request on the
/// wire — it does not wait for the peer to accept. On the in-memory test
/// transport that round trip is effectively instantaneous, but over any real
/// transport (QUIC over LAN/WiFi/Tailscale/Internet) it is a nonzero network
/// round trip, so sending immediately races the peer's accept and can hard-fail
/// with `SessionError::Channel("channel not open")`.
const CHANNEL_OPEN_BUDGET: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the channel to reach `Open`.
const CHANNEL_OPEN_POLL: Duration = Duration::from_millis(10);

/// Send `body` to `peer` over an established session: open a Chat channel,
/// wait for it to actually reach `Open` (the peer's accept), send one
/// `Message` frame, and only then persist + return the sent record.
///
/// Every CHAT channel opened here is closed again before returning.
///
/// # Why closing, and not reuse
///
/// A channel per message that is never closed is bounded only by messages per
/// session: `ChannelManager` counts live channels against a limit, so a long
/// conversation eventually stops sending with nothing to show the user but a
/// failure that arrives at message 257. Presence and clipboard solve the same
/// problem by caching one channel per session, and sync now does too — but both
/// have a struct to cache it in, and these are free functions over a borrowed
/// handle with nowhere to keep it.
///
/// Closing costs one extra `ChannelClose`/`ChannelClosed` pair per message.
/// That is invisible here because chat sends are **user-paced**: a person
/// typing is orders of magnitude slower than the round trip, unlike a chunk
/// request loop where the same cost was worth avoiding.
/// Persisting happens strictly after a successful send: on any failure (the
/// channel never opens, or the send itself errors) no record is written, so
/// local history never shows a message as sent when it was not.
pub async fn send_message(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
) -> Result<ChatRecord, SendError> {
    send_reply(handle, store, peer, body, None).await
}

/// Send `body` to `peer` as an answer to `in_reply_to` — [`send_message`] with
/// the one extra field, and its whole implementation.
///
/// Additive rather than a parameter on [`send_message`] so every existing
/// caller (the FFI's `chat_send`, the CLI's `chat send`, the round-trip tests)
/// keeps compiling untouched; `None` is exactly the old call.
///
/// The reference is not checked against local history, on purpose. A parent
/// that has been deleted, or whose window has closed, is a normal state — see
/// [`crate::reply`] — and refusing to send because of it would let one deleted
/// message veto a reply the user has already written. What *is* checked is the
/// shape (`ChatMessage::replying`), because a malformed reference is a
/// malformed message rather than a missing one.
pub async fn send_reply(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
    in_reply_to: Option<&str>,
) -> Result<ChatRecord, SendError> {
    let msg = ChatMessage::replying(body, in_reply_to)?; // enforces MAX_BODY
    let channel = chat_lane(handle).await?;
    let sent = send_on_open_channel(handle, channel, &msg).await;
    // **Not closed here.** The lane is the session's, not this message's — see
    // `chat_lane`. Closing right after a send raced the message it had just
    // queued.
    sent?;
    let rec = ChatRecord::sent(peer, &msg);
    store.append(&rec)?;
    Ok(rec)
}

/// Deliver a peer's queued CHAT-channel entries over an established session.
///
/// **Text keeps its existing behaviour precisely:** one CHAT channel, FIFO,
/// stop at the first failure so nothing is dequeued that did not go, and the
/// returned ids mean "delivered" so the caller can emit `chat_status: sent` for
/// each. Two silent-message-loss bugs have already shipped through this path;
/// it is not the place for elegance.
///
/// What changed is that the loop now **branches on `entry.kind`** instead of
/// rebuilding every entry as a `ChatMessage` and sending it as `MSG_TEXT`:
///
/// * [`Kind::Text`] — exactly as before.
/// * [`Kind::Decline`] — goes out as a `FileDecline` (`MSG_FILE_DECLINE`), on
///   this same channel. Sent as text it would have arrived as an empty chat
///   message and settled nothing. It is dequeued on success but deliberately
///   **not** returned: its id is the *sender's* `FileRef` id, which on this side
///   names the inbound row we declined, so reporting it as "sent" would tell the
///   surface a file we refused had been delivered.
/// * [`Kind::File`] — skipped here. Its bytes ride the TRANSFER stream channel,
///   so a multi-GB upload can never block a message behind it, and the caller
///   (which owns the transfer engine) decides which one to start via
///   [`next_file_for`]. Skipping rather than stopping keeps FIFO for everything
///   behind it.
pub async fn flush_to_session(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
) -> Result<Vec<String>, SendError> {
    let entries = store.outbox_for(peer)?;
    // Only text and declines travel here; a queue holding nothing but files
    // must not open a CHAT channel it has no message for.
    if !entries
        .iter()
        .any(|e| matches!(e.kind, Kind::Text | Kind::Decline))
    {
        return Ok(Vec::new());
    }
    let channel = chat_lane(handle).await?;

    let mut flushed = Vec::new();
    for entry in entries {
        match entry.kind {
            Kind::Text => {
                let msg = ChatMessage {
                    id: entry.message_id.clone(),
                    timestamp: entry.timestamp.clone(),
                    body: entry.body.clone(),
                    // Carried, or a reply that waited for an offline peer would
                    // arrive as an ordinary message: the link would survive in
                    // local history and be silently dropped on the wire for
                    // exactly the messages that were queued.
                    in_reply_to: entry.in_reply_to.clone(),
                    // Same reason as the reply link directly above: a queued
                    // group message that flushed without its tag would arrive
                    // as an ordinary one-to-one message.
                    group: entry.group.clone(),
                };
                if send_on_open_channel(handle, channel, &msg).await.is_err() {
                    break; // peer went away mid-flush; remaining entries stay queued
                }
                store.record_sent(&entry)?;
                store.outbox_remove(&entry.message_id)?;
                flushed.push(entry.message_id);
            }
            Kind::Decline => {
                let d = FileDecline {
                    id: entry.message_id.clone(),
                    timestamp: entry.timestamp.clone(),
                };
                if send_decline_on_open_channel(handle, channel, &d)
                    .await
                    .is_err()
                {
                    break;
                }
                // No `record_sent`: this id names the INBOUND row we declined,
                // and flipping that to `Sent` would make the conversation claim
                // we delivered a file we refused.
                store.outbox_remove(&entry.message_id)?;
            }
            Kind::File => continue,
        }
    }
    // Not closed: this is the session's lane, shared with every other chat
    // send over it (see `chat_lane`), and closing it here would race whatever
    // was queued on it — the bug this batch's own channel was closed for.
    Ok(flushed)
}

/// The one file entry a drain wants sent next for `peer`, if any: the oldest
/// queued [`Kind::File`] entry (the outbox lists FIFO by its time-ordered key).
///
/// Split out of [`flush_to_session`] because the two halves need different
/// machinery. Text is a frame on a channel this crate already owns; a file is a
/// transfer, and `peerbeam-chat` has no transfer engine. So this function only
/// **decides**, and the caller — which does own one — performs the send. That
/// also makes the one-file-in-flight rule the caller's to enforce, in the same
/// place it tracks running transfers.
///
/// An entry whose `kind` says file but that carries no staged blob is skipped
/// and logged rather than returned: there is nothing to send, and returning it
/// would stall every file behind it forever.
///
/// An entry whose blob is **gone from disk** is skipped for the same reason.
/// The caller re-checks before offering anything (its own `open_read` would
/// fail otherwise), but a caller-side check alone is head-of-line blocking:
/// this function would hand back the same dead entry every drain tick and every
/// later file queued for that peer would sit behind it forever, with nothing
/// but a log line to show for it. That is reachable — `SECURITY.md` tells users
/// where the blob store lives and that it can hold gigabytes, so one clear-out
/// of `<data_directory>/outbox-blobs/` strands the oldest entry.
///
/// **Skipped, never dequeued.** `Path::exists` is also false for a transient
/// permissions or I/O error, and dropping the entry on that would throw away a
/// queued share whose bytes are fine. The entry stays queued and recovers by
/// itself if the blob comes back; what it loses is only its place at the head
/// of the line.
pub fn next_file_for(store: &ChatStore, peer: &DeviceId) -> Result<Option<PendingFile>, ChatError> {
    for entry in store.outbox_for(peer)? {
        if entry.kind != Kind::File {
            continue;
        }
        match entry.file.clone() {
            Some(file) if Path::new(&file.staged_path).exists() => {
                return Ok(Some(PendingFile { entry, file }))
            }
            Some(file) => {
                tracing::warn!(
                    message_id = %entry.message_id,
                    staged_path = %file.staged_path,
                    "skipping a queued file entry whose staged blob is not on disk"
                );
            }
            None => {
                tracing::warn!(
                    message_id = %entry.message_id,
                    "skipping a queued file entry with no staged blob"
                );
            }
        }
    }
    Ok(None)
}

/// The one file entry a flush wants sent next. The caller owns the transfer
/// engine, so it performs the send; [`next_file_for`] only decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    /// The queue entry, including its `offers_refused` backstop count.
    pub entry: OutboxEntry,
    /// The blob the outbox owns — what will actually be delivered, whatever has
    /// since happened to the file the user picked.
    pub file: StagedFile,
}

/// Validate a path, mint the [`FileRef`], and persist the outgoing row as
/// [`Status::Staging`] — the synchronous half of a file share, doing no I/O
/// beyond `metadata`.
///
/// Split from the copy ([`stage_file_send`]) because the two have opposite
/// timing requirements. The caller must be able to hand its own caller an id
/// *immediately* — the FFI's `chat_send_file` returns `{id}` synchronously and
/// its surface writes an optimistic bubble against it — while staging a
/// multi-GB file can take minutes. Doing both in one call would force either a
/// blocked UI or an id that does not exist yet.
///
/// The row is written **before** the copy starts so a long stage is visible as
/// *Staging* rather than looking like a hung attach, and it is written
/// `Staging` rather than `Transferring` because nothing has been offered to
/// anyone yet: no transfer exists, so no wire event can settle it, and
/// `Staging` is deliberately outside the settle guard's writable set.
///
/// A refused path (missing, a directory, an unusable name) leaves **no row**
/// and touches no network: the whole call fails before anything is persisted.
///
/// Rejects a directory outright — a folder share is a different wire shape
/// (`send_folder`) and is not part of file-in-chat.
pub fn begin_file_send(
    store: &ChatStore,
    peer: &DeviceId,
    path: &str,
) -> Result<FileRef, SendError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| SendError::Session(format!("cannot read {path}: {e}")))?;
    if meta.is_dir() {
        return Err(SendError::Session(
            "folders aren't supported in chat yet — use Send folder".into(),
        ));
    }
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| SendError::Session(format!("no file name in {path}")))?;
    let r = FileRef::new(&name, meta.len())?;
    store.append(&ChatRecord::file_out(
        peer,
        &r,
        FileMeta::new(&r.name, r.size, Some(path.to_string())),
        Status::Staging,
    ))?;
    Ok(r)
}

/// Copy the picked file into outbox-owned storage and queue it — the
/// asynchronous half of a file share, following [`begin_file_send`].
///
/// On success `r` describes the **staged blob**, not the source: `r.size` is
/// overwritten with the bytes actually copied. That is not tidiness. The peer's
/// approval prompt is rendered from this `FileRef` while the bytes it is
/// consenting to come from the blob's own `TransferMeta`, and the two are
/// correlated by id alone — so a source that grew during the copy (a log, a
/// download still running) would otherwise show one size in the prompt while a
/// different number of bytes arrived. Both must be derived from the blob.
///
/// Staging failure is immediate failure: nothing is queued, and the row is
/// marked `Failed` rather than left on `Staging` forever, so the user learns now
/// instead of waiting for a delivery that was never scheduled.
///
/// `progress` receives the running byte count of the copy, so a surface can show
/// a determinate bar; `cancel` stops it within one 64 KiB buffer, leaving no
/// blob and no queue entry.
///
/// # `Ok(None)`: the row was gone by the time we finished copying
///
/// A copy runs for as long as the file is big, and `chat_delete` can land in
/// the middle of it — as can a [disappearing-message
/// window](crate::Retention) short enough to close on a row that is still
/// being staged, which `ChatStore::get` reports the same way for the same
/// reason. `ChatStore::delete_conversation` keeps a `Staging` row
/// precisely so that this is nearly impossible, but "nearly" is not a contract:
/// the row is read inside [`ChatStore::enqueue_file`], and if it has gone by
/// then nothing is queued. There is exactly one honest response, and it is the
/// one below — delete the blob we staged and tell the caller to publish
/// nothing.
///
/// Queueing anyway would offer the peer a file that the very next drain tick
/// discards (a queue entry with no record is read as "nothing will ever settle
/// this"), leaving them an approval prompt for a stream that never comes.
/// Keeping the blob would leak the bytes until a sweep collected them. `None`
/// is not a failure, so no row is marked `Failed`: there is no row, and the
/// user's own delete is what removed it.
#[allow(clippy::too_many_arguments)]
pub async fn stage_file_send(
    store: &ChatStore,
    staging: &StagingStore,
    peer: &DeviceId,
    r: &mut FileRef,
    path: &str,
    limits: StagingLimits,
    cancel: &TransferControl,
    progress: &UnboundedSender<u64>,
) -> Result<Option<StagedFile>, SendError> {
    let staged = match staging.stage(&r.id, path, limits, cancel, progress).await {
        Ok(s) => s,
        Err(e) => {
            // Nothing was queued, so the row must say so rather than sit on
            // `Staging` forever with no transfer that could ever settle it.
            let _ = store.set_status(peer, &r.id, Status::Failed);
            return Err(SendError::Session(e.to_string()));
        }
    };
    r.size = staged.size;
    if !store.enqueue_file(peer, r, &staged)? {
        staging.remove(&staged.staged_path);
        tracing::info!(
            message_id = %r.id,
            peer_id = %peer.0,
            "staged file dropped: its row was gone by the time the copy finished"
        );
        return Ok(None);
    }
    Ok(Some(staged))
}

/// Stage a picked file and queue it for delivery: [`begin_file_send`] followed
/// by [`stage_file_send`], for a caller that has no reason to separate them.
///
/// Returns the [`FileRef`] to publish on CHAT and the [`StagedFile`] whose
/// `staged_path` is what the transfer must read — never the user's original,
/// which may be deleted, moved or rewritten between queueing and delivery.
///
/// `Ok(None)` carries [`stage_file_send`]'s own "the conversation was deleted
/// while we copied" answer through unchanged: nothing is queued, no blob is
/// left behind, and the caller must publish no `FileRef`.
pub async fn prepare_file_send(
    store: &ChatStore,
    staging: &StagingStore,
    peer: &DeviceId,
    path: &str,
    limits: StagingLimits,
    cancel: &TransferControl,
    progress: &UnboundedSender<u64>,
) -> Result<Option<(FileRef, StagedFile)>, SendError> {
    let mut r = begin_file_send(store, peer, path)?;
    let Some(staged) =
        stage_file_send(store, staging, peer, &mut r, path, limits, cancel, progress).await?
    else {
        return Ok(None);
    };
    Ok(Some((r, staged)))
}

/// Send a prepared [`FileRef`] over the session's CHAT channel.
///
/// Takes no store and no peer: the record was already persisted by
/// [`prepare_file_send`], and re-deriving the peer here would risk writing the
/// row into a second namespace. This is purely the wire step.
///
/// Waits for the peer's channel accept first, for the same reason
/// [`send_message`] does — `open_channel` resolves as soon as the open is
/// queued locally, so sending immediately would race the accept over any real
/// transport.
pub async fn send_file_ref(handle: &SessionHandle, r: &FileRef) -> Result<(), SendError> {
    let channel = chat_lane(handle).await?;
    let frame = r.to_frame(channel)?;
    let sent = handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()));
    // The lane belongs to the session, not to this frame — see `chat_lane`.
    sent
}

/// Tell the sender we turned their file down, over the session's CHAT channel.
///
/// Takes no store and no peer for the same reason [`send_file_ref`] does: the
/// caller has already settled its own row locally, and re-deriving the peer
/// here would risk writing into a second namespace. This is purely the wire
/// step, and it is best-effort by design — the decline exists to spare the
/// sender a pointless retry, never to gate the local decision, which is already
/// final whether or not this frame lands.
///
/// Only call this for a peer whose negotiated capabilities carry
/// `CHAT_FEAT_FILEDECLINE`; sending a message the negotiation says the peer does
/// not speak is exactly the silent wire drift capability negotiation exists to
/// prevent.
/// Send one [`Receipt`] — a read watermark — over the peer's CHAT channel.
///
/// Whether a receipt should be sent at all is the caller's decision: it depends
/// on `DeviceConfig::share_read_receipts`, which is a privacy choice and has no
/// business being read down here.
pub async fn send_receipt(handle: &SessionHandle, r: &Receipt) -> Result<(), SendError> {
    let channel = chat_lane(handle).await?;
    let frame = r.to_frame(channel)?;
    let sent = handle
        .send_on_channel(channel, Receipt::message_type(), frame.flags, frame.payload)
        .await
        .map_err(|e| SendError::Session(e.to_string()));
    // The lane belongs to the session, not to this frame — see `chat_lane`.
    sent
}

/// Send one [`Reaction`] to a peer over its own CHAT channel.
///
/// Same shape as [`send_file_decline`]: open, wait for open, send. The caller
/// decides *whether* to send — that decision reads the negotiated capability
/// and lives with the rest of the send policy, not here.
pub async fn send_reaction(handle: &SessionHandle, r: &Reaction) -> Result<(), SendError> {
    let channel = chat_lane(handle).await?;
    let frame = r.to_frame(channel)?;
    let sent = handle
        .send_on_channel(
            channel,
            Reaction::message_type(),
            frame.flags,
            frame.payload,
        )
        .await
        .map_err(|e| SendError::Session(e.to_string()));
    // The lane belongs to the session, not to this frame — see `chat_lane`.
    sent
}

pub async fn send_file_decline(handle: &SessionHandle, d: &FileDecline) -> Result<(), SendError> {
    let channel = chat_lane(handle).await?;
    let sent = send_decline_on_open_channel(handle, channel, d).await;
    sent
}

/// Send one [`FileDecline`] over a channel that has already reached
/// [`ChannelState::Open`] — the same split as [`send_on_open_channel`], so
/// [`flush_to_session`] can deliver a *queued* decline down the channel it has
/// already opened for text rather than opening a second one per entry.
async fn send_decline_on_open_channel(
    handle: &SessionHandle,
    channel: ChannelId,
    d: &FileDecline,
) -> Result<(), SendError> {
    let frame = d.to_frame(channel)?;
    handle
        .send_on_channel(
            channel,
            FileDecline::message_type(),
            frame.flags,
            frame.payload,
        )
        .await
        .map_err(|e| SendError::Session(e.to_string()))
}

/// What one poll of `handle.channels()` tells us to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    /// The channel is `Open`: proceed to send.
    Open,
    /// Not decided yet; poll again after the interval.
    KeepWaiting,
    /// The peer rejected (or otherwise killed) the channel: fail fast rather
    /// than waiting out the full budget.
    Rejected,
}

/// Pure decision for one poll, factored out of [`wait_for_channel_open`] so it
/// can be unit-tested without spinning up a session.
///
/// `found` is this channel's state in the current snapshot, if it is present
/// at all. `seen_before` is whether an *earlier* poll found it present (in any
/// state) — `open_channel` guarantees the channel is registered locally
/// (`Opening`) before it returns, so it is present on the very first poll in
/// the normal case.
///
/// The channel manager does not transition a rejected/closed/errored channel
/// through a terminal [`ChannelState`] that stays queryable — `handle_channel_
/// reject`/`_close`/`_closed`/`_error` all *remove* the entry from the
/// registry outright (see `peerbeam-transfer/src/session/channel_manager.rs`).
/// So the real rejection signal is the channel *disappearing* from the
/// snapshot after having been seen present: that is what actually fires below.
/// The terminal-state arm is kept as a belt-and-suspenders check in case a
/// future revision of the channel manager starts leaving rejected/closed
/// channels queryable in a terminal state instead of removing them.
fn decide(found: Option<ChannelState>, seen_before: bool) -> PollOutcome {
    match found {
        Some(state) if state.is_open() => PollOutcome::Open,
        Some(state) if state.is_terminal() || matches!(state, ChannelState::Errored) => {
            PollOutcome::Rejected
        }
        Some(_) => PollOutcome::KeepWaiting, // Opening (or Closing, before removal).
        None if seen_before => PollOutcome::Rejected, // was present, now gone: rejected.
        None => PollOutcome::KeepWaiting,    // not registered yet (brief lag right after open).
    }
}

/// Block (with a bounded poll loop, never indefinitely) until `channel`
/// reaches [`ChannelState::Open`] on our side, or fail once the peer's
/// rejection is detected (the channel disappearing after having been seen) or
/// the budget is exhausted.
async fn wait_for_channel_open(
    handle: &SessionHandle,
    channel: ChannelId,
) -> Result<(), SendError> {
    let deadline = Instant::now() + CHANNEL_OPEN_BUDGET;
    let mut seen = false;
    loop {
        let channels = handle
            .channels()
            .await
            .map_err(|e| SendError::Session(e.to_string()))?;
        let found = channels.iter().find(|c| c.id == channel).map(|c| c.state);
        match decide(found, seen) {
            PollOutcome::Open => return Ok(()),
            PollOutcome::Rejected => {
                return Err(SendError::Session(format!(
                    "chat channel {channel:?} rejected by peer"
                )));
            }
            PollOutcome::KeepWaiting => {}
        }
        if found.is_some() {
            seen = true;
        }
        if Instant::now() >= deadline {
            return Err(SendError::Session(format!(
                "chat channel {channel:?} did not open within {CHANNEL_OPEN_BUDGET:?}"
            )));
        }
        tokio::time::sleep(CHANNEL_OPEN_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Direction, Kind};
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::{AppStore, EncryptionProvider, StorageProvider};
    use peerbeam_storage_fs::FsStorage;
    use std::sync::Arc;

    fn store() -> (ChatStore, tempfile::TempDir) {
        let (cs, _raw, dir) = store_with_raw();
        (cs, dir)
    }

    /// [`store`] plus the raw `AppStore` behind it, so a test can write an entry
    /// no `ChatStore` method can produce (e.g. a `Kind::File` entry carrying no
    /// staged blob).
    fn store_with_raw() -> (ChatStore, Arc<dyn AppStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> =
            Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app.clone()), app, dir)
    }

    /// A staging store rooted inside `dir`, plus limits nothing in these tests
    /// can breach (the bounds themselves are `staging.rs`'s to prove).
    fn staging(dir: &tempfile::TempDir) -> (StagingStore, StagingLimits) {
        let storage: Arc<dyn StorageProvider> = Arc::new(FsStorage::new());
        (
            StagingStore::new(
                dir.path().join("blobs").to_string_lossy().into_owned(),
                storage,
            ),
            StagingLimits {
                max_bytes: u64::MAX,
                min_free_bytes: 0,
            },
        )
    }

    /// `prepare_file_send` without the progress/cancel ceremony a caller needs.
    ///
    /// Unwraps the "the conversation was deleted while we copied" answer: no
    /// test using this helper deletes anything mid-stage, so a `None` here
    /// would be a bug in the helper's own fixture rather than a case to handle.
    /// The tests that *do* exercise that answer call `stage_file_send`
    /// directly.
    async fn prepare(
        cs: &ChatStore,
        st: &StagingStore,
        limits: StagingLimits,
        peer: &DeviceId,
        path: &str,
    ) -> Result<(FileRef, StagedFile), SendError> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(
            prepare_file_send(cs, st, peer, path, limits, &TransferControl::new(), &tx)
                .await?
                .expect("the row is never deleted under these tests"),
        )
    }

    /// The contract the whole send path rests on: one id names the chat row, the
    /// staged blob and (later) the transfer; the row is persisted before the
    /// copy starts; and it carries the sender's own path so the UI can open it.
    #[tokio::test]
    async fn prepare_file_send_stages_the_bytes_and_queues_a_pending_row() {
        let (cs, dir) = store();
        let (st, limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("report.pdf");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let path = path.to_string_lossy().into_owned();

        let (r, staged) = prepare(&cs, &st, limits, &peer, &path).await.unwrap();

        assert_eq!(r.name, "report.pdf", "name is the path's base component");
        assert_eq!(r.size, 4096);
        assert_eq!(staged.size, 4096);
        assert!(
            staged.staged_path.ends_with(&r.id),
            "the blob is named by the id that names the row: {}",
            staged.staged_path
        );
        assert_eq!(std::fs::read(&staged.staged_path).unwrap(), vec![7u8; 4096]);

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "one file share is one row");
        assert_eq!(hist[0].id, r.id, "the record key IS the FileRef id");
        assert_eq!(hist[0].kind, Kind::File);
        assert_eq!(hist[0].direction, Direction::Out);
        assert_eq!(hist[0].status, Status::Pending, "queued, not yet sent");
        let meta = hist[0].file.clone().expect("file meta");
        assert_eq!(meta.name, "report.pdf");
        assert_eq!(meta.size, 4096);
        assert_eq!(
            meta.local_path.as_deref(),
            Some(path.as_str()),
            "the sender's own path is kept record-side (never on the wire)"
        );

        // And it is genuinely queued, with the blob the outbox owns.
        let queued = cs.outbox_for(&peer).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message_id, r.id);
        assert_eq!(queued[0].kind, Kind::File);
        assert_eq!(queued[0].file.as_ref().unwrap(), &staged);
    }

    /// The row exists, and says `Staging`, before a single byte is copied — so a
    /// multi-GB attach reads as work in progress rather than a hung UI.
    #[test]
    fn begin_file_send_persists_a_staging_row_before_any_copy() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![7u8; 64]).unwrap();

        let r = begin_file_send(&cs, &peer, &path.to_string_lossy()).unwrap();

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].id, r.id);
        assert_eq!(
            hist[0].status,
            Status::Staging,
            "nothing has been offered to anyone yet"
        );
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "a file is queued only once its bytes are safely staged"
        );
    }

    /// TRAP 4: the `FileRef` on the wire and the bytes on the stream must both
    /// describe the blob. A source still being written to makes the pre-copy
    /// metadata wrong, and a prompt that says one size while another number of
    /// bytes arrives is exactly the mismatch staging exists to remove.
    #[tokio::test]
    async fn a_source_that_grew_during_the_copy_is_advertised_at_its_staged_size() {
        let (cs, dir) = store();
        let (st, limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("growing.log");
        std::fs::write(&path, vec![1u8; 64]).unwrap();
        let mut r = begin_file_send(&cs, &peer, &path.to_string_lossy()).unwrap();
        assert_eq!(r.size, 64, "the pre-copy metadata size");

        // The file grows between the metadata read and the copy.
        std::fs::write(&path, vec![1u8; 4096]).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let staged = stage_file_send(
            &cs,
            &st,
            &peer,
            &mut r,
            &path.to_string_lossy(),
            limits,
            &TransferControl::new(),
            &tx,
        )
        .await
        .unwrap()
        .expect("the row is still there, so it queues");

        assert_eq!(staged.size, 4096, "the blob holds what was copied");
        assert_eq!(
            r.size, 4096,
            "the wire reference must describe the blob, not the stale metadata"
        );
        assert_eq!(
            cs.get(&peer, &r.id).unwrap().unwrap().file.unwrap().size,
            4096,
            "and so must our own row"
        );
    }

    /// A staging failure is immediate failure: nothing queued, and a row that
    /// says so rather than sitting on `Staging` forever waiting for a delivery
    /// that was never scheduled.
    #[tokio::test]
    async fn a_refused_stage_fails_the_row_and_queues_nothing() {
        let (cs, dir) = store();
        let (st, _limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("too-big.bin");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();

        let err = prepare(
            &cs,
            &st,
            StagingLimits {
                max_bytes: 1024,
                min_free_bytes: 0,
            },
            &peer,
            &path.to_string_lossy(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("limit"), "unexpected error: {err}");

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1, "the row written before the copy stays");
        assert_eq!(hist[0].status, Status::Failed);
        assert!(
            cs.outbox_for(&peer).unwrap().is_empty(),
            "a refused stage must queue nothing"
        );
    }

    /// **A conversation deleted while the copy was running.** The row is read
    /// inside `enqueue_file`, and by the time a multi-GB stage finishes it may
    /// simply not be there.
    ///
    /// There is one honest outcome and this pins all of it: nothing queued, and
    /// **no blob left on disk**. Queueing anyway would put a `FileRef` in front
    /// of the peer that the next drain tick discards — an approval prompt for a
    /// stream that never comes — and keeping the blob would leak the bytes
    /// until a sweep collected them. The blob assertion is the load-bearing
    /// one: an implementation that merely returned `None` and walked away would
    /// pass everything else here.
    ///
    /// The row is removed through the raw `AppStore` rather than through
    /// `delete_conversation`, which now (correctly) keeps a `Staging` row —
    /// this is the residual window that keep leaves, not the one it closes.
    #[tokio::test]
    async fn stage_file_send_whose_row_has_gone_queues_nothing_and_leaves_no_blob() {
        let (cs, raw, dir) = store_with_raw();
        let (st, limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("holiday.mp4");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();

        let mut r = begin_file_send(&cs, &peer, &path.to_string_lossy()).unwrap();
        // The delete lands mid-copy.
        assert!(raw.delete(&crate::store::namespace(&peer), &r.id).unwrap());

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let staged = stage_file_send(
            &cs,
            &st,
            &peer,
            &mut r,
            &path.to_string_lossy(),
            limits,
            &TransferControl::new(),
            &tx,
        )
        .await
        .expect("a deleted conversation is not an error, just nothing to queue");

        assert!(staged.is_none(), "the caller must publish no FileRef");
        assert!(
            cs.outbox_pending().unwrap().is_empty(),
            "nothing is queued for a conversation that no longer exists"
        );
        assert!(
            cs.history(&peer).unwrap().is_empty(),
            "and no row is resurrected under the peer"
        );
        // The bytes really were let go — the whole reason this is not simply a
        // `false` the caller ignores.
        let blob = dir.path().join("blobs").join(&r.id);
        assert!(
            !blob.exists(),
            "the staged copy must be deleted, not left to leak: {}",
            blob.display()
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join("blobs"))
                .map(|d| d.count())
                .unwrap_or(0),
            0,
            "no blob under any name survives"
        );
    }

    /// A folder is a different wire shape entirely; refusing it here is what
    /// keeps `chat send --file some/dir` from silently sending a 0-byte file.
    #[test]
    fn begin_file_send_refuses_a_directory_and_writes_nothing() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let sub = dir.path().join("a-folder");
        std::fs::create_dir_all(&sub).unwrap();

        let err = begin_file_send(&cs, &peer, &sub.to_string_lossy()).unwrap_err();
        assert!(
            err.to_string().contains("folders aren't supported"),
            "unexpected error: {err}"
        );
        assert!(
            cs.history(&peer).unwrap().is_empty(),
            "a refused share must leave no row behind"
        );
    }

    #[test]
    fn begin_file_send_refuses_a_missing_path_and_writes_nothing() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let missing = dir.path().join("nope.bin");

        let err = begin_file_send(&cs, &peer, &missing.to_string_lossy()).unwrap_err();
        assert!(
            err.to_string().contains("cannot read"),
            "unexpected error: {err}"
        );
        assert!(cs.history(&peer).unwrap().is_empty());
    }

    /// The one-file-at-a-time decision, and the FIFO it decides in: five queued
    /// videos yield exactly one entry to send, the oldest, over and over until
    /// that one is dequeued.
    #[tokio::test]
    async fn next_file_for_offers_the_oldest_file_one_at_a_time() {
        let (cs, dir) = store();
        let (st, limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");
        let other = DeviceId::from("pb-carol");

        let mut ids = Vec::new();
        for n in 0..5 {
            let path = dir.path().join(format!("v{n}.mp4"));
            std::fs::write(&path, vec![n as u8; 32]).unwrap();
            let (r, _) = prepare(&cs, &st, limits, &peer, &path.to_string_lossy())
                .await
                .unwrap();
            ids.push(r.id);
        }
        // Another peer's queue must not be offered for this one.
        let theirs = dir.path().join("theirs.bin");
        std::fs::write(&theirs, b"x").unwrap();
        prepare(&cs, &st, limits, &other, &theirs.to_string_lossy())
            .await
            .unwrap();

        let first = next_file_for(&cs, &peer).unwrap().expect("one to send");
        assert_eq!(first.entry.message_id, ids[0], "FIFO: the oldest first");
        assert_eq!(first.file.size, 32);
        assert_eq!(
            next_file_for(&cs, &peer).unwrap().unwrap().entry.message_id,
            ids[0],
            "still the same one until it is dequeued — a caller cannot be handed \
             a second file merely by asking twice"
        );

        cs.outbox_remove(&ids[0]).unwrap();
        assert_eq!(
            next_file_for(&cs, &peer).unwrap().unwrap().entry.message_id,
            ids[1]
        );
    }

    /// Neither text nor a queued decline is ever offered as a file to send. Nor
    /// is a `Kind::File` entry carrying no staged blob: nothing can produce one
    /// today, but a partially-written or hand-edited row could, and returning it
    /// would stall every real file behind it forever.
    #[test]
    fn next_file_for_ignores_text_a_decline_and_a_file_entry_with_no_blob() {
        let (cs, raw, dir) = store_with_raw();
        let peer = DeviceId::from("pb-bob");
        cs.enqueue(&peer, &ChatMessage::new("hi").unwrap()).unwrap();
        cs.enqueue_decline(&peer, &FileDecline::new("0000000000001"))
            .unwrap();
        assert!(next_file_for(&cs, &peer).unwrap().is_none());

        let broken = OutboxEntry {
            peer_id: peer.0.clone(),
            message_id: "0000000000002".into(),
            body: String::new(),
            timestamp: "2026-08-13T10:00:00+00:00".into(),
            kind: Kind::File,
            file: None,
            offers_refused: 0,
            in_reply_to: None,
            group: None,
        };
        raw.put(
            crate::store::OUTBOX_NS,
            &broken.message_id,
            &serde_json::to_vec(&broken).unwrap(),
        )
        .unwrap();
        assert!(
            next_file_for(&cs, &peer).unwrap().is_none(),
            "a file entry with nothing to send must be skipped, not returned"
        );

        // The guard cannot pass by refusing everything: a real queued file, sat
        // behind all three of those, is still found. Real to the blob, too —
        // an entry whose bytes are missing is skipped in its own right.
        let r = FileRef::new("a.bin", 1).unwrap();
        cs.append(&ChatRecord::file_out(
            &peer,
            &r,
            FileMeta::new(&r.name, r.size, None),
            Status::Staging,
        ))
        .unwrap();
        let blob = dir.path().join(&r.id);
        std::fs::write(&blob, b"a").unwrap();
        assert!(
            cs.enqueue_file(
                &peer,
                &r,
                &StagedFile {
                    name: "a.bin".into(),
                    size: 1,
                    staged_path: blob.to_string_lossy().into_owned(),
                },
            )
            .unwrap(),
            "the row seeded above is there, so it queues"
        );
        assert_eq!(
            next_file_for(&cs, &peer).unwrap().unwrap().entry.message_id,
            r.id
        );
    }

    /// **A file whose staged bytes have gone must not block the queue behind
    /// it.** The caller re-checks the blob before offering it and skips — but if
    /// this function kept returning that same dead entry, every later file for
    /// that peer would sit behind it forever, on every drain tick, with only a
    /// log line as the symptom. Reachable without any bug: `SECURITY.md` tells
    /// users where `outbox-blobs/` lives and that it can hold gigabytes, so one
    /// clear-out strands the oldest entry.
    ///
    /// Skipped, never dequeued: the entry is still in the outbox afterwards, so
    /// it recovers by itself if the blob comes back.
    #[tokio::test]
    async fn next_file_for_steps_over_an_entry_whose_blob_has_gone() {
        let (cs, dir) = store();
        let (st, limits) = staging(&dir);
        let peer = DeviceId::from("pb-bob");

        let mut ids = Vec::new();
        for n in 0..2 {
            let path = dir.path().join(format!("v{n}.mp4"));
            std::fs::write(&path, vec![n as u8; 16]).unwrap();
            let (r, _) = prepare(&cs, &st, limits, &peer, &path.to_string_lossy())
                .await
                .unwrap();
            ids.push(r.id);
        }
        // The oldest is the one a drain would take. Take its bytes away.
        let first = next_file_for(&cs, &peer).unwrap().expect("the oldest");
        assert_eq!(first.entry.message_id, ids[0], "FIFO, before the blob goes");
        std::fs::remove_file(&first.file.staged_path).unwrap();

        let next = next_file_for(&cs, &peer)
            .unwrap()
            .expect("the second file must still be sendable");
        assert_eq!(
            next.entry.message_id, ids[1],
            "a dead head-of-queue entry must be stepped over, not returned forever"
        );
        assert!(
            std::path::Path::new(&next.file.staged_path).exists(),
            "and what is handed over always has its bytes"
        );

        // Skipped, not dequeued — and it recovers if the blob returns.
        assert_eq!(cs.outbox_for(&peer).unwrap().len(), 2, "both still queued");
        std::fs::write(&first.file.staged_path, vec![0u8; 16]).unwrap();
        assert_eq!(
            next_file_for(&cs, &peer).unwrap().unwrap().entry.message_id,
            ids[0],
            "the oldest is head of the queue again once its bytes are back"
        );
    }

    #[test]
    fn decide_opens_once_state_is_open() {
        assert_eq!(decide(Some(ChannelState::Open), false), PollOutcome::Open);
        assert_eq!(decide(Some(ChannelState::Open), true), PollOutcome::Open);
    }

    #[test]
    fn decide_keeps_waiting_while_opening_or_not_yet_registered() {
        assert_eq!(
            decide(Some(ChannelState::Opening), false),
            PollOutcome::KeepWaiting
        );
        assert_eq!(
            decide(Some(ChannelState::Opening), true),
            PollOutcome::KeepWaiting
        );
        // Not present yet and never seen: could just be registration lag right
        // after `open_channel` returned — keep waiting rather than giving up.
        assert_eq!(decide(None, false), PollOutcome::KeepWaiting);
    }

    #[test]
    fn decide_rejects_fast_when_a_previously_seen_channel_disappears() {
        // This is the real-world rejection signal: the channel manager removes
        // a rejected/closed/errored channel from the registry outright instead
        // of leaving it in a terminal state, so "gone after being seen" is what
        // actually fires.
        assert_eq!(decide(None, true), PollOutcome::Rejected);
    }

    #[test]
    fn decide_rejects_on_a_terminal_state_belt_and_suspenders() {
        // Currently dead in production (the manager never leaves a channel
        // queryable in these states — it removes it), kept as a safety net.
        assert_eq!(
            decide(Some(ChannelState::Rejected), false),
            PollOutcome::Rejected
        );
        assert_eq!(
            decide(Some(ChannelState::Closed), true),
            PollOutcome::Rejected
        );
        assert_eq!(
            decide(Some(ChannelState::Errored), false),
            PollOutcome::Rejected
        );
    }
}
