//! Recursive folder transfer with structure preservation.
//!
//! Builds on the single-file streaming core. A folder transfer is:
//!
//! ```text
//! Manifest(root, [ (rel_path, size) … ])          S→R
//! ResumeState([ 0, 0, … ])                        R→S
//! for each file:
//!   FileHeader(index, rel_path, size, offset=0)    S→R
//!   Chunk … Chunk                                  S→R
//!   FileEnd(index)                                 S→R
//! Complete                                         S→R
//! ```
//!
//! **Preserve structure** — each file keeps its path relative to the folder
//! root; the receiver recreates the tree under `dest_dir/<root>/…`. Relative
//! paths are sanitized (no `..`, no absolute) to prevent traversal.
//!
//! **Staged, like a single file** — each entry is written to `part_path(dest)`
//! and renamed onto its real name only once it has been flushed and closed.
//! This is not decoration: it used to `open_write` the destination directly, so
//! a folder receive that lost its connection mid-file left a **truncated file
//! under the name the user expects**, indistinguishable from a complete one,
//! and `docs/SECURITY.md`'s "Safe file writing" was false for this path. A tail
//! that never arrives now leaves a `.part` behind instead of a plausible lie.
//!
//! **No resume (yet)** — folder transfers are not wired to any resume UI/FFI,
//! so every folder receive is treated as fresh: each entry is written with
//! `open_write` (create/truncate) into its staging path, overwriting anything
//! already there rather than blind-appending onto it (blind-appending onto a
//! same-sized pre-existing file used to silently corrupt it). The wire messages
//! still carry a `have`/`offset` so a future resume feature can slot in without
//! a protocol change, but today the receiver always reports `0` and the sender
//! always streams from the start.
//!
//! TODO(transfer): folder *resume* — reusing the staged `.part` across attempts
//! rather than restarting it — is still a separate future task.
//!
//! A single unreadable source file (send side), or a destination path that
//! collides with an existing directory or cannot be made safe to write
//! (receive side), is skipped with a warning rather than aborting the whole
//! transfer: the other files in the folder did nothing wrong, and no entry
//! that is skipped was ever written.

use futures::io::AsyncWrite;
use futures::AsyncWriteExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::{Direction, Progress, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};

use crate::control::TransferControl;
use bytes::Bytes;

use crate::protocol::chunk_frame_owned;
use crate::stream::{
    build_progress, free_destination, read_fill, safe_component, send_with_retry,
    signal_pause_edge, to_hex, TransferOutcome,
};

// ── Wire messages ───────────────────────────────────────────────

/// One file's entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileMeta {
    path: String,
    size: u64,
}

/// Folder-transfer control/metadata messages (carried in Control frames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum FolderMessage {
    Manifest {
        transfer_id: String,
        root: String,
        files: Vec<FileMeta>,
    },
    ResumeState {
        have: Vec<u64>,
    },
    FileHeader {
        index: u32,
        path: String,
        size: u64,
        offset: u64,
    },
    FileEnd {
        index: u32,
        /// SHA-256 of the entry's bytes, hex — what makes a folder confirmed
        /// **correct** and not merely arrived.
        ///
        /// `Option`, defaulted and skipped when absent, so this is additive in
        /// both directions: an older receiver ignores an unknown field (nothing
        /// here sets `deny_unknown_fields`), and an older sender's `FileEnd`
        /// still parses, leaving the receiver with nothing to check — which is
        /// exactly what it could do before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum: Option<String>,
    },
    Complete,
    /// Receiver → sender: every entry arrived and was published.
    ///
    /// **Sent only when `TRANSFER_FEAT_FOLDER_ACK` was negotiated.** The enum is
    /// externally tagged, so a sender that predates this variant fails to parse
    /// it rather than ignoring it — the capability check is what keeps this
    /// additive.
    Received {
        files: u64,
        /// Entries whose bytes did not match the sender's digest, and which
        /// were therefore **not** published. Non-zero fails the send.
        #[serde(default)]
        corrupt: u64,
    },
    Cancel,
    /// Sender → receiver: cooperative pause — see `protocol::Control::Pause`.
    Pause,
    /// Sender → receiver: cooperative resume — see `protocol::Control::Resume`.
    Resume,
}

fn folder_frame(msg: &FolderMessage) -> Frame {
    Frame {
        kind: FrameKind::Control,
        payload: bytes::Bytes::from(serde_json::to_vec(msg).expect("FolderMessage serializable")),
    }
}

fn parse_folder(frame: &Frame) -> Result<FolderMessage> {
    serde_json::from_slice(&frame.payload)
        .map_err(|e| DomainError::Transfer(format!("bad folder message: {e}")))
}

/// Decode a folder transfer's opening `Manifest` frame into just the parts a
/// caller can show before accepting: the sender's transfer id, the sanitized
/// root name, and the summed byte total.
///
/// Kept here (rather than exposing `FolderMessage`) so the folder wire enum
/// stays private, and deliberately *frame-shaped* rather than link-shaped:
/// `recv_manifest` loops reading frames until it finds a manifest, which would
/// consume the frame a peek has already read. `session::peek_incoming_meta`
/// reads exactly one frame and hands it here.
///
/// Returns `None` for anything that is not a well-formed `Manifest` — the
/// caller treats that as "nothing learned", never as a failure.
pub(crate) fn manifest_preview(frame: &Frame) -> Option<(String, String, u64)> {
    match parse_folder(frame).ok()? {
        FolderMessage::Manifest {
            transfer_id,
            root,
            files,
        } => {
            let total = files.iter().map(|f| f.size).fold(0u64, u64::saturating_add);
            Some((transfer_id, sanitize_name(&root), total))
        }
        _ => None,
    }
}

