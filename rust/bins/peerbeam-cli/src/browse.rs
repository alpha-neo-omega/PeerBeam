//! `peerbeam browse` — list what a device shares.

use std::sync::Arc;

use crate::cli::BrowseArgs;
use crate::commands::{self, SecureCtx};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn browse(ctx: &Ctx, args: BrowseArgs, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let devices = commands::snapshot(config.clone(), 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = commands::resolve_peer(ctx, &candidates, &Some(args.peer))?;
    let device = devices[index].device.clone();
    let path = args.path.unwrap_or_default();

    let quic = Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = session_transfer::dial(
        &quic, &routes, &device, "browse", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}: {e}", device.name)))?;

    if !session.supports_browse() {
        session.close().await;
        return Err(CliError::Other(format!(
            "{} is running a build without browsing",
            device.name
        )));
    }

    let answer = session_transfer::request_listing(&session, &path).await;
    session.close().await;
    let Some(answer) = answer else {
        return Err(CliError::Other(format!(
            "{} did not answer about {}",
            device.name,
            if path.is_empty() { "its shares" } else { &path }
        )));
    };

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "path": answer.path,
            "entries": answer.entries.iter().map(|e| serde_json::json!({
                "name": e.name, "is_dir": e.is_dir, "size": e.size,
            })).collect::<Vec<_>>(),
            "truncated": answer.truncated,
            "denied": answer.denied,
        }));
        return Ok(());
    }

    if answer.entries.is_empty() {
        // One sentence for every reason, because the device sent one answer for
        // every reason. Guessing which it was would be inventing information
        // the protocol deliberately withholds.
        ctx.line(&ctx.dim(
            "nothing to show — the device may share nothing here, or may not \
             have granted this machine permission to look",
        ));
        return Ok(());
    }
    for e in &answer.entries {
        if e.is_dir {
            ctx.line(&format!("{}/", ctx.bold(&e.name)));
        } else {
            ctx.line(&format!("{}  {}", e.name, human_size(e.size)));
        }
    }
    if answer.truncated {
        ctx.line(&ctx.dim("…more entries than one listing carries"));
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// `peerbeam sync <PEER> <PATH> <INTO>`.
///
/// Fetches the peer's manifest, works out what this directory is missing or
/// behind on, and asks for those files. The bytes arrive as ordinary inbound
/// transfers — so a `peerbeam receive` or daemon must be running to accept
/// them, exactly as for any other file. Said plainly here because a sync that
/// reported "12 files" and then delivered nothing would be baffling.
/// `peerbeam sync`, once or continuously.
///
/// With `--watch` this runs until interrupted, re-checking on an interval. The
/// **settling rule** is what makes that safe: a file is only acted on once its
/// size and mtime have held still across two consecutive passes, so saving a
/// large file part-way through a poll does not sync a half-written copy — and,
/// more subtly, does not raise the version vector once per observation and
/// manufacture a conflict out of an ordinary save.
pub async fn sync(ctx: &Ctx, args: crate::cli::SyncArgs, path_override: Option<&str>) -> CliResult {
    let Some(interval) = args.watch else {
        return sync_once(ctx, args, path_override).await;
    };
    // Clamped: a zero or one-second poll would rescan and re-hash the folder
    // continuously, costing more than the sync it is trying to notice.
    let period = std::time::Duration::from_secs(interval.max(5));
    ctx.line(&ctx.dim(&format!(
        "watching every {}s — Ctrl-C to stop",
        period.as_secs()
    )));

    let mut settling = peerbeam_sync::Settling::new();
    loop {
        let into = std::path::PathBuf::from(&args.into);
        let observed = observe(&into);
        let settled = settling.observe(&observed);
        if settled.is_empty() && settling.unsettled() > 0 {
            // Something is still being written. Waiting is the whole point.
            tokio::time::sleep(period).await;
            continue;
        }
        if let Err(e) = sync_once(ctx, args.clone(), path_override).await {
            // A failed pass must not end the watch: the peer may simply be
            // asleep, and a watcher that quits on the first unreachable device
            // is one nobody can leave running.
            ctx.line(&ctx.dim(&format!("sync failed, will retry: {e}")));
        }
        tokio::time::sleep(period).await;
    }
}

/// Fetch one file by chunks, writing it locally. Returns bytes saved, or `None`
/// if delta transfer could not be used and the caller should ask for the whole
/// file instead.
///
/// **Falls back rather than fails.** Every reason this can give up — the peer
/// has no chunk map, a chunk never arrived, the rebuilt bytes did not verify —
/// is a reason to send the file the slow way, not a reason for the sync to stop.
async fn fetch_by_delta(
    session: &session_transfer::Session,
    index: &peerbeam_sync::SyncIndex,
    folder: &str,
    into: &std::path::Path,
    remote_path: &str,
    write_to: &str,
) -> Option<u64> {
    let answer = session_transfer::request_chunk_map(session, remote_path).await?;
    if answer.denied || answer.chunks.is_empty() {
        return None;
    }
    let map = peerbeam_sync::ChunkMap {
        path: remote_path.to_string(),
        chunks: answer.chunks,
    };

    let have = index.chunks().have(folder);
    let need = peerbeam_sync::plan_delta(&map, &have);
    let fetched = if need.fetch.is_empty() {
        std::collections::HashMap::new()
    } else {
        session_transfer::request_chunks(session, remote_path, &need.fetch).await
    };

    let local = index.load(folder).ok()?;
    let rebuilt = peerbeam_sync::reassemble(&map, |h| {
        // What just arrived, else what is already on disk somewhere in this
        // folder. `reassemble` verifies every chunk either way, so a stale
        // local file cannot corrupt the result.
        fetched
            .get(h)
            .cloned()
            .or_else(|| index.chunks().read(folder, into, &local, h))
    })?;

    let dest = into.join(write_to);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&dest, &rebuilt).ok()?;
    Some(need.reuse_bytes)
}

