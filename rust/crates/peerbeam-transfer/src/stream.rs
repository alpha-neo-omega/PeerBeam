//! Streaming, chunked file send/receive over a [`Link`] with resume and
//! integrity verification.
//!
//! Memory is bounded to one chunk buffer per direction regardless of file
//! size — nothing is ever fully loaded. The send loop honours pause and
//! cancel between chunks and retries transient link errors.
//!
//! Each transfer negotiates a resume offset (the receiver reports how many
//! bytes it already has) and verifies a whole-file SHA-256 at the end:
//!
//! ```text
//! Meta(name,size,chunk_size)   S→R
//! ResumeAck(offset)            R→S
//! Chunk … Chunk                S→R   (streamed from offset)
//! Complete(checksum)           S→R
//! Verify(ok)                   R→S
//! ```

use std::borrow::Cow;
use std::time::Duration;

use bytes::Bytes;
use futures::io::AsyncRead;
use futures::{AsyncReadExt, AsyncWriteExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::{Direction, Progress, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::TransferId;
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};

use crate::control::TransferControl;
use crate::protocol::{
    chunk_frame_owned, control_frame, meta_frame, parse_control, parse_meta, Control, TransferMeta,
};

/// Suffix of the file a partial receive is written to. The final name only
/// appears once the whole file has arrived and verified, so a `.part` is never
/// something the user believes they already have — which is what makes it safe
/// for [`crate::checkpoint`] to reclaim one whose transfer will never resume.
pub const PART_SUFFIX: &str = ".part";

/// Where a partial receive of `dest` is written. The single definition, shared
/// with [`crate::checkpoint`] so the receive path and the disposal path cannot
/// drift onto two different files.
#[must_use]
pub fn part_path(dest: &str) -> String {
    format!("{dest}{PART_SUFFIX}")
}

/// Base backoff between retry attempts (grows linearly with attempts).
const RETRY_BACKOFF: Duration = Duration::from_millis(20);

/// Buffer size used when hashing an already-present prefix on resume.
const HASH_BUF: usize = 64 * 1024;

/// How a transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The whole file was transferred and verified.
    Completed,
    /// Aborted via [`TransferControl::cancel`] or a peer `Cancel`.
    Cancelled,
}

/// Parameters for sending one file.
#[derive(Debug, Clone)]
pub struct SendRequest {
    /// Unique transfer id (echoed into progress).
    pub transfer_id: String,
    /// File name presented to the receiver.
    pub name: String,
    /// Local source path.
    pub path: String,
    /// Total size in bytes (for progress; `0` if unknown).
    pub size: u64,
    /// Chunk size in bytes — bounds memory and framing granularity.
    pub chunk_size: u32,
}

/// Result of receiving a file.
#[derive(Debug, Clone)]
pub struct Received {
    /// How it ended.
    pub outcome: TransferOutcome,
    /// The (sanitized) file name written.
    pub name: String,
    /// Bytes written to disk.
    pub bytes: u64,
    /// The sender's transfer id, from the wire meta — lets a caller correlate
    /// this receive with an out-of-band reference such as a chat FileRef.
    pub transfer_id: String,
}

