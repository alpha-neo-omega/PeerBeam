//! `peerbeam transfers` — live sessions, and the transfers that were
//! interrupted rather than finished.
//!
//! # What an interrupted transfer is
//!
//! A transfer that ended because the link dropped or the process died leaves a
//! **checkpoint** in `<data_directory>/checkpoints`, recording the peer, the
//! file, its size and how far it got. It is what lets a transfer be picked up
//! later instead of started again.
//!
//! ```text
//!   ID          DIR   PEER              FILE          PROGRESS         AGE
//!   tx-4131-0   out   pb-f4e4d56fce98   movie.mkv     1.2 GB / 4.0 GB  2h
//!   fileref-7   in    pb-9a10c2b40f21   photos.zip    88 MB / 210 MB   1d
//! ```
//!
//! # This machine's checkpoints, whoever wrote them
//!
//! The checkpoint directory belongs to the *machine*, not to a frontend, so
//! `transfers list` on a headless box shows what the app and the daemon left
//! behind and `transfers resume` can pick one up over SSH (I7). The one-shot
//! `peerbeam send` deliberately writes none of its own: it is a foreground
//! command, and re-running it already resumes — the receiver's partial file is
//! what negotiates the offset, not a record on this side.
//!
//! # Direction decides what is possible
//!
//! **Outgoing** can be resumed from here: this side dials and the receiver's
//! on-disk bytes set the offset. **Incoming** cannot — the transfer protocol is
//! sender-driven, and inventing a "please re-send" message would be a wire
//! change. An incoming checkpoint keeps its partial file, its progress and its
//! consent so it continues the moment its sender offers it again; `discard` is
//! what a user does with one they are done waiting for.
//!
//! # Output
//!
//! Human text on stdout, errors on stderr, `--json` emits one object per line.
//! Exit codes are the CLI's usual ones: `2` for a resume the checkpoint refuses
//! (a different peer, a changed file), `3` for an id that matches nothing, `4`
//! when the peer cannot be reached.

use std::sync::Arc;

use serde_json::json;

use peerbeam_config::EngineConfig;
use peerbeam_domain::entity::{Direction, TransferSession};
use peerbeam_domain::id::TransferId;
use peerbeam_domain::port::ReliabilityStore;
use peerbeam_engine::RouteManager;
use peerbeam_reliability_fs::FsReliability;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{check_resume, partial_file, ResumeClaim};
use peerbeam_transfer_quic::QuicTransport;

use crate::cli::TransfersAction;
use crate::commands::{
    chat_store, clamp_chunk_size, human_bytes, load_config, secure_send_file, snapshot, SecureCtx,
};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

/// How long `transfers resume` scans for the peer before giving up.
const DISCOVERY_SECS: u64 = 3;

/// Dispatch `peerbeam transfers`.
pub async fn transfers(
    ctx: &Ctx,
    action: Option<TransfersAction>,
    path_override: Option<&str>,
) -> CliResult {
    match action {
        None => overview(ctx, path_override),
        Some(TransfersAction::List) => list(ctx, path_override),
        Some(TransfersAction::Resume { id }) => resume(ctx, &id, path_override).await,
        Some(TransfersAction::Discard { id }) => discard(ctx, &id, path_override),
    }
}

/// The checkpoint store for this machine's configured data directory.
fn store(config: &EngineConfig) -> FsReliability {
    FsReliability::new(std::path::Path::new(&config.storage.data_directory).join("checkpoints"))
}

fn all(config: &EngineConfig) -> Result<Vec<TransferSession>, CliError> {
    store(config).list_checkpoints().map_err(CliError::from)
}

fn one(config: &EngineConfig, id: &str) -> Result<TransferSession, CliError> {
    store(config)
        .load_checkpoint(&TransferId::from(id))
        .map_err(CliError::from)?
        .ok_or_else(|| CliError::NotFound(format!("interrupted transfer {id}")))
}

fn row_json(cp: &TransferSession) -> serde_json::Value {
    let file = cp.files.first();
    json!({
        "id": cp.id.as_str(),
        "direction": match cp.direction {
            Direction::Sending => "sending",
            Direction::Receiving => "receiving",
        },
        "peer": cp.peer.0,
        "file": file.map(|f| f.name.clone()).unwrap_or_default(),
        "path": file.and_then(|f| f.path.to_str()).unwrap_or_default(),
        "transferred_bytes": cp.transferred_bytes,
        "total_bytes": cp.total_bytes,
        "started_at": cp.started_at.to_rfc3339(),
        // Only an outgoing transfer can be restarted from this side.
        "resumable": cp.direction == Direction::Sending,
    })
}

// ── the bare command ────────────────────────────────────────────────────────

/// `peerbeam transfers` — live sessions, plus what is waiting to be resumed.
///
/// The `sessions`/`transport` half is unchanged, so a script reading it keeps
/// working; `interrupted` is additive. Both belong here: "what is moving" and
/// "what stopped moving" are the same question asked at two moments.
fn overview(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let diag = peerbeam_engine::SessionDiagnostics::new();
    let interrupted: Vec<_> = all(&config)?.iter().map(row_json).collect();
    let value = json!({
        "sessions": diag.sessions_json(),
        "transport": diag.transport_json(),
        "interrupted": interrupted,
    });
    crate::commands::present(ctx, &value)
}

// ── list ────────────────────────────────────────────────────────────────────