/// Every file under `root`, as the settling rule wants to see it.
fn observe(root: &std::path::Path) -> Vec<(String, peerbeam_sync::Observed)> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<(String, peerbeam_sync::Observed)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                walk(root, &path, out);
            } else if meta.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push((
                        peerbeam_domain::wire_path(rel),
                        peerbeam_sync::Observed {
                            size: meta.len(),
                            modified: meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map_or(0, |d| d.as_secs() as i64),
                        },
                    ));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

async fn sync_once(
    ctx: &Ctx,
    args: crate::cli::SyncArgs,
    path_override: Option<&str>,
) -> CliResult {
    let into = std::path::PathBuf::from(&args.into);
    if !into.is_dir() {
        return Err(CliError::NotFound(format!(
            "{} is not a directory",
            args.into
        )));
    }
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let devices = commands::snapshot(config.clone(), 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = commands::resolve_peer(ctx, &candidates, &Some(args.peer))?;
    let device = devices[index].device.clone();

    let quic = Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = session_transfer::dial(
        &quic, &routes, &device, "sync", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}: {e}", device.name)))?;

    if !session.supports_sync() {
        session.close().await;
        return Err(CliError::Other(format!(
            "{} is running a build without folder sync",
            device.name
        )));
    }

    let manifest = session_transfer::request_manifest(&session, &args.path).await;
    let Some(manifest) = manifest else {
        session.close().await;
        return Err(CliError::Other(format!(
            "{} did not answer about {}",
            device.name, args.path
        )));
    };
    if manifest.denied {
        session.close().await;
        // One sentence for every reason, as browsing does: the device sent one
        // answer for all of them.
        return Err(CliError::Other(format!(
            "{} is not sharing {} with this device — it may share nothing \
             there, or may not have granted permission",
            device.name, args.path
        )));
    }

    // Rescan first: a file edited in an editor is exactly as real as one
    // received, and a sync that ignored it would overwrite the user's work with
    // the peer's older copy.
    let index = commands::sync_index(&config, &sc.enc, &sc.ident);
    index
        .rescan(&args.path, &into)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let local = index
        .load(&args.path)
        .map_err(|e| CliError::Other(e.to_string()))?;

    let remote: Vec<peerbeam_sync::RemoteFile> = manifest
        .files
        .iter()
        .map(|f| peerbeam_sync::RemoteFile {
            path: f.path.clone(),
            size: f.size,
            version: f.version.clone(),
            content: f.content.clone(),
            deleted: f.deleted,
        })
        .collect();
    let actions = peerbeam_sync::reconcile(&local, &remote, &device.id.0);
    let remote_versions: std::collections::BTreeMap<String, peerbeam_sync::VersionVector> = remote
        .iter()
        .map(|f| (f.path.clone(), f.version.clone()))
        .collect();
    let outcome = peerbeam_sync::apply_local(&index, &args.path, &into, &actions, &remote_versions)
        .map_err(|e| CliError::Other(e.to_string()))?;

    let mut delta_saved: u64 = 0;
    for a in &actions {
        // A conflict is fetched too — keeping both copies means having both —
        // but under its conflict name, so the local file is never touched.
        let (want, write_to) = match a {
            peerbeam_sync::Action::Fetch { path } => (Some(path), path.clone()),
            peerbeam_sync::Action::Conflict { path, keep_as } => (Some(path), keep_as.clone()),
            _ => (None, String::new()),
        };
        let Some(rel) = want else { continue };
        let remote_path = format!("{}/{rel}", args.path);

        match fetch_by_delta(&session, &index, &args.path, &into, &remote_path, &write_to).await {
            Some(saved) => delta_saved += saved,
            // No chunk map, a missing chunk, or an unwritable file: ask for the
            // whole thing. Slower, and always correct.
            None => {
                session_transfer::request_file(&session, &remote_path).await;
            }
        }
    }
    session.close().await;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "fetching": outcome.fetching,
            "renamed": outcome.renamed,
            "pushing": outcome.pushing,
            "deleted": outcome.deleted,
            "conflicts": outcome.conflicts,
            "truncated": manifest.truncated,
        }));
        return Ok(());
    }
    ctx.line(&format!(
        "{} to fetch, {} to send, {} deleted, {} moved",
        ctx.bold(&outcome.fetching.to_string()),
        outcome.pushing,
        outcome.deleted,
        outcome.renamed
    ));
    if !outcome.conflicts.is_empty() {
        // Named individually, not counted. A conflict is a decision the user
        // now has to make, and "3 conflicts" tells them nothing about where.
        ctx.line(&format!(
            "{} — your version is untouched; theirs arrives as:",
            ctx.bold("conflicts")
        ));
        for name in &outcome.conflicts {
            ctx.line(&format!("  {name}"));
        }
    }
    if delta_saved > 0 {
        ctx.line(&ctx.dim(&format!(
            "{} reused from what you already had",
            commands::human_bytes(delta_saved)
        )));
    }
    if outcome.fetching > 0 || !outcome.conflicts.is_empty() {
        ctx.line(&ctx.dim(
            "incoming files arrive as ordinary transfers — `peerbeam receive` \
             or the daemon must be running to accept them",
        ));
    }
    if manifest.truncated {
        ctx.line(&ctx.dim("the folder has more files than one manifest carries"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