/// Send a file over `link`, resuming from the receiver's offset and streaming
/// from `storage` in `chunk_size` pieces.
///
/// Emits a [`Progress`] per chunk. Checks `ctrl` each chunk: blocks while
/// paused, and on cancel sends a best-effort `Cancel` and returns
/// [`TransferOutcome::Cancelled`]. Each frame send is retried up to `retries`
/// times. Returns [`DomainError::Integrity`] if the receiver reports a
/// checksum mismatch.
pub async fn send_file(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    req: SendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
) -> Result<TransferOutcome> {
    let meta = TransferMeta {
        transfer_id: req.transfer_id.clone(),
        name: req.name.clone(),
        size: req.size,
        chunk_size: req.chunk_size,
    };
    send_with_retry(link, meta_frame(&meta), retries).await?;

    // The receiver tells us how much it already has.
    let offset = recv_resume_ack(link).await?.min(req.size);

    // The whole-file hash must cover 0..end. Seed it with the already-present
    // prefix (read once) so a resumed send still produces the full checksum.
    let mut hasher = Sha256::new();
    if offset > 0 {
        hash_prefix(storage, &req.path, offset, &mut hasher).await?;
    }

    let mut reader = storage.open_read(&req.path, offset).await?;
    let chunk = req.chunk_size.max(1) as usize;
    let mut sent = offset;

    // Cooperative pause: edge-detect our own pause state and tell the
    // receiver on the main stream, so a sender-initiated pause stops the
    // receiver too (rather than only the receiver's transport backpressure
    // stalling once our chunks stop arriving). Fires exactly once per pause
    // edge and once per resume edge — never polled or retried — so this
    // cannot loop.
    let mut signalled_pause = false;

    loop {
        signal_pause_edge(
            link,
            ctrl,
            &mut signalled_pause,
            || control_frame(&Control::Pause),
            || control_frame(&Control::Resume),
        )
        .await;
        if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
            return Ok(outcome);
        }
        // Fresh owned buffer per chunk, read-filled to full `chunk` size (short
        // reads coalesced). It is moved straight into the frame — no per-chunk
        // copy on the hot path.
        let mut buf = vec![0u8; chunk];
        let n = read_fill(reader.as_mut(), &mut buf).await?;
        if n == 0 {
            break;
        }
        buf.truncate(n);

        // Meter *after* reading and *before* sending: the wait must delay the
        // wire, not the disk, and it must cover the bytes actually read rather
        // than the chunk size asked for — a short read at end-of-file should
        // not be charged for a full chunk. Zero-cost when unlimited, which is
        // the default and the common case.
        let wait = ctrl.throttle(n as u64);
        if !wait.is_zero() {
            // `cancel_or_pause` is checked again on the next iteration, so a
            // cancel arriving mid-wait is honoured one chunk later at worst —
            // and the wait is bounded by one chunk's worth of budget.
            tokio::time::sleep(wait).await;
        }

        hasher.update(&buf);
        send_with_retry(link, chunk_frame_owned(Bytes::from(buf)), retries).await?;
        sent += n as u64;
        let _ = progress.send(make_progress(
            &req.transfer_id,
            Direction::Sending,
            TransferStatus::Transferring,
            req.size.max(sent),
            sent,
            &req.name,
        ));
    }

    let checksum = to_hex(&hasher.finalize());
    send_with_retry(
        link,
        control_frame(&Control::Complete { checksum }),
        retries,
    )
    .await?;

    match recv_verify(link).await? {
        true => {
            let _ = progress.send(make_progress(
                &req.transfer_id,
                Direction::Sending,
                TransferStatus::Completed,
                req.size.max(sent),
                sent,
                &req.name,
            ));
            Ok(TransferOutcome::Completed)
        }
        false => Err(DomainError::Integrity(
            "receiver reported checksum mismatch".into(),
        )),
    }
}