// ── Public API ──────────────────────────────────────────────────

/// Parameters for sending a folder.
#[derive(Debug, Clone)]
pub struct FolderSendRequest {
    /// Unique transfer id (echoed into progress).
    pub transfer_id: String,
    /// Local folder root to send.
    pub root_path: String,
    /// Chunk size in bytes.
    pub chunk_size: u32,
}

/// Result of receiving a folder.
#[derive(Debug, Clone)]
pub struct FolderReceived {
    /// How it ended.
    pub outcome: TransferOutcome,
    /// The (sanitized) root folder name written under `dest_dir`.
    pub root: String,
    /// Number of files that ended up complete.
    pub files: usize,
    /// Total bytes written during this transfer.
    pub bytes: u64,
    /// The sender's transfer id, from the wire meta — lets a caller correlate
    /// this receive with an out-of-band reference such as a chat FileRef.
    pub transfer_id: String,
}

/// Send a folder recursively over `link`, preserving structure. Skips (with a
/// warning) any source file that fails to open rather than aborting.
/// `expect_ack` is the negotiated `TRANSFER_FEAT_FOLDER_ACK` bit. With it, this
/// waits for the receiver's `Received` before reporting `Completed`, so success
/// means the bytes landed rather than that they reached a send buffer. Without
/// it — a peer that predates the bit — nothing is expected and the behaviour is
/// exactly what it was.
pub async fn send_folder(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    req: FolderSendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
    expect_ack: bool,
) -> Result<TransferOutcome> {
    // TODO(transfer): empty subdirectories are not preserved — `list_files`
    // only returns files, so a source folder that contains only empty dirs
    // arrives with no structure at all on the receiver.
    let files = storage.list_files(&req.root_path).await?;
    let root = base_name(&req.root_path);
    let total: u64 = files.iter().map(|(_, s)| *s).sum();
    let files_total = files.len() as u32;

    let manifest = FolderMessage::Manifest {
        transfer_id: req.transfer_id.clone(),
        root: root.clone(),
        files: files
            .iter()
            .map(|(p, s)| FileMeta {
                path: p.clone(),
                size: *s,
            })
            .collect(),
    };
    send_with_retry(link, folder_frame(&manifest), retries).await?;

    let have = recv_resume(link).await?;

    let mut done: u64 = 0;
    let mut files_completed: u32 = 0;
    let chunk = req.chunk_size.max(1) as usize;

    // Cooperative pause: edge-detect our own pause state and tell the
    // receiver on the main stream — see `stream::send_file`'s identical
    // mechanism (shared via `signal_pause_edge`). Tracked once for the whole
    // folder transfer, since either the per-file or the per-chunk loop below
    // may be the one blocking when paused.
    let mut signalled_pause = false;

    for (i, (rel, size)) in files.iter().enumerate() {
        let already = have.get(i).copied().unwrap_or(0).min(*size);

        // Zero-byte files must not match the "already complete" skip
        // (0 >= 0): the receiver still needs the FileHeader to create them.
        if *size > 0 && already >= *size {
            // Receiver already has the whole file — skip it.
            done += *size;
            files_completed += 1;
            emit(
                progress,
                &req.transfer_id,
                total,
                done,
                rel,
                files_completed,
                files_total,
                Direction::Sending,
                TransferStatus::Transferring,
            );
            continue;
        }

        signal_pause_edge(
            link,
            ctrl,
            &mut signalled_pause,
            || folder_frame(&FolderMessage::Pause),
            || folder_frame(&FolderMessage::Resume),
        )
        .await;
        if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
            return Ok(outcome);
        }

        // Open the source file BEFORE announcing it: a file that vanished,
        // got locked, or lost read permission between the manifest snapshot
        // above and now must not kill the whole transfer, and the receiver
        // must never see a `FileHeader` for a file we then fail to stream
        // (no phantom/partial entry left behind).
        let src = join(&req.root_path, rel);
        // One digest per entry, not one per folder: the receiver verifies each
        // file before it renames it into place, so a single corrupted entry is
        // withheld while the rest of the tree still lands. A folder-wide hash
        // could only condemn everything or nothing.
        let mut entry_hash = Sha256::new();
        let mut reader = match storage.open_read(&src, already).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("skipping unreadable file {rel}: {e}");
                continue;
            }
        };

        send_with_retry(
            link,
            folder_frame(&FolderMessage::FileHeader {
                index: i as u32,
                path: rel.clone(),
                size: *size,
                offset: already,
            }),
            retries,
        )
        .await?;
        done += already;

        loop {
            signal_pause_edge(
                link,
                ctrl,
                &mut signalled_pause,
                || folder_frame(&FolderMessage::Pause),
                || folder_frame(&FolderMessage::Resume),
            )
            .await;
            if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
                return Ok(outcome);
            }
            let mut buf = vec![0u8; chunk];
            let n = read_fill(reader.as_mut(), &mut buf).await?;
            if n == 0 {
                break;
            }
            buf.truncate(n);

            // Metered exactly as the single-file loop is, and for the same
            // reason: after the read and before the wire, charged for the bytes
            // actually read. Without this the configured ceiling applied to
            // `send file` and silently did nothing for `send folder/` — which is
            // the case somebody sets a ceiling *for*, a large thing going out
            // over a link other people are using. Zero-cost when unlimited.
            let wait = ctrl.throttle(n as u64);
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }

            entry_hash.update(&buf);
            send_with_retry(link, chunk_frame_owned(Bytes::from(buf)), retries).await?;
            done += n as u64;
            emit(
                progress,
                &req.transfer_id,
                total,
                done,
                rel,
                files_completed,
                files_total,
                Direction::Sending,
                TransferStatus::Transferring,
            );
        }

        send_with_retry(
            link,
            folder_frame(&FolderMessage::FileEnd {
                index: i as u32,
                checksum: Some(to_hex(&entry_hash.finalize())),
            }),
            retries,
        )
        .await?;
        files_completed += 1;
    }

    send_with_retry(link, folder_frame(&FolderMessage::Complete), retries).await?;
    // **The bytes are not delivered because the last frame was written.**
    // `send_frame` returns once the transport accepts the bytes into its send
    // buffer, and the session closes the shared QUIC connection shortly after
    // this returns — which quinn documents as licence for the peer to discard
    // stream data it has received but not yet handed to the application. So a
    // folder used to be reported sent while its tail was thrown away, and the
    // single-file path has always known better: it ends on the receiver's
    // verdict, not on its own write.
    if expect_ack {
        wait_for_folder_ack(link, files_total).await?;
    }
    emit(
        progress,
        &req.transfer_id,
        total,
        total,
        &root,
        files_total,
        files_total,
        Direction::Sending,
        TransferStatus::Completed,
    );
    Ok(TransferOutcome::Completed)
}

