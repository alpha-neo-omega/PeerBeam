//! Sending a chat message over an established session.

use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{ChannelId, ChannelState, ChannelType};
use peerbeam_transfer::SessionHandle;

use crate::message::{ChatError, ChatMessage, FileRef};
use crate::record::{ChatRecord, FileMeta, Status};
use crate::store::ChatStore;

/// Send one already-built [`ChatMessage`] over a channel that has already
/// reached [`ChannelState::Open`] (factored out of [`send_message`] so
/// [`flush_to_session`] can reuse the same wire-send step for each queued
/// entry without re-minting a message or re-opening the channel).
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
/// Persisting happens strictly after a successful send: on any failure (the
/// channel never opens, or the send itself errors) no record is written, so
/// local history never shows a message as sent when it was not.
pub async fn send_message(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
    body: &str,
) -> Result<ChatRecord, SendError> {
    let msg = ChatMessage::new(body)?; // enforces MAX_BODY
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
    send_on_open_channel(handle, channel, &msg).await?;
    let rec = ChatRecord::sent(peer, &msg);
    store.append(&rec)?;
    Ok(rec)
}

/// Flush all of `peer`'s queued outbox messages over an established session.
/// Opens the CHAT channel once, sends each queued message in FIFO order, and on
/// each success upserts the conversation record to `Sent` and removes the
/// outbox entry. Returns the message ids flushed. A per-message send error
/// stops this peer's flush (the rest stay queued); a channel-open failure
/// returns `Err`.
pub async fn flush_to_session(
    handle: &SessionHandle,
    store: &ChatStore,
    peer: &DeviceId,
) -> Result<Vec<String>, SendError> {
    let entries = store.outbox_for(peer)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;

    let mut flushed = Vec::new();
    for entry in entries {
        let msg = ChatMessage {
            id: entry.message_id.clone(),
            timestamp: entry.timestamp.clone(),
            body: entry.body.clone(),
        };
        if send_on_open_channel(handle, channel, &msg).await.is_err() {
            break; // peer went away mid-flush; remaining entries stay queued
        }
        store.record_sent(&entry)?;
        store.outbox_remove(&entry.message_id)?;
        flushed.push(entry.message_id);
    }
    Ok(flushed)
}

/// Validate a path and stage a file-share: mint the [`FileRef`] and persist the
/// outgoing record. Does no I/O beyond metadata — the bytes move over TRANSFER,
/// correlated with the returned reference by its `id`.
///
/// The record is written **before** anything touches the network, and written
/// as [`Status::Transferring`], on purpose:
///
/// * the id has to be durable and visible in history before it is published
///   out of band as a transfer id, so a failure mid-send can never leave a
///   half-sent file with no row to settle;
/// * a dial that never connects therefore leaves a `Transferring` row, which
///   [`ChatStore::reconcile_peer`] flips to `Status::Interrupted` at the next
///   start. That is the intended shape: a row that outlives its process is
///   settled by reconciliation, not by hoping an event still arrives.
///
/// Rejects a directory outright — a folder share is a different wire shape
/// (`send_folder`) and is not part of file-in-chat.
pub fn prepare_file_send(
    store: &ChatStore,
    peer: &DeviceId,
    path: &str,
) -> Result<(FileRef, ChatRecord), SendError> {
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
    let rec = ChatRecord::file_out(
        peer,
        &r,
        FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some(path.to_string()),
        },
        Status::Transferring,
    );
    store.append(&rec)?;
    Ok((r, rec))
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
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .map_err(|e| SendError::Session(e.to_string()))?;
    wait_for_channel_open(handle, channel).await?;
    let frame = r.to_frame(channel)?;
    handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
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
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Arc;

    fn store() -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app), dir)
    }

    /// The staging contract the whole send path rests on: one id names both the
    /// chat row and (later) the transfer, the row is persisted before anything
    /// is dialed, and it carries the sender's local path so the UI can open it.
    #[test]
    fn prepare_file_send_persists_a_transferring_row_keyed_by_the_file_ref_id() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let path = dir.path().join("report.pdf");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let path = path.to_string_lossy().into_owned();

        let (r, rec) = prepare_file_send(&cs, &peer, &path).unwrap();

        assert_eq!(r.name, "report.pdf", "name is the path's base component");
        assert_eq!(r.size, 4096, "size is read from the filesystem");
        assert_eq!(rec.id, r.id, "the record key IS the FileRef id");

        let hist = cs.history(&peer).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].id, r.id);
        assert_eq!(hist[0].kind, Kind::File);
        assert_eq!(hist[0].direction, Direction::Out);
        assert_eq!(
            hist[0].status,
            Status::Transferring,
            "persisted before the network, so a crash is reconcilable"
        );
        let meta = hist[0].file.clone().expect("file meta");
        assert_eq!(meta.name, "report.pdf");
        assert_eq!(meta.size, 4096);
        assert_eq!(
            meta.local_path.as_deref(),
            Some(path.as_str()),
            "the sender's own path is kept record-side (never on the wire)"
        );
    }

    /// A folder is a different wire shape entirely; refusing it here is what
    /// keeps `chat send --file some/dir` from silently sending a 0-byte file.
    #[test]
    fn prepare_file_send_refuses_a_directory_and_writes_nothing() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let sub = dir.path().join("a-folder");
        std::fs::create_dir_all(&sub).unwrap();

        let err = prepare_file_send(&cs, &peer, &sub.to_string_lossy()).unwrap_err();
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
    fn prepare_file_send_refuses_a_missing_path_and_writes_nothing() {
        let (cs, dir) = store();
        let peer = DeviceId::from("pb-bob");
        let missing = dir.path().join("nope.bin");

        let err = prepare_file_send(&cs, &peer, &missing.to_string_lossy()).unwrap_err();
        assert!(
            err.to_string().contains("cannot read"),
            "unexpected error: {err}"
        );
        assert!(cs.history(&peer).unwrap().is_empty());
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