/// Receive a file over `link`, streaming to `dest_dir` in `storage`, resuming
/// from any partial file already on disk and verifying the SHA-256 at the end.
pub async fn receive_file(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
) -> Result<Received> {
    let meta = recv_meta(link).await?;

    // Sanitize: only the base name, never an attacker-chosen path.
    let base = sanitize_file_name(&meta.name);
    // `local_path`, not `format!("{dir}/{base}")`: this string is stored as the
    // file's `local_path` and shown as the tap-to-open target, so it must use
    // this machine's separator. Gluing on a `/` yields `C:\dir/file` on Windows
    // — which opens, but is not a path anyone can render or split sensibly.
    let dest = peerbeam_domain::local_path(std::path::Path::new(dest_dir), &base)
        .to_string_lossy()
        .into_owned();
    // Data is written to a `.part` file; the final name only appears once the
    // whole file is received and verified (safe, atomic, no partial clobber).
    //
    // TODO(transfer): the `.part` name is derived from the destination name
    // alone (no size/hash binding), so two different transfers that resolve
    // to the same destination could in principle share a `.part`. Out of
    // scope for this fix.
    let part = part_path(&dest);

    // Resume from whatever the in-progress `.part` already holds.
    let existing = storage.size(&part).await?.unwrap_or(0).min(meta.size);
    send_with_retry(
        link,
        control_frame(&Control::ResumeAck { offset: existing }),
        0,
    )
    .await?;

    let mut hasher = Sha256::new();
    let mut writer = if existing > 0 {
        hash_prefix(storage, &part, existing, &mut hasher).await?;
        storage.open_append(&part).await?
    } else {
        storage.open_write(&part).await?
    };
    let mut received = existing;
    let mut integrity_ok = true;

    // Cooperative pause: edge-detect a *local* pause on our own `ctrl` (this
    // side's own user pausing the receive — never set by a peer `Pause`
    // frame; see the `Control::Pause` arm below for why) and mirror it to
    // the sender over the progress back-channel via a `Progress{status:
    // Paused}` message. `drive()` (in `peerbeam-ffi`) turns that into the
    // actual back-channel sentinel; this crate only knows about `Progress`,
    // not the raw channel. Fires exactly once per edge, so it cannot loop.
    let mut signalled_pause = false;

    let outcome = loop {
        if ctrl.is_cancelled() {
            let _ = link.send_frame(control_frame(&Control::Cancel)).await;
            break TransferOutcome::Cancelled;
        }

        if ctrl.is_paused() && !signalled_pause {
            signalled_pause = true;
            let _ = progress.send(make_progress(
                &meta.transfer_id,
                Direction::Receiving,
                TransferStatus::Paused,
                meta.size.max(received),
                received,
                &base,
            ));
        } else if !ctrl.is_paused() && signalled_pause {
            signalled_pause = false;
            let _ = progress.send(make_progress(
                &meta.transfer_id,
                Direction::Receiving,
                TransferStatus::Transferring,
                meta.size.max(received),
                received,
                &base,
            ));
        }

        // Honor a receiver-side pause: stop draining frames (transport
        // backpressure stalls the sender) and stop writing. wait_while_paused
        // is a no-op when not paused and also returns on cancel, which the
        // biased cancelled() branch of the select below then handles.
        ctrl.wait_while_paused().await;

        // Race the next frame against cancellation: a plain check at the top
        // of the loop only fires between frames, so a sender that stalls mid
        // transfer would otherwise leave this parked on `recv_frame` forever
        // even after the caller cancels. `cancelled()` re-checks around the
        // same `Notify` `wait_while_paused` uses, so it wakes promptly.
        let frame = tokio::select! {
            biased;
            _ = ctrl.cancelled() => {
                let _ = link.send_frame(control_frame(&Control::Cancel)).await;
                break TransferOutcome::Cancelled;
            }
            frame = link.recv_frame() => frame?,
        };

        match frame {
            Some(frame) => match frame.kind {
                FrameKind::Chunk => {
                    writer
                        .write_all(&frame.payload)
                        .await
                        .map_err(|e| DomainError::Storage(format!("write chunk: {e}")))?;
                    hasher.update(&frame.payload);
                    received += frame.payload.len() as u64;
                    let _ = progress.send(make_progress(
                        &meta.transfer_id,
                        Direction::Receiving,
                        TransferStatus::Transferring,
                        meta.size.max(received),
                        received,
                        &base,
                    ));
                }
                FrameKind::Control => match parse_control(&frame)? {
                    Control::Complete { checksum } => {
                        integrity_ok = to_hex(&hasher.clone().finalize()) == checksum;
                        let _ = send_with_retry(
                            link,
                            control_frame(&Control::Verify { ok: integrity_ok }),
                            0,
                        )
                        .await;
                        break TransferOutcome::Completed;
                    }
                    Control::Cancel => break TransferOutcome::Cancelled,
                    // Cooperative pause: the sender told us it paused/resumed.
                    // Deliberately do NOT call `ctrl.pause()`/`ctrl.resume()`
                    // here — this loop's *own* pause handling
                    // (`wait_while_paused` above) fully stops draining
                    // `recv_frame`, which is right for a *local* pause (it's
                    // what backpressures an uncooperative/non-QUIC sender),
                    // but would be wrong here: if we blocked on a
                    // peer-initiated pause too, we could never read the
                    // sender's matching `Control::Resume` — a deadlock, since
                    // that frame arrives on the very stream we'd have stopped
                    // reading, with nothing local left to wake us. A
                    // compliant sender (see `send_file`'s `signal_pause_edge`)
                    // never sends a `Chunk` between its own `Pause` and
                    // `Resume`, so simply not blocking loses nothing — the
                    // status just needs mirroring to the UI/back-channel.
                    Control::Pause => {
                        let _ = progress.send(make_progress(
                            &meta.transfer_id,
                            Direction::Receiving,
                            TransferStatus::Paused,
                            meta.size.max(received),
                            received,
                            &base,
                        ));
                    }
                    Control::Resume => {
                        let _ = progress.send(make_progress(
                            &meta.transfer_id,
                            Direction::Receiving,
                            TransferStatus::Transferring,
                            meta.size.max(received),
                            received,
                            &base,
                        ));
                    }
                    Control::ResumeAck { .. } | Control::Verify { .. } => {}
                },
                _ => {}
            },
            None => {
                return Err(DomainError::Transfer(
                    "link closed before transfer completed".into(),
                ))
            }
        }
    };

    writer
        .flush()
        .await
        .map_err(|e| DomainError::Storage(format!("flush: {e}")))?;
    writer
        .close()
        .await
        .map_err(|e| DomainError::Storage(format!("close: {e}")))?;

    // On a verified completion, atomically promote `.part` to its final,
    // non-colliding name. On integrity failure or cancel, the `.part` stays
    // on disk (resumable) and the final file is never created/clobbered.
    let final_name = if outcome == TransferOutcome::Completed {
        if !integrity_ok {
            // A poisoned `.part` must not survive a failed integrity check:
            // resume logic re-hashes whatever prefix is on disk, so leaving
            // corrupt bytes here would make this file permanently
            // undeliverable (every retry "resumes" from the bad data and
            // fails again). The writer above is already flushed and closed,
            // so removing the file is safe. Best-effort: if this fails, the
            // Integrity error below still surfaces so the caller can retry
            // or the user can intervene manually.
            if let Err(e) = tokio::fs::remove_file(&part).await {
                tracing::warn!("failed to remove poisoned .part {part}: {e}");
            }
            return Err(DomainError::Integrity(format!(
                "checksum mismatch for {base}"
            )));
        }
        let final_path = storage.finalize(&part, &dest).await?;
        let name = std::path::Path::new(&final_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| base.clone());
        let _ = progress.send(make_progress(
            &meta.transfer_id,
            Direction::Receiving,
            TransferStatus::Completed,
            meta.size.max(received),
            received,
            &name,
        ));
        name
    } else {
        base
    };

    Ok(Received {
        outcome,
        name: final_name,
        bytes: received,
        transfer_id: meta.transfer_id,
    })
}

