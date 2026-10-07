//! `peerbeam check-updates` and `peerbeam download-update`.
//!
//! The only requests PeerBeam makes to anything that is not a peer, and neither
//! happens unless someone runs it. Amendments A1, A3 and A4 in
//! `docs/ARCHITECTURAL_INVARIANTS.md` are the terms they are permitted on; the
//! work itself is in `peerbeam-update`, and this is the part a person sees.

use std::path::Path;

use peerbeam_update::artifact::ResolveError;
use peerbeam_update::download::DownloadError;
use peerbeam_update::newest::{download_newest, DownloadOutcome};

use crate::exit::{CliError, CliResult};
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
///
/// It exits `0` only when nothing is wrong: a verified file was written, or
/// there was nothing newer to write. Every other code means no file was
/// written, and [`exit_status`] says which code means what. So a script's
/// `download-update && install` never reaches the install after a refusal.
pub async fn download_update(ctx: &Ctx, args: crate::cli::DownloadUpdateArgs) -> CliResult {
    let current = env!("CARGO_PKG_VERSION");
    let dir = args.to.unwrap_or_else(|| std::path::PathBuf::from("."));
    let outcome = attempt(ctx, current, &dir).await;
    report(ctx, current, &outcome);
    exit_status(&outcome)
}

/// Do the work, and return how it ended without printing any of it.
///
/// The sequence itself -- refuse a machine that can never be served, ask
/// which release is newest, fetch it only when it is newer -- is
/// [`peerbeam_update::newest::download_newest`], shared with the app's
/// Download button. What is the CLI's own is drawing progress.
async fn attempt(ctx: &Ctx, current: &str, dir: &Path) -> DownloadOutcome {
    // Progress to stderr, and only on a terminal: this is a several-tens-of-
    // megabytes fetch and silence for a minute reads as a hang. In --json mode
    // the stream stays machine-readable, so nothing is drawn.
    let quiet = ctx.json || ctx.quiet;
    let mut last_pct = u64::MAX;
    let mut drew = false;
    let outcome = download_newest(current, dir, |done, total| {
        if quiet {
            return;
        }
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = done * 100 / total;
            if pct != last_pct {
                last_pct = pct;
                drew = true;
                eprint!("\rdownloading {pct}%");
            }
        }
    })
    .await;
    // End the progress line only if one was drawn.
    if drew {
        eprintln!();
    }
    outcome
}

/// Say how it ended: the one `update_download` event under `--json`, or a line
/// for a person.
///
/// A failure prints nothing here. It is said once, on stderr, by the error
/// [`exit_status`] returns, which is where `main` reports every failure. That
/// also means `--quiet` can no longer make a refused download silent.
fn report(ctx: &Ctx, current: &str, outcome: &DownloadOutcome) {
    if ctx.json {
        ctx.json_line(&event(current, outcome));
        return;
    }
    match outcome {
        DownloadOutcome::Downloaded { file, .. } => ctx.line(&format!(
            "verified and saved to {}\ninstall it yourself — PeerBeam does not",
            file.path.display()
        )),
        DownloadOutcome::NothingNewer { latest: Some(_) } => ctx.line(&format!(
            "{current} is the newest release — nothing to fetch"
        )),
        DownloadOutcome::NothingNewer { latest: None } => ctx.line("no releases published yet"),
        DownloadOutcome::CheckFailed(_) | DownloadOutcome::NotWritten { .. } => {}
    }
}

