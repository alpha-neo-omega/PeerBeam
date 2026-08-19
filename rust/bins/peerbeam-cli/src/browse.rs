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
                        rel.to_string_lossy().into_owned(),
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

    for a in &actions {
        let want = match a {
            peerbeam_sync::Action::Fetch { path } => Some(path),
            // A conflict is fetched too: keeping both copies means having both.
            peerbeam_sync::Action::Conflict { path, .. } => Some(path),
            _ => None,
        };
        if let Some(rel) = want {
            session_transfer::request_file(&session, &format!("{}/{rel}", args.path)).await;
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