/// Edge-detect a local pause/resume transition on `ctrl` and tell the peer
/// about it on the main stream, exactly once per edge (tracked in
/// `signalled`). Generic over the frame builders so both the single-file
/// protocol (`Control::Pause`/`Resume`) and the folder protocol
/// (`FolderMessage::Pause`/`Resume`) share this one edge-detector instead of
/// duplicating the branch logic. Best-effort: a dropped `Pause`/`Resume`
/// frame only costs the peer a slightly later reaction (it still stops on
/// its own transport backpressure/cancel checks), so send errors are
/// ignored here rather than aborting the transfer. Called every loop
/// iteration but only ever sends on the actual transition, so it cannot loop.
pub(crate) async fn signal_pause_edge(
    link: &mut dyn Link,
    ctrl: &TransferControl,
    signalled: &mut bool,
    pause_frame: impl FnOnce() -> Frame,
    resume_frame: impl FnOnce() -> Frame,
) {
    if ctrl.is_paused() && !*signalled {
        *signalled = true;
        let _ = link.send_frame(pause_frame()).await;
    } else if !ctrl.is_paused() && *signalled {
        *signalled = false;
        let _ = link.send_frame(resume_frame()).await;
    }
}

/// If cancelled, send `Cancel` and return the outcome; if paused, block.
async fn cancel_or_pause(
    link: &mut dyn Link,
    ctrl: &TransferControl,
    retries: u32,
) -> Result<Option<TransferOutcome>> {
    if ctrl.is_cancelled() {
        let _ = send_with_retry(link, control_frame(&Control::Cancel), retries).await;
        return Ok(Some(TransferOutcome::Cancelled));
    }
    ctrl.wait_while_paused().await;
    if ctrl.is_cancelled() {
        let _ = send_with_retry(link, control_frame(&Control::Cancel), retries).await;
        return Ok(Some(TransferOutcome::Cancelled));
    }
    Ok(None)
}