/// `peerbeam transfers list` — every interrupted transfer, newest first.
fn list(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let rows = all(&config)?;

    if ctx.json {
        for cp in &rows {
            ctx.json_line(&row_json(cp));
        }
        return Ok(());
    }

    if rows.is_empty() {
        ctx.line(&ctx.dim("no interrupted transfers"));
        return Ok(());
    }

    let mut table: Vec<Vec<String>> = Vec::new();
    for cp in &rows {
        table.push(vec![
            cp.id.as_str().to_string(),
            // `out`/`in` rather than the wire spellings: this column answers
            // "can I resume this?", and three characters keeps the table
            // scannable.
            match cp.direction {
                Direction::Sending => "out".into(),
                Direction::Receiving => "in".into(),
            },
            cp.peer.0.clone(),
            cp.files
                .first()
                .map(|f| f.name.clone())
                .unwrap_or_else(|| "-".into()),
            format!(
                "{} / {}",
                human_bytes(cp.transferred_bytes),
                human_bytes(cp.total_bytes)
            ),
            age(cp.started_at),
        ]);
    }
    ctx.table(&["ID", "DIR", "PEER", "FILE", "PROGRESS", "AGE"], &table);
    ctx.line("");
    ctx.line(&ctx.dim(
        "`out` resumes with `peerbeam transfers resume <ID>`; `in` resumes on its own \
         when its sender offers it again.",
    ));
    Ok(())
}

/// A coarse "how long ago" — a checkpoint's age is the thing that decides
/// whether it is worth resuming, and a timestamp makes the reader do that
/// arithmetic themselves.
fn age(started: chrono::DateTime<chrono::Utc>) -> String {
    let secs = chrono::Utc::now()
        .signed_duration_since(started)
        .num_seconds()
        .max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// ── resume ──────────────────────────────────────────────────────────────────

/// `peerbeam transfers resume <ID>` — dial the peer and continue an outgoing
/// transfer from where it stopped.
///
/// The checkpoint is verified against the file **as it is on disk now**, not
/// against what it remembered: the whole point is that time has passed, and
/// appending the bytes of a file that has since been replaced to a receiver's
/// prefix of the old one would build something that never existed anywhere.
async fn resume(ctx: &Ctx, id: &str, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let cp = one(&config, id)?;

    if cp.direction == Direction::Receiving {
        return Err(CliError::Usage(format!(
            "{id} is an incoming transfer — it resumes when its sender offers it again, \
             or use `peerbeam transfers discard {id}`"
        )));
    }
    let file = cp
        .files
        .first()
        .ok_or_else(|| CliError::Usage(format!("{id} records no file to resume")))?;
    let path = file
        .path
        .to_str()
        .ok_or_else(|| CliError::Usage(format!("{id} records an unreadable path")))?
        .to_string();
    let on_disk = std::fs::metadata(&path)
        .map_err(|e| CliError::NotFound(format!("{path}: {e}")))?
        .len();
    check_resume(
        &cp,
        &ResumeClaim {
            peer: &cp.peer,
            name: &file.name,
            total_bytes: on_disk,
            direction: Direction::Sending,
        },
    )
    .map_err(|why| CliError::Usage(format!("cannot resume {id}: {}", why.message())))?;

    // The peer's addresses come from discovery, never from the checkpoint:
    // where a device can be reached is exactly what changes while a transfer
    // sits interrupted.
    let devices = snapshot(config.clone(), DISCOVERY_SECS).await;
    let target = devices
        .iter()
        .find(|m| m.device.id == cp.peer)
        .map(|m| m.device.clone())
        .ok_or_else(|| CliError::Connection(format!("{} is not reachable", cp.peer.0)))?;
    if target.port == 0 {
        return Err(CliError::NotFound(format!(
            "{} did not advertise a transfer port",
            target.name
        )));
    }

    let sc = SecureCtx::build(&config)?;
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let storage = FsStorage::new();
    let chat = chat_store(&config, &sc.enc, &sc.ident);
    let sink = crate::chat::received_sink(ctx);

    if !ctx.json {
        ctx.line(&ctx.dim(&format!(
            "resuming {} to {} from {}",
            file.name,
            target.name,
            human_bytes(cp.transferred_bytes)
        )));
    }
    // The checkpoint's own id goes on the wire: it is what the receiver
    // correlates its partial file with, and what a surface showing this
    // transfer is keyed by.
    secure_send_file(
        ctx,
        &quic,
        &routes,
        &target,
        &sc,
        &chat,
        &sink,
        &storage,
        &path,
        &file.name,
        file.size,
        clamp_chunk_size(config.transfer.chunk_size),
        Some(cp.id.as_str()),
    )
    .await?;

    // The send returned Ok, so the transfer verified: the record has nothing
    // left to describe.
    store(&config)
        .clear_checkpoint(&cp.id)
        .map_err(CliError::from)?;
    Ok(())
}

// ── discard ─────────────────────────────────────────────────────────────────

/// `peerbeam transfers discard <ID>` — forget a checkpoint and the partial
/// bytes it was holding.
///
/// The partial file goes deliberately. Leaving it would let a transfer the
/// user threw away seed the next one of the same name with a prefix from the
/// one before it — the silent corruption the binding check exists to prevent,
/// arrived at from the other direction.
fn discard(ctx: &Ctx, id: &str, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let cp = one(&config, id)?;
    let mut removed = false;
    if let Some(part) = partial_file(&cp) {
        match std::fs::remove_file(&part) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(CliError::Other(format!("{part}: {e}"))),
        }
    }
    store(&config)
        .clear_checkpoint(&cp.id)
        .map_err(CliError::from)?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "discarded",
            "id": id,
            "partial_removed": removed,
        }));
    } else {
        ctx.line(&ctx.green(&format!("discarded {id}")));
        if removed {
            ctx.line(&ctx.dim("  the partial file was removed too"));
        }
    }
    Ok(())
}
