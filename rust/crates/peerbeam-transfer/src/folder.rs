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
//! **No resume (yet)** — folder transfers are not wired to any resume UI/FFI,
//! so every folder receive is treated as fresh: each file is written with
//! `open_write` (create/truncate), overwriting anything already at the
//! destination path rather than blind-appending onto it (blind-appending onto
//! a same-sized pre-existing file used to silently corrupt it). The wire
//! messages still carry a `have`/`offset` so a future resume feature can slot
//! in without a protocol change, but today the receiver always reports `0`
//! and the sender always streams from the start.
//!
//! TODO(transfer): a proper `.part`-staged folder-resume (mirroring
//! `stream::receive_file`) is a separate future task.
//!
//! A single unreadable source file (send side) or a destination path that
//! collides with an existing directory (receive side) is skipped with a
//! warning rather than aborting the whole transfer.

use futures::io::AsyncWrite;
use futures::AsyncWriteExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::{Direction, Progress, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, FrameKind, Link, StorageProvider};

use crate::control::TransferControl;
use bytes::Bytes;

use crate::protocol::chunk_frame_owned;
use crate::stream::{build_progress, read_fill, send_with_retry, TransferOutcome};

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
    },
    Complete,
    Cancel,
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
}

/// Send a folder recursively over `link`, preserving structure. Skips (with a
/// warning) any source file that fails to open rather than aborting.
pub async fn send_folder(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    req: FolderSendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
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

        if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
            return Ok(outcome);
        }

        // Open the source file BEFORE announcing it: a file that vanished,
        // got locked, or lost read permission between the manifest snapshot
        // above and now must not kill the whole transfer, and the receiver
        // must never see a `FileHeader` for a file we then fail to stream
        // (no phantom/partial entry left behind).
        let src = join(&req.root_path, rel);
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
            if let Some(outcome) = cancel_or_pause(link, ctrl, retries).await? {
                return Ok(outcome);
            }
            let mut buf = vec![0u8; chunk];
            let n = read_fill(reader.as_mut(), &mut buf).await?;
            if n == 0 {
                break;
            }
            buf.truncate(n);
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
            folder_frame(&FolderMessage::FileEnd { index: i as u32 }),
            retries,
        )
        .await?;
        files_completed += 1;
    }

    send_with_retry(link, folder_frame(&FolderMessage::Complete), retries).await?;
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

/// Receive a folder recursively over `link`, into `dest_dir/<root>/…`.
pub async fn receive_folder(
    link: &mut dyn Link,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
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

    let mut current: Option<Box<dyn AsyncWrite + Unpin + Send>> = None;
    // Set whenever the file announced by the most recent `FileHeader` was
    // skipped (unsafe path or a create/open failure, e.g. a destination path
    // that collides with an existing directory) — its `FileEnd` must not be
    // counted as completed.
    let mut current_skipped = false;

    let outcome = loop {
        if ctrl.is_cancelled() {
            let _ = link.send_frame(folder_frame(&FolderMessage::Cancel)).await;
            break TransferOutcome::Cancelled;
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
                        writer
                            .write_all(&frame.payload)
                            .await
                            .map_err(|e| DomainError::Storage(format!("write chunk: {e}")))?;
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
                    FolderMessage::FileHeader { path, .. } => {
                        close_writer(current.take()).await;
                        // Traversal/absolute paths are a security violation
                        // from a peer that should not happen with a
                        // conforming sender — reject outright rather than
                        // silently continuing.
                        let dp = dest_path(dest_dir, &root, &path)
                            .ok_or_else(|| DomainError::Transfer(format!("unsafe path: {path}")))?;
                        // Always write fresh (create/truncate): folder
                        // receives are never resumed (see module docs), so
                        // anything already at `dp` is overwritten rather than
                        // blind-appended-to. A create/open failure here — a
                        // destination path that collides with an existing
                        // directory, or a filesystem-level type mismatch —
                        // must not abort the whole folder: skip just this
                        // entry and keep going.
                        match storage.open_write(&dp).await {
                            Ok(w) => {
                                current = Some(w);
                                current_skipped = false;
                            }
                            Err(e) => {
                                tracing::warn!("skipping folder entry {path}: {e}");
                                current = None;
                                current_skipped = true;
                            }
                        }
                    }
                    FolderMessage::FileEnd { .. } => {
                        close_writer(current.take()).await;
                        if !current_skipped {
                            files_completed += 1;
                        }
                        current_skipped = false;
                    }
                    FolderMessage::Complete => {
                        close_writer(current.take()).await;
                        break TransferOutcome::Completed;
                    }
                    FolderMessage::Cancel => {
                        close_writer(current.take()).await;
                        break TransferOutcome::Cancelled;
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

async fn close_writer(writer: Option<Box<dyn AsyncWrite + Unpin + Send>>) {
    if let Some(mut w) = writer {
        let _ = w.flush().await;
        let _ = w.close().await;
    }
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
        base
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

/// Sanitize a relative path: reject empty, absolute, `.` and `..` components.
///
/// Splits on **both** `/` and `\`: a Windows receiver treats `\` as a path
/// separator, so a peer sending `..\..\etc` would otherwise slip through a
/// `/`-only split as one component and traverse out of the destination when the
/// OS later normalizes it. Any component that is `..`, is empty/`.`, contains a
/// NUL, or carries a drive/`:` marker is rejected.
fn sanitize_rel(rel: &str) -> Option<String> {
    let mut parts = Vec::new();
    for comp in rel.split(['/', '\\']) {
        if comp.contains('\0') || comp.contains(':') {
            return None;
        }
        match comp {
            "" | "." => continue,
            ".." => return None,
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn join(root: &str, rel: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), rel)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            FolderMessage::FileEnd { index: 1 },
            FolderMessage::Complete,
            FolderMessage::Cancel,
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
        // Drive letters / colons and NULs are rejected outright.
        assert_eq!(sanitize_rel(r"C:\evil"), None);
        assert_eq!(sanitize_rel("a\0b"), None);
    }

    #[test]
    fn sanitize_name_strips_paths() {
        assert_eq!(sanitize_name("/a/b/folder"), "folder");
        assert_eq!(sanitize_name(".."), "folder");
        assert_eq!(sanitize_name("plain"), "plain");
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