/// How long the sender waits for the receiver's `Received` before giving up.
///
/// The receiver sends it the moment it has flushed and renamed the last entry,
/// so this covers one round trip plus that final `fsync` — not the transfer.
/// Generous enough for a slow link and a slow disk; short enough that a peer
/// which died mid-write does not hold a send open indefinitely.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Block until the receiver confirms the folder landed, or fail the send.
///
/// **A timeout here is a failure, not a shrug.** The whole point of the
/// acknowledgement is that "sent" should mean "arrived"; treating silence as
/// success would restore precisely the bug it exists to close. The caller only
/// reaches this when the peer advertised `TRANSFER_FEAT_FOLDER_ACK`, so silence
/// means the peer stopped answering, not that it does not understand.
///
/// A `Cancel` is reported as cancelled rather than as an error: the receiver
/// turning the folder down is an outcome, not a fault.
async fn wait_for_folder_ack(link: &mut dyn Link, files_total: u32) -> Result<()> {
    let deadline = tokio::time::timeout(ACK_TIMEOUT, async {
        loop {
            match link.recv_frame().await? {
                Some(frame) if frame.kind == FrameKind::Control => {
                    match parse_folder(&frame)? {
                        FolderMessage::Received { files, corrupt } => return Ok((files, corrupt)),
                        FolderMessage::Cancel => {
                            return Err(DomainError::Transfer(
                                "the receiver cancelled the folder".into(),
                            ))
                        }
                        // Anything else is a peer that is still talking; keep
                        // reading rather than guessing what it meant.
                        _ => continue,
                    }
                }
                // Data frames cannot appear here — the sender is done writing —
                // but ignoring them is safer than failing on one.
                Some(_) => continue,
                None => {
                    return Err(DomainError::Transfer(
                        "the link closed before the receiver confirmed the folder".into(),
                    ))
                }
            }
        }
    })
    .await;

    let (files, corrupt) = match deadline {
        Ok(r) => r?,
        Err(_) => {
            return Err(DomainError::Transfer(
                "the receiver never confirmed the folder arrived".into(),
            ))
        }
    };
    // **Corruption fails the send.** The receiver withheld those entries rather
    // than publishing them, so the folder on its disk is incomplete — reporting
    // `Completed` would be the same lie in a new place. `Integrity`, not
    // `Transfer`, because that is what the single-file path raises for a
    // checksum mismatch and the two should not be told apart by their errors.
    if corrupt > 0 {
        return Err(DomainError::Integrity(format!(
            "{corrupt} of {files_total} folder entries failed their checksum and were not written"
        )));
    }
    // Reported, not enforced: the receiver legitimately publishes fewer entries
    // than were offered — a name that collides with a directory is skipped by
    // design — and failing the whole send over one skipped file would be worse
    // than the warning. What matters is that it answered at all.
    if files != u64::from(files_total) {
        tracing::warn!(
            "folder acknowledged with {files} of {files_total} entries; some were skipped"
        );
    }
    Ok(())
}