/// The exit code for each ending. This is the contract `docs/CLI.md`
/// documents, and it is what a script branches on.
///
/// `0` means nothing is wrong. Every other code means nothing was written:
///
/// * `4`: the release feed or GitHub could not be reached, or answered with an
///   error. Nothing is known to be wrong, and trying again later may work.
///   Offline stays an ordinary state (A3 condition 5). Nothing retries, nags
///   or waits on it, and a script that can do without the file is free to
///   treat `4` as fine. What a script must not be told is that a file it
///   asked for arrived when it did not, and that is what `0` used to say here.
/// * `5`: refused. The bytes, or the route they took, did not prove to be the
///   project's. That may be an attack, so it never shares a code with being
///   offline (A4 condition 4).
/// * `8`: this machine has no artifact that this command can fetch.
/// * `1`: the file could not be written where it was asked for.
///
/// There is deliberately no catch-all arm. A new way for a download to fail
/// will not compile until someone decides what a script should see for it.
fn exit_status(outcome: &DownloadOutcome) -> CliResult {
    let why = match outcome {
        DownloadOutcome::Downloaded { .. } | DownloadOutcome::NothingNewer { .. } => return Ok(()),
        DownloadOutcome::CheckFailed(e) => {
            return Err(CliError::Connection(format!(
                "{e}; nothing was downloaded — see {}",
                peerbeam_update::DOWNLOAD_PAGE
            )))
        }
        DownloadOutcome::NotWritten { why, .. } => why,
    };
    Err(match why {
        DownloadError::Unreachable { .. } => CliError::Connection(format!(
            "{why}; nothing was downloaded — see {}",
            peerbeam_update::DOWNLOAD_PAGE
        )),
        // A "version" from the feed that is not one is the feed's answer
        // being refused, so it sits with the other refusals and ahead of the
        // `Resolve(_)` arm below, which means "this machine has no artifact".
        DownloadError::Unverified(_)
        | DownloadError::RedirectedOffHost { .. }
        | DownloadError::InsecureRedirect { .. }
        | DownloadError::InsecureHost(_)
        | DownloadError::Resolve(ResolveError::NotAVersion(_)) => {
            CliError::Integrity(format!("{why}; refused, and nothing was written"))
        }
        DownloadError::Resolve(_) | DownloadError::NoHostCompiledIn => {
            CliError::Unavailable(format!(
                "{why}; get it from {} instead",
                peerbeam_update::DOWNLOAD_PAGE
            ))
        }
        DownloadError::Io { .. } => CliError::Other(why.to_string()),
    })
}