/// Reduce a sender-supplied file name to the single, safe path component the
/// receiver will actually write under `dest_dir`.
///
/// The name comes straight off the wire, so it is attacker-controlled: only the
/// base component survives, and a name that has no base component at all
/// (empty, `/`, `..`) falls back to a fixed placeholder. Shared with
/// `session::peek_incoming_meta` so the name shown in an approval prompt is
/// *exactly* the name that would land on disk — a preview that sanitized
/// differently would be a prompt that lies about what it is approving.
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "received.bin".to_string());
    safe_component(&base).into_owned()
}

/// The one authority on what a single path component may be *on this
/// platform*. Applied to every peer-supplied name the receiver writes: a
/// single file's destination above, and — via [`crate::folder`] — each
/// component of a folder entry's relative path and the folder's own root name.
///
/// Unix forbids only `/` and NUL in a name, both of which the callers already
/// handle, so a name is passed through untouched there: rewriting
/// `Chapter 1: Intro.md` on a Linux receiver would invent a problem the OS
/// does not have. Windows is the platform with rules, and
/// [`windows_safe_component`] is where they live.
///
/// `cfg!`, not `#[cfg]`: the Windows rules stay compiled — and unit-tested —
/// on every platform, so a Linux test run still catches a break in the code
/// path only Windows users take. The unused branch folds away at compile time.
pub(crate) fn safe_component(name: &str) -> Cow<'_, str> {
    if cfg!(windows) {
        Cow::Owned(windows_safe_component(name))
    } else {
        Cow::Borrowed(name)
    }
}

/// Rewrite one path component into the name Windows will actually create.
///
/// Three Windows behaviours turn a name that is perfectly ordinary on Unix
/// into a problem here, and all three are silent:
///
/// * **Device names.** `File::create("nul.txt")` opens the NUL *device*: every
///   write succeeds, every byte is discarded, the transfer verifies and reports
///   success — and no file exists. The match is on the segment before the first
///   `.`, so `aux.h`, `com1.log` and `prn` are hit too. A Linux peer sending a
///   source tree with an `aux.h` in it is the realistic case, not an attack.
/// * **Illegal characters.** `< > : " | ? *` and the control characters cannot
///   appear in a name at all, so the create simply fails and the file is lost.
/// * **Trailing dots and spaces.** Win32 strips them from a component before
///   resolving it, so `report.` *is* `report`: the name we report to the user
///   is not the name on disk, and in a folder receive (which writes with
///   `open_write`, no collision handling) a sibling `report` is silently
///   truncated by it.
///
/// Every case is **renamed**, never refused. The peer that sent `aux.h` is not
/// an attacker, it is a Linux box with a legal filename; refusing costs the
/// user the file, `aux_.h` costs them one character. Traversal is the opposite
/// case — nothing to preserve — and is still rejected upstream
/// (`folder::sanitize_rel`, and `Path::file_name` for a single file).
///
/// Never returns an empty string for a non-empty input: every rule substitutes
/// or appends, none deletes, so callers keep their existing placeholder logic
/// for the genuinely nameless case.
fn windows_safe_component(name: &str) -> String {
    // Substituting beats deleting for the same reason chat's `display_name`
    // substitutes: a deleted character leaves no evidence it was ever there,
    // and a name deleted down to nothing would need a second fallback here.
    // The two separators are mapped as well, so this function's output is a
    // single component whatever it is handed.
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    // One `_` for the whole trailing run, rather than dropping it: `report.`
    // stays distinct from `report` instead of quietly becoming it. This also
    // disarms `.. ` and `...`, which Win32 trims back to a `..` that climbs
    // out of the destination — `sanitize_rel` only rejects the exact `..`.
    let keep = out.trim_end_matches(['.', ' ']).len();
    if keep < out.len() {
        out.truncate(keep);
        out.push('_');
    }

    // Device check last, on the name as it will finally be written: `nul.txt.`
    // reaches the NUL device (Windows drops that trailing dot), so a check made
    // before the dot was dealt with would wave it through.
    let seg_end = out.find('.').unwrap_or(out.len());
    if is_windows_device(&out[..seg_end]) {
        out.insert(seg_end, '_');
    }
    out
}