/// Receive a folder recursively over `link`, into `dest_dir/<root>/…`.
///
/// `ack` is the negotiated `TRANSFER_FEAT_FOLDER_ACK` bit: with it, a completed
/// folder is confirmed back to the sender once the last entry is flushed and
/// renamed, so the sender's "sent" means the bytes are on this disk.
pub async fn receive_folder(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    ack: bool,
) -> Result<FolderReceived> {
    let (transfer_id, root, files) = recv_manifest(link).await?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    let files_total = files.len() as u32;

    // No resume: folder transfers always start fresh (see module docs), so
    // every file is reported as having 0 bytes already — regardless of
    // whatever may already exist at the destination path. Reporting a
    // pre-existing file's size here would make the sender treat it as an
    // already-received prefix and skip it (if same size) or the receiver
    // would blind-append onto it (if smaller), corrupting a file that just
    // happens to share a destination name. `open_write` below always
    // creates/truncates instead.
    let have = vec![0u64; files.len()];
    send_with_retry(
        link,
        folder_frame(&FolderMessage::ResumeState { have: have.clone() }),
        0,
    )
    .await?;

    let mut done: u64 = 0;
    let mut files_completed: u32 = 0;

    // Destinations already written during this receive, folded to lower case.
    //
    // **macOS and Android are case-insensitive.** `Notes.txt` and `notes.txt`
    // are one file there, so a folder holding both had its second entry silently
    // overwrite the first — and `files_completed` still counted two, so the
    // transfer reported that everything arrived while the user was one file
    // short. Linux, being case-sensitive, is unaffected, which is why nothing
    // caught it.
    let mut written: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current: Option<Box<dyn AsyncWrite + Unpin + Send>> = None;
    // The entry being written, as (staging path, final path).
    //
    // **A folder entry is staged like a single file, and for the same reason.**
    // This path used to `open_write` the FINAL destination, so an interrupted
    // folder receive left a truncated file sitting under the name the user
    // expects — indistinguishable from a complete one, and `docs/SECURITY.md`'s
    // "Safe file writing" was simply false here. `stream::receive_file` has
    // always staged into `part_path` and renamed on success; this now does the
    // same, so a tail that never arrives leaves a `.part` behind rather than a
    // plausible-looking lie.
    //
    // The third field is the staging **claim**. `receive_file` has held one
    // since two concurrent receives of the same name were found interleaving
    // into one `.part`; folder entries were never given the same guarantee, so
    // two folder receives carrying `DCIM/IMG_0001.jpg` — or a folder and a
    // single file — still shared a staging path and published a mixture of both
    // senders' bytes. Holding the claim here closes that, and dropping it with
    // `current_paths` frees the name on every exit.
    let mut current_paths: Option<(String, String, crate::stream::StagingClaim)> = None;
    // Rolling digest of the entry being written, compared against the sender's
    // on `FileEnd`.
    let mut entry_hash = Sha256::new();
    // How many bytes the current entry declared, and how many have landed.
    //
    // The manifest's size is a bound, not a hint: without it a sender could
    // declare a small folder and stream without end into the receiver's disk,
    // and — if it finished with an honest checksum — have the oversized file
    // published under the declared name. `stream::receive_file` carries the
    // same guard for the single-file path.
    let mut entry_limit: u64 = 0;
    let mut entry_written: u64 = 0;
    // Entries whose bytes did not match and were therefore withheld. Reported
    // back so the sender fails rather than believing the folder landed whole.
    let mut corrupt: u64 = 0;
    // Set whenever the file announced by the most recent `FileHeader` was
    // skipped (unsafe path or a create/open failure, e.g. a destination path
    // that collides with an existing directory) — its `FileEnd` must not be
    // counted as completed.
    let mut current_skipped = false;

    // Cooperative pause: edge-detect a *local* pause on our own `ctrl` (this
    // side's own user pausing the receive — never set by a peer `Pause`
    // frame; see the `FolderMessage::Pause` arm below for why) and mirror it
    // to the sender via `Progress{status: Paused}`, exactly like
    // `stream::receive_file`.
    let mut signalled_pause = false;

    let outcome = loop {
        if ctrl.is_cancelled() {
            let _ = link.send_frame(folder_frame(&FolderMessage::Cancel)).await;
            break TransferOutcome::Cancelled;
        }

        if ctrl.is_paused() && !signalled_pause {
            signalled_pause = true;
            emit(
                progress,
                &transfer_id,
                total,
                done,
                &root,
                files_completed,
                files_total,
                Direction::Receiving,
                TransferStatus::Paused,
            );
        } else if !ctrl.is_paused() && signalled_pause {
            signalled_pause = false;
            emit(
                progress,
                &transfer_id,
                total,
                done,
                &root,
                files_completed,
                files_total,
                Direction::Receiving,
                TransferStatus::Transferring,
            );
        }

        // Honor a receiver-side pause: stop draining frames (transport
        // backpressure stalls the sender) and stop writing. wait_while_paused
        // is a no-op when not paused and also returns on cancel, which the
        // biased cancelled() branch of the select below then handles.
        ctrl.wait_while_paused().await;

        // Race the next frame against cancellation — see the identical
        // comment in `stream::receive_file`: without this, a sender that
        // stalls mid-folder would leave this parked on `recv_frame` forever
        // even after the caller cancels.
        let frame = tokio::select! {
            biased;
            _ = ctrl.cancelled() => {
                let _ = link.send_frame(folder_frame(&FolderMessage::Cancel)).await;
                break TransferOutcome::Cancelled;
            }
            frame = link.recv_frame() => frame?,
        };

        match frame {
            Some(frame) => match frame.kind {
                FrameKind::Chunk => {
                    if let Some(writer) = current.as_mut() {
                        // See `entry_limit`: the declared size bounds what may
                        // be written under that name.
                        if entry_written + frame.payload.len() as u64 > entry_limit {
                            return Err(DomainError::Transfer(
                                "sender exceeded the size its manifest declared".into(),
                            ));
                        }
                        writer
                            .write_all(&frame.payload)
                            .await
                            .map_err(|e| DomainError::Storage(format!("write chunk: {e}")))?;
                        entry_hash.update(&frame.payload);
                        entry_written += frame.payload.len() as u64;
                        done += frame.payload.len() as u64;
                        emit(
                            progress,
                            &transfer_id,
                            total,
                            done,
                            &root,
                            files_completed,
                            files_total,
                            Direction::Receiving,
                            TransferStatus::Transferring,
                        );
                    }
                }
                FrameKind::Control => match parse_folder(&frame)? {
                    FolderMessage::FileHeader {
                        path, size, offset, ..
                    } => {
                        // A resumed entry has `offset` bytes already on disk, so
                        // only the remainder may still arrive.
                        entry_limit = size.saturating_sub(offset);
                        entry_written = 0;
                        // The previous entry is finished by the arrival of the
                        // next header when a sender omits `FileEnd`, so its
                        // flush — and its rename — are as load-bearing here as
                        // they are there.
                        finish_entry(current.take(), current_paths.take(), storage).await?;
                        // A path with a `..` or a NUL in it is a violation no
                        // conforming sender commits — but it is skipped like
                        // any other entry that cannot land, not aborted.
                        // Nothing is written either way (no destination was
                        // even computed), so refusing the whole folder buys no
                        // safety and costs the user every remaining file; the
                        // warning is the record that it happened.
                        let Some(dp) = dest_path(dest_dir, &root, &path) else {
                            tracing::warn!("skipping folder entry with unsafe path {path}");
                            current = None;
                            current_skipped = true;
                            continue;
                        };
                        // Two entries of this folder that differ only in case
                        // are two files on the sender and one destination here,
                        // so the second is given a free name instead of landing
                        // on the first. Only entries *this* receive wrote are
                        // considered: an unrelated file already on disk is still
                        // overwritten, which is what a folder receive has always
                        // done and what resume-less delivery means.
                        let dp = if written.insert(dp.to_lowercase()) {
                            dp
                        } else {
                            match free_destination(storage, &dp).await {
                                Ok(free) => {
                                    tracing::warn!(
                                        "folder entry {path} differs from an earlier entry only \
                                         by case on this filesystem; writing it to {free}"
                                    );
                                    written.insert(free.to_lowercase());
                                    free
                                }
                                Err(e) => {
                                    tracing::warn!("skipping folder entry {path}: {e}");
                                    current = None;
                                    current_skipped = true;
                                    continue;
                                }
                            }
                        };
                        // Always write fresh (create/truncate): folder
                        // receives are never resumed (see module docs), so
                        // anything already at `dp` is overwritten rather than
                        // blind-appended-to. A create/open failure here — a
                        // destination path that collides with an existing
                        // directory, or a filesystem-level type mismatch —
                        // must not abort the whole folder: skip just this
                        // entry and keep going.
                        entry_hash = Sha256::new();
                        // Claimed, not merely named: another live receive may
                        // already be writing this `.part`, and two writers on
                        // one staging file publish a mixture. A name that is
                        // taken yields the next free one, exactly as the
                        // single-file path does.
                        let claimed = match crate::stream::claim_destination(storage, dp).await {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!("skipping folder entry {path}: {e}");
                                None
                            }
                        };
                        let Some((dp, part, claim)) = claimed else {
                            current = None;
                            current_paths = None;
                            current_skipped = true;
                            continue;
                        };
                        match storage.open_write(&part).await {
                            Ok(w) => {
                                current = Some(w);
                                current_paths = Some((part, dp, claim));
                                current_skipped = false;
                            }
                            Err(e) => {
                                tracing::warn!("skipping folder entry {path}: {e}");
                                current = None;
                                current_paths = None;
                                current_skipped = true;
                            }
                        }
                    }
                    FolderMessage::FileEnd { checksum, .. } => {
                        // **Verified before it is published, never after.**
                        // Renaming first and checking second would put a
                        // corrupted file under its real name for as long as the
                        // check took, which is the exact window staging exists
                        // to close. A mismatch leaves the `.part` on disk as
                        // evidence and the destination untouched.
                        //
                        // `checksum: None` is an older sender that cannot tell
                        // us — nothing to verify, and refusing the entry would
                        // punish the user for the peer's age.
                        let digest = to_hex(&std::mem::take(&mut entry_hash).finalize());
                        let mismatch = checksum.as_deref().is_some_and(|want| want != digest);
                        if mismatch {
                            corrupt += 1;
                            let dest = current_paths
                                .as_ref()
                                .map(|(_, d, _)| d.clone())
                                .unwrap_or_default();
                            tracing::warn!(
                                "folder entry {dest} failed its checksum and was not published"
                            );
                            // Closed but deliberately not finalised.
                            current_paths = None;
                            let _ = close_writer(current.take()).await;
                            current_skipped = false;
                            continue;
                        }
                        // Before the count, not after: `files_completed` is
                        // what the caller reports as arrived, and an entry
                        // whose flush failed did not arrive.
                        let landed =
                            finish_entry(current.take(), current_paths.take(), storage).await?;
                        if !current_skipped && landed {
                            files_completed += 1;
                        }
                        current_skipped = false;
                    }
                    FolderMessage::Complete => {
                        finish_entry(current.take(), current_paths.take(), storage).await?;
                        break TransferOutcome::Completed;
                    }
                    // Receiver → sender only. A peer sending us one is confused
                    // rather than hostile; ignoring it costs nothing and is
                    // kinder than aborting a folder that is otherwise arriving.
                    FolderMessage::Received { .. } => continue,
                    FolderMessage::Cancel => {
                        // Logged rather than propagated: this receive is ending
                        // as `Cancelled` either way, so nothing is about to
                        // claim the file arrived — and reporting a storage
                        // error for a transfer the user (or the sender) stopped
                        // would name the wrong cause. A partial file is the
                        // expected outcome of a cancel; a *completed* entry
                        // with a missing tail is what the `?`s above prevent.
                        // Closed, but **not finalised**: a cancelled entry is
                        // incomplete by definition, so it stays a `.part` and
                        // never appears under the name a complete file would.
                        // Nothing to clear — `current_paths` dies with this
                        // `break`, and the entry is published only by
                        // `finish_entry`, which this arm deliberately skips.
                        if let Err(e) = close_writer(current.take()).await {
                            tracing::warn!("cancelled folder receive left a file unflushed: {e}");
                        }
                        break TransferOutcome::Cancelled;
                    }
                    // Cooperative pause: the sender told us it paused/resumed.
                    // Deliberately do NOT call `ctrl.pause()`/`ctrl.resume()`
                    // here — see the identical, more detailed rationale on
                    // `stream::receive_file`'s `Control::Pause` arm: blocking
                    // this loop on a peer-initiated pause would leave us
                    // unable to ever read the sender's matching `Resume`, and
                    // a compliant sender never sends a `Chunk` between its
                    // own `Pause` and `Resume` (see `send_folder`'s
                    // `signal_pause_edge` use), so nothing is lost by just
                    // mirroring the status.
                    FolderMessage::Pause => {
                        emit(
                            progress,
                            &transfer_id,
                            total,
                            done,
                            &root,
                            files_completed,
                            files_total,
                            Direction::Receiving,
                            TransferStatus::Paused,
                        );
                    }
                    FolderMessage::Resume => {
                        emit(
                            progress,
                            &transfer_id,
                            total,
                            done,
                            &root,
                            files_completed,
                            files_total,
                            Direction::Receiving,
                            TransferStatus::Transferring,
                        );
                    }
                    // Unexpected mid-stream; ignore.
                    FolderMessage::Manifest { .. } | FolderMessage::ResumeState { .. } => {}
                },
                _ => {}
            },
            None => {
                return Err(DomainError::Transfer(
                    "link closed before folder completed".into(),
                ))
            }
        }
    };

    // **Answer before reporting.** The sender is blocked on this when the
    // feature was negotiated, and it is what turns its "sent" from "the bytes
    // reached a send buffer" into "the bytes are on the peer's disk". Sent
    // after `finish_entry` has flushed and renamed the last entry, so the claim
    // is true when it is made.
    //
    // A failure to send it is not made fatal to the *receive*: the files are
    // written and renamed, the user has them, and turning that into a failed
    // receive would be a worse lie than the one this closes. The sender learns
    // the truth anyway — it times out and fails its own side.
    if ack && outcome == TransferOutcome::Completed {
        let frame = folder_frame(&FolderMessage::Received {
            files: u64::from(files_completed),
            corrupt,
        });
        if let Err(e) = link.send_frame(frame).await {
            tracing::warn!("folder arrived but the acknowledgement could not be sent: {e}");
        }
    }

    if outcome == TransferOutcome::Completed {
        emit(
            progress,
            &transfer_id,
            total,
            done,
            &root,
            files_total,
            files_total,
            Direction::Receiving,
            TransferStatus::Completed,
        );
    }

    Ok(FolderReceived {
        outcome,
        root,
        files: files_completed as usize,
        bytes: done,
        transfer_id,
    })
}

