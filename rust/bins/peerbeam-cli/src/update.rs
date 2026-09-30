//! `peerbeam check-updates` and `peerbeam download-update`.
//!
//! The only requests PeerBeam makes to anything that is not a peer, and neither
//! happens unless someone runs it. Amendments A1, A3 and A4 in
//! `docs/ARCHITECTURAL_INVARIANTS.md` are the terms they are permitted on; the
//! work itself is in `peerbeam-update`, and this is the part a person sees.

use crate::exit::CliResult;
use crate::output::Ctx;

/// Ask the project's site what the newest published version is.
///
/// Deliberately thin: it prints what it was told and stops. Amendment A1 permits
/// a check, not an updater — nothing here downloads, installs, or changes
/// behaviour on the strength of the answer.
///
/// Being unable to reach the feed is **not an error exit**. Offline is an
/// ordinary state for this app, and a script that runs `peerbeam check-updates`
/// on a machine with no route out should not fail because of it; the JSON says
/// `reachable: false` and the human text says so plainly.
pub async fn check_updates(ctx: &Ctx) -> CliResult {
    let current = env!("CARGO_PKG_VERSION");
    // Awaited on the dispatcher's runtime rather than building one. An earlier
    // version called `block_on` here and panicked outright — "Cannot start a
    // runtime from within a runtime" — because `dispatch` is already async.
    let outcome = peerbeam_update::check().await;

    match outcome {
        Ok(Some(release)) => {
            let newer = peerbeam_update::is_newer(&release.version, current);
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": true,
                    "current": current,
                    "latest": release.version,
                    "update_available": newer,
                    "url": release.url,
                }));
            } else if newer {
                ctx.line(&format!(
                    "{} is available — you have {}\n{}",
                    release.version, current, release.url
                ));
            } else {
                ctx.line(&format!("{current} is the newest release"));
            }
            Ok(())
        }
        Ok(None) => {
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": true,
                    "current": current,
                    "latest": serde_json::Value::Null,
                    "update_available": false,
                }));
            } else {
                ctx.line("no releases published yet");
            }
            Ok(())
        }
        Err(e) => {
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": false,
                    "current": current,
                    "reason": e.to_string(),
                }));
            } else {
                ctx.line(&format!(
                    "could not check for updates — {e}\nyou have {current}; see {}",
                    peerbeam_update::DOWNLOAD_PAGE
                ));
            }
            Ok(())
        }
    }
}

/// `peerbeam download-update` — fetch the newest release for this machine.
///
/// Amendment A3's seventh condition: reachable from the CLI, not GUI-only.
/// Everything the GUI can do here, this does, including the refusals.
///
/// Note what it does **not** do. It does not install, unpack, chmod or run
/// anything, and it does not touch the running binary — A3 condition 4. The
/// output is a file path, and installing it is the person's own next step,
/// exactly as with a download from the website.
pub async fn download_update(ctx: &Ctx, args: crate::cli::DownloadUpdateArgs) -> CliResult {
    let current = env!("CARGO_PKG_VERSION");

    // Which version to fetch is the release check's answer, not an argument:
    // a `--version` flag would let someone fetch an arbitrary string, and the
    // set of things worth downloading is "the newest release" and nothing else.
    let release = match peerbeam_update::check().await {
        Ok(Some(r)) => r,
        Ok(None) => {
            ctx.line("no releases published yet");
            return Ok(());
        }
        Err(e) => {
            // Offline is ordinary here — A3 condition 5, carried from A1.
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_download",
                    "ok": false,
                    "reason": e.to_string(),
                }));
            } else {
                ctx.line(&format!(
                    "could not check for updates — {e}\nyou have {current}; see {}",
                    peerbeam_update::DOWNLOAD_PAGE
                ));
            }
            return Ok(());
        }
    };

    if !peerbeam_update::is_newer(&release.version, current) {
        if ctx.json {
            ctx.json_line(&serde_json::json!({
                "event": "update_download",
                "ok": true,
                "downloaded": false,
                "current": current,
                "latest": release.version,
                "reason": "already newest",
            }));
        } else {
            ctx.line(&format!(
                "{current} is the newest release — nothing to fetch"
            ));
        }
        return Ok(());
    }

    let dir = args.to.unwrap_or_else(|| std::path::PathBuf::from("."));

    // Progress to stderr, and only on a terminal: this is a several-tens-of-
    // megabytes fetch and silence for a minute reads as a hang. In --json mode
    // the stream stays machine-readable, so nothing is drawn.
    let quiet = ctx.json || ctx.quiet;
    let mut last_pct = u64::MAX;
    let outcome = peerbeam_update::download::fetch(&release.version, &dir, |done, total| {
        if quiet {
            return;
        }
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = done * 100 / total;
            if pct != last_pct {
                last_pct = pct;
                eprint!("\rdownloading {pct}%");
            }
        }
    })
    .await;
    if !quiet {
        eprintln!();
    }

    match outcome {
        Ok(done) => {
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_download",
                    "ok": true,
                    "downloaded": true,
                    "current": current,
                    "latest": release.version,
                    "path": done.path.display().to_string(),
                    "name": done.name,
                    "bytes": done.bytes,
                }));
            } else {
                ctx.line(&format!(
                    "verified and saved to {}\ninstall it yourself — PeerBeam does not",
                    done.path.display()
                ));
            }
            Ok(())
        }
        Err(e) => {
            // Every refusal is reported and none is fatal to the process: a
            // download that did not happen leaves the person exactly where
            // they were, with a working install and a website that still has
            // the file.
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_download",
                    "ok": false,
                    "downloaded": false,
                    "current": current,
                    "latest": release.version,
                    "reason": e.to_string(),
                }));
            } else {
                ctx.line(&format!(
                    "did not download — {e}\nget it from {} instead",
                    peerbeam_update::DOWNLOAD_PAGE
                ));
            }
            Ok(())
        }
    }
}