/// Does `segment` name a Win32 character device?
///
/// Matched case-insensitively and with a trailing run of spaces ignored,
/// because Win32 does both before resolving a name: `NUL`, `nul` and `nul `
/// are one device, which is why `nul .txt` has to be caught as well.
fn is_windows_device(segment: &str) -> bool {
    let upper = segment.trim_end_matches(' ').to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // The numbered ports: COM0–COM9 and LPT0–LPT9, plus the superscript
    // spellings (`COM¹`) Microsoft documents as resolving to the same devices.
    // Exactly one digit — `com10.log` is an ordinary file.
    let Some(port) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut digits = port.chars();
    matches!(
        (digits.next(), digits.next()),
        (Some('0'..='9' | '¹' | '²' | '³'), None)
    )
}

async fn recv_meta(link: &mut dyn Link) -> Result<TransferMeta> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Meta => return parse_meta(&frame),
            Some(_) => continue,
            None => return Err(DomainError::Transfer("link closed before meta".into())),
        }
    }
}

async fn recv_resume_ack(link: &mut dyn Link) -> Result<u64> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Control => {
                if let Control::ResumeAck { offset } = parse_control(&frame)? {
                    return Ok(offset);
                }
            }
            Some(_) => continue,
            None => {
                return Err(DomainError::Transfer(
                    "link closed before resume ack".into(),
                ))
            }
        }
    }
}

/// Await the peer's `Verify` verdict on a `Complete` we sent, skipping any
/// frame that is not one. Shared with [`crate::pipe`], whose stream ends with
/// the same `Complete`/`Verify` exchange — a pipe having no second
/// implementation of this handshake is the point (I2).
pub(crate) async fn recv_verify(link: &mut dyn Link) -> Result<bool> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Control => match parse_control(&frame)? {
                Control::Verify { ok } => return Ok(ok),
                Control::Cancel => return Err(DomainError::Cancelled),
                _ => continue,
            },
            Some(_) => continue,
            None => return Err(DomainError::Transfer("link closed before verify".into())),
        }
    }
}

/// Read into `buf` until it is full or EOF, coalescing short reads. Returns the
/// number of bytes read (0 only at EOF). Keeps chunk framing at full
/// `chunk_size` even when the underlying reader returns partial reads, cutting
/// frame count and per-chunk overhead.
pub(crate) async fn read_fill(
    reader: &mut (dyn AsyncRead + Unpin + Send),
    buf: &mut [u8],
) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader
            .read(&mut buf[filled..])
            .await
            .map_err(|e| DomainError::Storage(format!("read chunk: {e}")))?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Feed the first `len` bytes of `path` into `hasher` (used to resume a hash).