// ── Helpers ─────────────────────────────────────────────────────

/// If cancelled, send `Cancel` and return the outcome; if paused, block.
async fn cancel_or_pause(
    link: &mut dyn Link,
    ctrl: &TransferControl,
    retries: u32,
) -> Result<Option<TransferOutcome>> {
    if ctrl.is_cancelled() {
        let _ = send_with_retry(link, folder_frame(&FolderMessage::Cancel), retries).await;
        return Ok(Some(TransferOutcome::Cancelled));
    }
    ctrl.wait_while_paused().await;
    if ctrl.is_cancelled() {
        let _ = send_with_retry(link, folder_frame(&FolderMessage::Cancel), retries).await;
        return Ok(Some(TransferOutcome::Cancelled));
    }
    Ok(None)
}

/// Finish the file just written, surfacing a **deferred** write failure.
///
/// A buffered writer accepts every `write_all` and can only discover at flush
/// time that the bytes had nowhere to go — a disk that filled up, a quota, a
/// volume that went away mid-folder. Dropping those two results is how a
/// folder receive reported `Completed` for a file whose tail never landed:
/// the entry was counted, the caller printed a success, and the sender was
/// free to delete its copy. `stream::receive_file` has always propagated both
/// (same `flush: {e}` / `close: {e}` wording), and this is the same failure on
/// the folder path — not a different policy.
///
/// This is deliberately *not* the "skip the entry and warn" treatment given to
/// a destination that cannot be opened at all: there, nothing was written and
/// nothing was claimed. Here the bytes were already accepted and counted.
/// Flush and close the entry's writer, then publish it under its real name.
///
/// **Close first, rename second, and only on success.** The rename is what
/// makes a folder entry appear complete, so it must not happen until the bytes
/// are actually on disk — a flush error leaves the `.part` behind and reports
/// itself rather than publishing a file whose tail never landed.
///
/// `paths` is `None` for an entry that was skipped (an unsafe path, a
/// destination that would not open), which is why the writer and the paths are
/// taken together: there is nothing to publish and nothing to fail about.
///
/// `finalize_replacing`, not `finalize`: a folder receive overwrites, and has
/// always said so. `finalize` picks a free name when the destination exists,
/// which would turn re-receiving a folder into a directory accumulating
/// `f (1).bin` beside the file it was meant to replace.
async fn finish_entry(
    writer: Option<Box<dyn AsyncWrite + Unpin + Send>>,
    // The claim rides along and is dropped at the end of this call, which is
    // what frees the staging name for the next receive.
    paths: Option<(String, String, crate::stream::StagingClaim)>,
    storage: &dyn StorageProvider,
) -> Result<bool> {
    close_writer(writer).await?;
    let Some((part, dest, _claim)) = paths else {
        return Ok(false);
    };
    match storage.finalize_replacing(&part, &dest).await {
        Ok(_) => Ok(true),
        // **One entry that cannot land must not take the folder with it.** A
        // destination that collides with an existing directory used to fail at
        // `open_write` and was skipped with a warning; staging moved that
        // failure to the rename, and propagating it here turned a skipped file
        // into an aborted transfer. Same verdict as before, later in the
        // sequence: warn, do not count it as arrived, keep going.
        //
        // The staged `.part` is deliberately left on disk — it is the evidence
        // that bytes arrived for a file that could not be published, and the
        // single-file path leaves one for the same reason.
        Err(e) => {
            tracing::warn!("folder entry could not be written to {dest}: {e}");
            Ok(false)
        }
    }
}

