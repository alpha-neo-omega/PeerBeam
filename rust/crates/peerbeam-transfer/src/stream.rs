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

use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::{Direction, Progress, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::TransferId;
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};

use crate::control::TransferControl;
use crate::protocol::{
    chunk_frame, control_frame, meta_frame, parse_control, parse_meta, Control, TransferMeta,
};

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
    let mut buf = vec![0u8; req.chunk_size.max(1) as usize];
    let mut sent = offset;

    loop {
        if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
            return Ok(outcome);
        }
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| DomainError::Storage(format!("read chunk: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        send_with_retry(link, chunk_frame(&buf[..n]), retries).await?;
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
    let base = std::path::Path::new(&meta.name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "received.bin".to_string());
    let dest = format!("{}/{}", dest_dir.trim_end_matches('/'), base);

    // Resume from whatever we already have on disk.
    let existing = storage.size(&dest).await?.unwrap_or(0).min(meta.size);
    send_with_retry(
        link,
        control_frame(&Control::ResumeAck { offset: existing }),
        0,
    )
    .await?;

    let mut hasher = Sha256::new();
    let mut writer = if existing > 0 {
        hash_prefix(storage, &dest, existing, &mut hasher).await?;
        storage.open_append(&dest).await?
    } else {
        storage.open_write(&dest).await?
    };
    let mut received = existing;
    let mut integrity_ok = true;

    let outcome = loop {
        if ctrl.is_cancelled() {
            let _ = link.send_frame(control_frame(&Control::Cancel)).await;
            break TransferOutcome::Cancelled;
        }
        match link.recv_frame().await? {
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

    if outcome == TransferOutcome::Completed {
        if !integrity_ok {
            return Err(DomainError::Integrity(format!(
                "checksum mismatch for {base}"
            )));
        }
        let _ = progress.send(make_progress(
            &meta.transfer_id,
            Direction::Receiving,
            TransferStatus::Completed,
            meta.size.max(received),
            received,
            &base,
        ));
    }

    Ok(Received {
        outcome,
        name: base,
        bytes: received,
    })
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

async fn recv_verify(link: &mut dyn Link) -> Result<bool> {
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