/// The one `update_download` event that a `--json` run prints.
///
/// `ok`, `downloaded`, `current` and `latest` are always present, so a script
/// can read `.downloaded` without first working out which kind of ending it
/// got. `latest` is `null` when the newest version is not known. `path`,
/// `name` and `bytes` appear only on the one ending that wrote a file, and
/// `reason` appears on every other ending.
fn event(current: &str, outcome: &DownloadOutcome) -> serde_json::Value {
    // The fields are the shared contract (`DownloadOutcome::to_json`), so the
    // CLI and the app cannot describe one download differently. `ok` there is
    // `DownloadOutcome::is_ok`, and a test pins that it agrees with
    // [`exit_status`] for every ending.
    let mut e = outcome.to_json(current);
    e["event"] = "update_download".into();
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_update::download::{DownloadError as D, Downloaded};
    use peerbeam_update::verify::VerifyError;
    use peerbeam_update::UpdateError;

    /// What `main` turns the command's result into.
    fn exit_code(outcome: &DownloadOutcome) -> i32 {
        match exit_status(outcome) {
            Ok(()) => 0,
            Err(e) => e.code(),
        }
    }

    fn written() -> DownloadOutcome {
        DownloadOutcome::Downloaded {
            latest: "0.13.0".into(),
            file: Downloaded {
                path: "/tmp/u/peerbeam-0.13.0-amd64.deb".into(),
                name: "peerbeam-0.13.0-amd64.deb".into(),
                bytes: 16_384_000,
            },
        }
    }

    /// A newer release exists and this is why none of it was written.
    fn refused(why: D) -> DownloadOutcome {
        DownloadOutcome::NotWritten {
            latest: Some("0.13.0".into()),
            why,
        }
    }

    /// Refused before the release check was made.
    fn refused_first(why: ResolveError) -> DownloadOutcome {
        DownloadOutcome::NotWritten {
            latest: None,
            why: D::Resolve(why),
        }
    }

    /// **The contract `docs/CLI.md` documents.** Every way the command can
    /// end, and the code a script sees for it. `0` means nothing is wrong:
    /// a verified file was written, or there was nothing newer to write. Every
    /// other code means nothing was written, and says why. A signature that did
    /// not verify is `5` and never `4`, because offline is a bad day and a
    /// forged checksum may be an attack. A script has to be able to tell which
    /// one it got (A4 condition 4).
    #[test]
    fn every_way_a_download_can_end_has_its_documented_exit_code() {
        let cases: Vec<(&str, DownloadOutcome, i32)> = vec![
            ("verified file written", written(), 0),
            (
                "already the newest",
                DownloadOutcome::NothingNewer {
                    latest: Some("0.12.1".into()),
                },
                0,
            ),
            (
                "nothing published",
                DownloadOutcome::NothingNewer { latest: None },
                0,
            ),
            (
                "release feed unreachable",
                DownloadOutcome::CheckFailed(UpdateError::Unreachable("dns error".into())),
                4,
            ),
            (
                "release feed unreadable",
                DownloadOutcome::CheckFailed(UpdateError::Unreadable("<!doctype html>".into())),
                4,
            ),
            (
                "GitHub unreachable",
                refused(D::Unreachable {
                    what: "the checksums".into(),
                    why: "timed out".into(),
                }),
                4,
            ),
            (
                "checksums not signed by the project",
                refused(D::Unverified(VerifyError::BadSignature("forged".into()))),
                5,
            ),
            (
                "artifact does not match its signed digest",
                refused(D::Unverified(VerifyError::DigestMismatch {
                    name: "peerbeam-0.13.0-amd64.deb".into(),
                    expected: "aa".into(),
                    actual: "bb".into(),
                })),
                5,
            ),
            (
                "artifact not in the signed list",
                refused(D::Unverified(VerifyError::NotListed(
                    "peerbeam-0.13.0-amd64.deb".into(),
                ))),
                5,
            ),
            (
                "build carries no signing key",
                refused(D::Unverified(VerifyError::NoKeyCompiledIn)),
                5,
            ),
            (
                "redirected off the allowlist",
                refused(D::RedirectedOffHost {
                    host: "evil.example".into(),
                }),
                5,
            ),
            (
                "redirected down to plain http",
                refused(D::InsecureRedirect {
                    host: "github.com".into(),
                }),
                5,
            ),
            (
                "release host is not https",
                refused(D::InsecureHost("http://github.com".into())),
                5,
            ),
            (
                // What the release feed says is newest is the one served
                // value that reaches a URL and a file name. A "version" that
                // is not one is the feed being refused -- not this machine
                // lacking an artifact, which is what 8 tells a script.
                "the feed named something that is not a version",
                refused(D::Resolve(ResolveError::NotAVersion(
                    "99.0.0/../../x".into(),
                ))),
                5,
            ),
            (
                "Linux package format unknown",
                refused_first(ResolveError::UnknownLinuxFormat),
                8,
            ),
            (
                "two package managers claim the binary",
                refused_first(ResolveError::AmbiguousLinuxFormat("Deb, Rpm".into())),
                8,
            ),
            (
                "Android",
                refused_first(ResolveError::UnsupportedOs("android".into())),
                8,
            ),
            (
                "no build for this architecture",
                refused_first(ResolveError::UnsupportedArch),
                8,
            ),
            (
                "build carries no release host",
                refused(D::NoHostCompiledIn),
                8,
            ),
            (
                "--to cannot be written",
                refused(D::Io {
                    path: "/root/updates".into(),
                    why: "permission denied".into(),
                }),
                1,
            ),
        ];
        for (name, outcome, want) in cases {
            assert_eq!(exit_code(&outcome), want, "{name}: {outcome:?}");
            // `--json`'s `ok` comes from `is_ok`, shared with the app; the
            // exit code comes from `exit_status`. They must never disagree.
            assert_eq!(
                exit_code(&outcome) == 0,
                outcome.is_ok(),
                "{name}: the exit code and `ok` disagree"
            );
        }
    }

    /// A script reading `--json` gets the shared download JSON -- whose
    /// shape `peerbeam_update::newest` pins -- named as this command's event.
    #[test]
    fn the_event_is_the_shared_json_named_update_download() {
        for outcome in [
            written(),
            DownloadOutcome::NothingNewer {
                latest: Some("0.12.1".into()),
            },
            refused(D::Unverified(VerifyError::BadSignature("forged".into()))),
            refused_first(ResolveError::UnknownLinuxFormat),
        ] {
            let mut e = event("0.12.1", &outcome);
            assert_eq!(e["event"], "update_download", "{outcome:?}");
            e.as_object_mut().expect("an object").remove("event");
            assert_eq!(e, outcome.to_json("0.12.1"), "{outcome:?}");
        }
    }
}
