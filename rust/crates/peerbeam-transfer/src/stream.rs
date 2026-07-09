//! Streaming, chunked file send/receive over a [`Link`].
//!
//! Memory is bounded to one chunk buffer per direction regardless of file
//! size — nothing is ever fully loaded. The send loop honours pause and
//! cancel between chunks and retries transient link errors.

use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt};
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

/// How a transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The whole file was transferred.
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

/// Send a file over `link`, streaming from `storage` in `chunk_size` pieces.
///
/// Emits a [`Progress`] per chunk. Checks `ctrl` each chunk: blocks while
/// paused, and on cancel sends a best-effort `Cancel` and returns
/// [`TransferOutcome::Cancelled`]. Each frame send is retried up to `retries`
/// times on transient link errors.
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

    let mut reader = storage.open_read(&req.path, 0).await?;
    let mut buf = vec![0u8; req.chunk_size.max(1) as usize];
    let mut sent: u64 = 0;

    loop {
        if ctrl.is_cancelled() {
            let _ = send_with_retry(link, control_frame(&Control::Cancel), retries).await;
            return Ok(TransferOutcome::Cancelled);
        }
        ctrl.wait_while_paused().await;
        if ctrl.is_cancelled() {
            let _ = send_with_retry(link, control_frame(&Control::Cancel), retries).await;
            return Ok(TransferOutcome::Cancelled);
        }

        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| DomainError::Storage(format!("read chunk: {e}")))?;
        if n == 0 {
            break; // EOF
        }

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

    send_with_retry(link, control_frame(&Control::Complete), retries).await?;
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

/// Receive a file over `link`, streaming to `dest_dir` in `storage`.
///
/// Reads the opening [`TransferMeta`], then writes each chunk straight to
/// disk (never buffering the whole file), emitting a [`Progress`] per chunk.
/// Honours `Control::Complete`/`Cancel` and local `ctrl` cancellation.
pub async fn receive_file(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
) -> Result<Received> {
    // Await the opening metadata frame (ignore any stray frames before it).
    let meta = loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Meta => break parse_meta(&frame)?,
            Some(_) => continue,
            None => return Err(DomainError::Transfer("link closed before meta".into())),
        }
    };

    // Sanitize: only the base name, never an attacker-chosen path.
    let base = std::path::Path::new(&meta.name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "received.bin".to_string());
    let dest = format!("{}/{}", dest_dir.trim_end_matches('/'), base);

    let mut writer = storage.open_write(&dest).await?;
    let mut received: u64 = 0;

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
                    Control::Complete => break TransferOutcome::Completed,
                    Control::Cancel => break TransferOutcome::Cancelled,
                    Control::Ack { .. } => {}
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