async fn hash_prefix(
    storage: &dyn StorageProvider,
    path: &str,
    len: u64,
    hasher: &mut Sha256,
) -> Result<()> {
    let mut reader = storage.open_read(path, 0).await?;
    let mut buf = vec![0u8; HASH_BUF];
    let mut remaining = len;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = reader
            .read(&mut buf[..want])
            .await
            .map_err(|e| DomainError::Storage(format!("hash prefix: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

/// Send one frame, retrying transient errors up to `retries` times.
pub(crate) async fn send_with_retry(link: &mut dyn Link, frame: Frame, retries: u32) -> Result<()> {
    let mut attempt = 0u32;
    loop {
        match link.send_frame(frame.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt >= retries {
                    return Err(e);
                }
                attempt += 1;
                tracing::debug!("send retry {attempt}/{retries}: {e}");
                tokio::time::sleep(RETRY_BACKOFF * attempt).await;
            }
        }
    }
}

/// Lowercase hex encoding.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn make_progress(
    transfer_id: &str,
    direction: Direction,
    status: TransferStatus,
    total: u64,
    done: u64,
    name: &str,
) -> Progress {
    let files_completed = u32::from(status == TransferStatus::Completed);
    build_progress(
        transfer_id,
        direction,
        status,
        total,
        done,
        name,
        files_completed,
        1,
    )
}

/// General progress builder shared by single-file and folder transfers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_progress(
    transfer_id: &str,
    direction: Direction,
    status: TransferStatus,
    total: u64,
    done: u64,
    name: &str,
    files_completed: u32,
    files_total: u32,
) -> Progress {
    Progress {
        transfer: TransferId::from(transfer_id),
        direction,
        status,
        total_bytes: total,
        transferred_bytes: done,
        speed_bps: 0.0,
        current_file: Some(name.to_string()),
        files_completed,
        files_total,
        eta_secs: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_file_name_keeps_only_the_base_component() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_file_name(""), "received.bin");
        assert_eq!(sanitize_file_name("/"), "received.bin");
        assert_eq!(sanitize_file_name(".."), "received.bin");
    }

    /// A Windows device name must be renamed, not written: `File::create` on
    /// `nul.txt` opens the NUL device, every write is discarded, and the
    /// transfer reports success for a file that does not exist. The stem is
    /// what matches, so ordinary Unix names (`aux.h`, `com1.log`) are affected.
    #[test]
    fn windows_device_names_are_renamed_not_written_to_the_device() {
        assert_eq!(windows_safe_component("nul.txt"), "nul_.txt");
        assert_eq!(windows_safe_component("aux.h"), "aux_.h");
        assert_eq!(windows_safe_component("com1.log"), "com1_.log");
        assert_eq!(windows_safe_component("LPT9"), "LPT9_");
        assert_eq!(windows_safe_component("NUL"), "NUL_");
        assert_eq!(windows_safe_component("nul.tar.gz"), "nul_.tar.gz");
        // Win32 trims a component's trailing spaces and dots before resolving
        // it, so both of these still reach the device without the rename.
        assert_eq!(windows_safe_component("nul .txt"), "nul _.txt");
        assert_eq!(windows_safe_component("nul.txt."), "nul_.txt_");
        // Names that merely start like a device are left alone.
        assert_eq!(windows_safe_component("nullify.txt"), "nullify.txt");
        assert_eq!(windows_safe_component("com10.log"), "com10.log");
        assert_eq!(windows_safe_component("comment.md"), "comment.md");
    }

    #[test]
    fn windows_illegal_characters_become_underscores() {
        assert_eq!(
            windows_safe_component(r#"a<b>c:d"e|f?g*h"#),
            "a_b_c_d_e_f_g_h"
        );
        assert_eq!(windows_safe_component("tab\there"), "tab_here");
        // Separators too, so the result is always a single component.
        assert_eq!(windows_safe_component(r"a/b\c"), "a_b_c");
    }

    /// Windows strips a trailing run of dots and spaces, so without this the
    /// reported name is not the name on disk and `report.` silently becomes
    /// `report`.
    #[test]
    fn windows_trailing_dots_and_spaces_collapse_to_one_underscore() {
        assert_eq!(windows_safe_component("report."), "report_");
        assert_eq!(windows_safe_component("report "), "report_");
        assert_eq!(windows_safe_component("report. . "), "report_");
        assert_eq!(windows_safe_component("report.txt"), "report.txt");
        assert_eq!(windows_safe_component(".hidden"), ".hidden");
        // `.. ` and `...` are trimmed back to `..` by Win32 — a traversal that
        // `sanitize_rel`'s exact-`..` check does not see.
        assert_eq!(windows_safe_component(".. "), "_");
        assert_eq!(windows_safe_component("..."), "_");
    }

    /// The platform gate itself: a Unix receiver must not rewrite names its
    /// filesystem accepts, and a Windows receiver must.
    #[test]
    fn only_windows_rewrites_a_legal_unix_name() {
        if cfg!(windows) {
            assert_eq!(sanitize_file_name("aux.h"), "aux_.h");
            assert_eq!(
                sanitize_file_name("Chapter 1: Intro.md"),
                "Chapter 1_ Intro.md"
            );
        } else {
            assert_eq!(sanitize_file_name("aux.h"), "aux.h");
            assert_eq!(
                sanitize_file_name("Chapter 1: Intro.md"),
                "Chapter 1: Intro.md"
            );
        }
    }
}
