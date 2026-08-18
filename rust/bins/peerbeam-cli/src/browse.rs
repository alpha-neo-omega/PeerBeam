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