async fn close_writer(writer: Option<Box<dyn AsyncWrite + Unpin + Send>>) -> Result<()> {
    let Some(mut w) = writer else {
        return Ok(());
    };
    w.flush()
        .await
        .map_err(|e| DomainError::Storage(format!("flush: {e}")))?;
    w.close()
        .await
        .map_err(|e| DomainError::Storage(format!("close: {e}")))
}

async fn recv_resume(link: &mut dyn Link) -> Result<Vec<u64>> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Control => {
                if let FolderMessage::ResumeState { have } = parse_folder(&frame)? {
                    return Ok(have);
                }
            }
            Some(_) => continue,
            None => {
                return Err(DomainError::Transfer(
                    "link closed before resume state".into(),
                ))
            }
        }
    }
}

#[allow(clippy::type_complexity)]
async fn recv_manifest(link: &mut dyn Link) -> Result<(String, String, Vec<FileMeta>)> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Control => {
                if let FolderMessage::Manifest {
                    transfer_id,
                    root,
                    files,
                } = parse_folder(&frame)?
                {
                    return Ok((transfer_id, sanitize_name(&root), files));
                }
            }
            Some(_) => continue,
            None => return Err(DomainError::Transfer("link closed before manifest".into())),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit(
    progress: &UnboundedSender<Progress>,
    transfer_id: &str,
    total: u64,
    done: u64,
    name: &str,
    files_completed: u32,
    files_total: u32,
    direction: Direction,
    status: TransferStatus,
) {
    let _ = progress.send(build_progress(
        transfer_id,
        direction,
        status,
        total.max(done),
        done,
        name,
        files_completed,
        files_total,
    ));
}

