//! `peerbeam check-updates` and `peerbeam download-update`.
//!
//! The only requests PeerBeam makes to anything that is not a peer, and neither
//! happens unless someone runs it. Amendments A1, A3 and A4 in
//! `docs/ARCHITECTURAL_INVARIANTS.md` are the terms they are permitted on; the
//! work itself is in `peerbeam-update`, and this is the part a person sees.

use std::path::Path;

use peerbeam_update::download::{DownloadError, Downloaded};
use peerbeam_update::UpdateError;

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

/// Every way `download-update` can end, decided before any of it is printed.
///
/// Kept apart from the printing so that the exit code for each ending can be
/// pinned without a network. That code is the contract a script depends on.
#[derive(Debug)]
enum DownloadOutcome {
    /// A newer release was fetched, verified, and moved into place.
    Downloaded { latest: String, file: Downloaded },
    /// There is nothing newer to fetch. `latest` is `None` when the site
    /// publishes no release at all.
    NothingNewer { latest: Option<String> },
    /// The release check did not complete, so nothing was fetched.
    CheckFailed(UpdateError),
    /// Nothing was written. `latest` is `None` when the download was refused
    /// before the release check was made.
    NotWritten {
        latest: Option<String>,
        why: DownloadError,
    },
}

/// Do the work, and return how it ended without printing any of it.
async fn attempt(ctx: &Ctx, current: &str, dir: &Path) -> DownloadOutcome {
    // First, before anything is asked of the network: can this machine be
    // served at all? For a tarball or source build, Android, or an unknown
    // architecture, no answer from the release check would let it download
    // anything. Every request discloses something (A1 and A3 each record what
    // theirs does), so a request that cannot lead anywhere is not made. It would
    // also make the answer depend on the day: "already newest" while this build
    // is current, and a refusal once it is not.
    //
    // This resolves for the version already running and throws the name away.
    // Whether this machine has a name at all does not depend on the version,
    // and `fetch` resolves again for the version it downloads. On Linux it
    // blocks while dpkg and rpm answer. That is harmless here: nothing else
    // runs on this runtime, and `fetch` does the same a moment later.
    if let Err(why) = peerbeam_update::artifact::artifact_for_this_build(current) {
        return DownloadOutcome::NotWritten {
            latest: None,
            why: why.into(),
        };
    }

    // Which version to fetch is the release check's answer, not an argument:
    // a `--version` flag would let someone fetch an arbitrary string, and the
    // set of things worth downloading is "the newest release" and nothing else.
    let release = match peerbeam_update::check().await {
        Ok(Some(r)) => r,
        Ok(None) => return DownloadOutcome::NothingNewer { latest: None },
        Err(e) => return DownloadOutcome::CheckFailed(e),
    };
    if !peerbeam_update::is_newer(&release.version, current) {
        return DownloadOutcome::NothingNewer {
            latest: Some(release.version),
        };
    }

    // Progress to stderr, and only on a terminal: this is a several-tens-of-
    // megabytes fetch and silence for a minute reads as a hang. In --json mode
    // the stream stays machine-readable, so nothing is drawn.
    let quiet = ctx.json || ctx.quiet;
    let mut last_pct = u64::MAX;
    let fetched = peerbeam_update::download::fetch(&release.version, dir, |done, total| {
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

    match fetched {
        Ok(file) => DownloadOutcome::Downloaded {
            latest: release.version,
            file,
        },
        Err(why) => DownloadOutcome::NotWritten {
            latest: Some(release.version),
            why,
        },
    }
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
        DownloadError::Unverified(_)
        | DownloadError::RedirectedOffHost { .. }
        | DownloadError::InsecureHost(_) => {
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
    // Derived from the exit code rather than decided a second time, so the
    // two cannot disagree.
    let ok = exit_status(outcome).is_ok();
    let (latest, reason) = match outcome {
        DownloadOutcome::Downloaded { latest, file } => {
            return serde_json::json!({
                "event": "update_download",
                "ok": ok,
                "downloaded": true,
                "current": current,
                "latest": latest,
                "path": file.path.display().to_string(),
                "name": file.name,
                "bytes": file.bytes,
            })
        }
        DownloadOutcome::NothingNewer { latest: Some(v) } => {
            (Some(v.as_str()), "already newest".to_string())
        }
        DownloadOutcome::NothingNewer { latest: None } => {
            (None, "no releases published yet".to_string())
        }
        DownloadOutcome::CheckFailed(e) => (None, e.to_string()),
        DownloadOutcome::NotWritten { latest, why } => (latest.as_deref(), why.to_string()),
    };
    serde_json::json!({
        "event": "update_download",
        "ok": ok,
        "downloaded": false,
        "current": current,
        "latest": latest,
        "reason": reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_update::artifact::ResolveError;
    use peerbeam_update::download::DownloadError as D;
    use peerbeam_update::verify::VerifyError;

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
                "release host is not https",
                refused(D::InsecureHost("http://github.com".into())),
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
        }
    }

    /// A script reading `--json` instead of the exit code learns the same
    /// thing. `downloaded` is true only when a file was written, and then the
    /// event says where. `ok` agrees with the exit code.
    #[test]
    fn the_event_says_whether_a_file_was_written_and_where() {
        let e = event("0.12.1", &written());
        assert_eq!(e["event"], "update_download");
        assert_eq!(e["ok"], true);
        assert_eq!(e["downloaded"], true);
        assert_eq!(e["current"], "0.12.1");
        assert_eq!(e["latest"], "0.13.0");
        assert_eq!(e["path"], "/tmp/u/peerbeam-0.13.0-amd64.deb");
        assert_eq!(e["name"], "peerbeam-0.13.0-amd64.deb");
        assert_eq!(e["bytes"], 16_384_000);

        for (outcome, ok) in [
            (
                DownloadOutcome::NothingNewer {
                    latest: Some("0.12.1".into()),
                },
                true,
            ),
            (DownloadOutcome::NothingNewer { latest: None }, true),
            (
                DownloadOutcome::CheckFailed(UpdateError::Unreachable("dns error".into())),
                false,
            ),
            (
                refused(D::Unverified(VerifyError::BadSignature("forged".into()))),
                false,
            ),
            (refused_first(ResolveError::UnknownLinuxFormat), false),
        ] {
            let e = event("0.12.1", &outcome);
            assert_eq!(e["event"], "update_download", "{outcome:?}");
            assert_eq!(e["ok"], ok, "{outcome:?}");
            assert_eq!(e["downloaded"], false, "{outcome:?}");
            assert_eq!(e["current"], "0.12.1", "{outcome:?}");
            assert!(e.get("latest").is_some(), "latest is always present: {e}");
            assert!(e["path"].is_null(), "nothing written, so no path: {e}");
            assert!(e["reason"].is_string(), "says why nothing was written: {e}");
        }

        // Refused before the release check: the newest version was never
        // asked for, so it is not reported as known.
        let e = event("0.12.1", &refused_first(ResolveError::UnknownLinuxFormat));
        assert!(e["latest"].is_null(), "{e}");
    }
}