/// Base folder name from a path (sanitized), e.g. `/a/b/myfolder` → `myfolder`.
fn base_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string())
}

/// Reduce an arbitrary name to a single safe path component.
fn sanitize_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if base.is_empty() || base == "." || base == ".." {
        "folder".to_string()
    } else {
        // The root is a *directory* this receiver creates, so it needs the
        // same platform check as the files inside it: a folder named `aux`
        // cannot be created on Windows at all, which would fail every entry
        // in the tree rather than one name.
        safe_component(&base).into_owned()
    }
}

/// Build a safe destination path, rejecting traversal in the relative path.
fn dest_path(dest_dir: &str, root: &str, rel: &str) -> Option<String> {
    let safe = sanitize_rel(rel)?;
    Some(format!(
        "{}/{}/{}",
        dest_dir.trim_end_matches('/'),
        root,
        safe
    ))
}

/// Sanitize a relative path: reject empty, absolute, `.` and `..` components,
/// and reduce every surviving component to what this platform can actually
/// write ([`safe_component`]).
///
/// Splits on **both** `/` and `\`: a Windows receiver treats `\` as a path
/// separator, so a peer sending `..\..\etc` would otherwise slip through a
/// `/`-only split as one component and traverse out of the destination when the
/// OS later normalizes it. A component that is `..`, is empty/`.`, or contains a
/// NUL is rejected outright — malformed or hostile on every platform.
///
/// A component that is merely *unwriteable here* is rewritten instead of
/// rejected. This used to reject any `:` on every platform, which cost the
/// whole transfer: `service 14:30:02.log` or `Chapter 1: Intro.md` — ordinary
/// Unix names — aborted the receive and lost every file behind them. On Windows
/// [`safe_component`] maps the `:` to `_`, which disarms a drive marker too
/// (`C:\evil` becomes the contained `C_/evil`), so refusing bought nothing that
/// rewriting does not.
fn sanitize_rel(rel: &str) -> Option<String> {
    let mut parts = Vec::new();
    for comp in rel.split(['/', '\\']) {
        if comp.contains('\0') {
            return None;
        }
        match comp {
            "" | "." => continue,
            ".." => return None,
            c => parts.push(safe_component(c)),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// A destination path under `root` for a `/`-separated relative path.
///
/// Native separators: `rel` arrives in wire form (always `/`), and what comes
/// out is a location on this machine.
fn join(root: &str, rel: &str) -> String {
    peerbeam_domain::local_path(std::path::Path::new(root), rel)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Where the counter goes.** A received file that collides gets ` (n)`
    /// before the extension, so `report.pdf` stays a PDF — the app already does
    /// this when it lands a file beside one of the same name, and two
    /// conventions for one situation would be worse than either.
    #[tokio::test]
    async fn a_free_name_keeps_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let storage = peerbeam_storage_fs::FsStorage::new();
        let taken = dir.path().join("report.pdf");
        std::fs::write(&taken, b"x").unwrap();

        let free = free_destination(&storage, &taken.to_string_lossy())
            .await
            .expect("a free name");
        assert!(free.ends_with(" (1).pdf"), "got {free}");
    }

    /// A dotfile has no extension to insert before: the leading dot names the
    /// file, it does not separate a suffix.
    #[tokio::test]
    async fn a_dotfile_keeps_its_name_whole() {
        let dir = tempfile::tempdir().unwrap();
        let storage = peerbeam_storage_fs::FsStorage::new();
        let taken = dir.path().join(".gitignore");
        std::fs::write(&taken, b"x").unwrap();

        let free = free_destination(&storage, &taken.to_string_lossy())
            .await
            .expect("a free name");
        assert!(free.ends_with(".gitignore (1)"), "got {free}");
    }

    /// And it keeps counting rather than giving up on the first collision.
    #[tokio::test]
    async fn successive_collisions_keep_counting() {
        let dir = tempfile::tempdir().unwrap();
        let storage = peerbeam_storage_fs::FsStorage::new();
        let base = dir.path().join("a.txt");
        std::fs::write(&base, b"x").unwrap();
        std::fs::write(dir.path().join("a (1).txt"), b"x").unwrap();

        let free = free_destination(&storage, &base.to_string_lossy())
            .await
            .expect("a free name");
        assert!(free.ends_with(" (2).txt"), "got {free}");
    }

    /// **The name is split, not the path.** Deciding where the extension begins
    /// by looking at the whole string meant asking whether it ended in `/`,
    /// which is not the separator everywhere: on Windows a dotfile was read as
    /// an extension and came back as ` (1).gitignore`. Built with `join`, so the
    /// path carries whatever separator this platform actually uses and the rule
    /// is checked against that rather than against an assumption about it.
    ///
    /// (`Path::file_name` is platform-dependent by design — a backslash is a
    /// legal character in a Linux filename — so a Windows-shaped string cannot
    /// stand in for a Windows path here. Only the native shape is meaningful.)
    #[test]
    fn a_dotfile_has_no_extension_whatever_the_separator_is() {
        let path = std::path::Path::new("dir").join("sub").join(".gitignore");
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .expect("a file name");
        assert_eq!(name, ".gitignore");
        assert_eq!(
            name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()),
            None,
            "a leading dot names the file, it is not an extension"
        );
    }

    #[test]
    fn folder_message_roundtrips() {
        let msgs = vec![
            FolderMessage::Manifest {
                transfer_id: "t".into(),
                root: "r".into(),
                files: vec![FileMeta {
                    path: "a/b.txt".into(),
                    size: 10,
                }],
            },
            FolderMessage::ResumeState { have: vec![0, 5] },
            FolderMessage::FileHeader {
                index: 1,
                path: "a/b.txt".into(),
                size: 10,
                offset: 5,
            },
            FolderMessage::FileEnd {
                index: 1,
                checksum: None,
            },
            FolderMessage::Complete,
            FolderMessage::Cancel,
            FolderMessage::Pause,
            FolderMessage::Resume,
        ];
        for m in msgs {
            assert_eq!(parse_folder(&folder_frame(&m)).unwrap(), m);
        }
    }

    #[test]
    fn sanitize_rel_rejects_traversal() {
        assert_eq!(sanitize_rel("a/b/c.txt"), Some("a/b/c.txt".to_string()));
        assert_eq!(sanitize_rel("./a//b"), Some("a/b".to_string()));
        assert_eq!(sanitize_rel("../etc/passwd"), None);
        assert_eq!(sanitize_rel("a/../../b"), None);
        assert_eq!(sanitize_rel(""), None);
    }

    #[test]
    fn sanitize_rel_rejects_windows_traversal() {
        // Backslash is a separator on Windows: reject `..` behind it, and treat
        // mixed separators as a real path split (not one opaque component).
        assert_eq!(sanitize_rel(r"..\..\Windows\System32"), None);
        assert_eq!(sanitize_rel(r"a\..\..\b"), None);
        assert_eq!(sanitize_rel(r"a\b\c.txt"), Some("a/b/c.txt".to_string()));
        // A NUL is rejected outright: no platform can hold one in a name.
        assert_eq!(sanitize_rel("a\0b"), None);
    }

    /// A colon is a legal Unix filename character, and rejecting it here used
    /// to abort the whole folder receive — one `service 14:30:02.log` in a
    /// shared folder lost every file behind it. It must survive as one
    /// contained component instead.
    #[test]
    fn sanitize_rel_keeps_a_colon_instead_of_killing_the_transfer() {
        let got = sanitize_rel("logs/service 14:30:02.log").expect("kept, not rejected");
        if cfg!(windows) {
            assert_eq!(got, "logs/service 14_30_02.log");
        } else {
            assert_eq!(got, "logs/service 14:30:02.log");
        }
    }

    /// A drive marker no longer costs the transfer either: it is defanged into
    /// an ordinary component under the destination rather than rejected.
    #[test]
    fn sanitize_rel_contains_a_drive_marker_rather_than_rejecting_it() {
        let got = sanitize_rel(r"C:\evil").expect("contained, not rejected");
        assert!(!got.starts_with('/'), "still relative: {got}");
        assert!(!got.contains(".."), "still contained: {got}");
        if cfg!(windows) {
            assert_eq!(got, "C_/evil");
        } else {
            assert_eq!(got, "C:/evil");
        }
    }

    #[test]
    fn sanitize_name_strips_paths() {
        assert_eq!(sanitize_name("/a/b/folder"), "folder");
        assert_eq!(sanitize_name(".."), "folder");
        assert_eq!(sanitize_name("plain"), "plain");
    }

    /// The root becomes a directory on disk, so a Windows device name there is
    /// as fatal as one on a file — and a Unix root must not be rewritten.
    #[test]
    fn sanitize_name_renames_a_windows_device_root() {
        if cfg!(windows) {
            assert_eq!(sanitize_name("aux"), "aux_");
        } else {
            assert_eq!(sanitize_name("aux"), "aux");
        }
    }

    #[test]
    fn dest_path_composes_and_rejects() {
        assert_eq!(
            dest_path("/out", "root", "sub/f.txt"),
            Some("/out/root/sub/f.txt".to_string())
        );
        assert_eq!(dest_path("/out", "root", "../escape"), None);
    }
}
