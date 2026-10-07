//! Downloading the newest release: the one sequence behind every surface.
//!
//! `peerbeam download-update` and the app's **Download** button do the same
//! thing in the same order. They refuse a machine that can never be served,
//! ask which release is newest, and fetch it only when it is newer than this
//! build. The order is part of what amendment A3 permits -- a request that
//! cannot lead anywhere is not made -- so it lives here once, where neither
//! surface can drift from it. Each surface decides only how to show progress
//! and how to say how it ended.

use std::path::Path;

use serde_json::{json, Value};

use crate::download::{self, DownloadError, Downloaded};
use crate::{artifact, UpdateError};

/// Every way downloading the newest release can end, decided before any of it
/// is shown.
///
/// Kept apart from showing it so that each ending can be pinned without a
/// network: the CLI maps it to an exit code, and every surface to the JSON in
/// [`DownloadOutcome::to_json`].
#[derive(Debug)]
pub enum DownloadOutcome {
    /// A newer release was fetched, verified, and moved into place.
    Downloaded {
        /// The version that was written.
        latest: String,
        /// Where it was written, and what it is.
        file: Downloaded,
    },
    /// There is nothing newer to fetch. `latest` is `None` when the site
    /// publishes no release at all.
    NothingNewer {
        /// The newest published version, when there is one.
        latest: Option<String>,
    },
    /// The release check did not complete, so nothing was fetched.
    CheckFailed(UpdateError),
    /// Nothing was written. `latest` is `None` when the download was refused
    /// before the release check was made.
    NotWritten {
        /// The newest published version, when the check was made.
        latest: Option<String>,
        /// Why nothing was written.
        why: DownloadError,
    },
}

impl DownloadOutcome {
    /// Nothing went wrong: a verified file was written, or there was nothing
    /// newer to write. Every other ending wrote nothing, and says why.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Downloaded { .. } | Self::NothingNewer { .. })
    }

    /// The ending in the one shape every surface publishes.
    ///
    /// `ok`, `downloaded`, `current` and `latest` are always present, so a
    /// reader can test `downloaded` without first working out which kind of
    /// ending it got; `latest` is `null` when the newest version is not known.
    /// `path`, `name` and `bytes` appear only on the one ending that wrote a
    /// file, and `reason` on every other.
    #[must_use]
    pub fn to_json(&self, current: &str) -> Value {
        let ok = self.is_ok();
        let (latest, reason) = match self {
            Self::Downloaded { latest, file } => {
                return json!({
                    "ok": ok,
                    "downloaded": true,
                    "current": current,
                    "latest": latest,
                    "path": file.path.display().to_string(),
                    "name": file.name,
                    "bytes": file.bytes,
                });
            }
            Self::NothingNewer { latest: Some(v) } => {
                (Some(v.as_str()), "already newest".to_string())
            }
            Self::NothingNewer { latest: None } => (None, "no releases published yet".to_string()),
            Self::CheckFailed(e) => (None, e.to_string()),
            Self::NotWritten { latest, why } => (latest.as_deref(), why.to_string()),
        };
        json!({
            "ok": ok,
            "downloaded": false,
            "current": current,
            "latest": latest,
            "reason": reason,
        })
    }
}

/// Download the newest release for this build into `dir`, reporting progress
/// through `progress(done, total)`.
///
/// Which version is fetched is the release check's answer, never an argument:
/// the set of things worth downloading is "the newest release" and nothing
/// else, so no caller can name an arbitrary version.
pub async fn download_newest<F>(current: &str, dir: &Path, progress: F) -> DownloadOutcome
where
    F: FnMut(u64, Option<u64>),
{
    // First, before anything is asked of the network: can this machine be
    // served at all? For a tarball or source build, Android, or an unknown
    // architecture, no answer from the release check would let it download
    // anything. Every request discloses something (A1 and A3 each record what
    // theirs does), so a request that cannot lead anywhere is not made. It
    // would also make the answer depend on the day: "already newest" while
    // this build is current, and a refusal once it is not.
    //
    // This resolves for the version already running and throws the name away.
    // Whether this machine has a name at all does not depend on the version,
    // and `fetch` resolves again for the version it downloads. On Linux it
    // blocks while dpkg and rpm answer, as `fetch` does a moment later.
    if let Err(why) = artifact::artifact_for_this_build(current) {
        return DownloadOutcome::NotWritten {
            latest: None,
            why: why.into(),
        };
    }

    let release = match crate::check().await {
        Ok(Some(r)) => r,
        Ok(None) => return DownloadOutcome::NothingNewer { latest: None },
        Err(e) => return DownloadOutcome::CheckFailed(e),
    };
    if !crate::is_newer(&release.version, current) {
        return DownloadOutcome::NothingNewer {
            latest: Some(release.version),
        };
    }

    match download::fetch(&release.version, dir, progress).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ResolveError;
    use crate::verify::VerifyError;

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

    fn refused(why: DownloadError) -> DownloadOutcome {
        DownloadOutcome::NotWritten {
            latest: Some("0.13.0".into()),
            why,
        }
    }

    /// The contract both surfaces publish. `downloaded` is true only when a
    /// file was written, and then the JSON says where; every other ending
    /// says why nothing was.
    #[test]
    fn the_json_says_whether_a_file_was_written_and_where() {
        let e = written().to_json("0.12.1");
        assert_eq!(e["ok"], true);
        assert_eq!(e["downloaded"], true);
        assert_eq!(e["current"], "0.12.1");
        assert_eq!(e["latest"], "0.13.0");
        assert_eq!(e["path"], "/tmp/u/peerbeam-0.13.0-amd64.deb");
        assert_eq!(e["name"], "peerbeam-0.13.0-amd64.deb");
        assert_eq!(e["bytes"], 16_384_000);
        assert!(
            e.get("reason").is_none(),
            "a written file needs no reason: {e}"
        );

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
                refused(DownloadError::Unverified(VerifyError::BadSignature(
                    "forged".into(),
                ))),
                false,
            ),
            (
                DownloadOutcome::NotWritten {
                    latest: None,
                    why: DownloadError::Resolve(ResolveError::UnknownLinuxFormat),
                },
                false,
            ),
        ] {
            let e = outcome.to_json("0.12.1");
            assert_eq!(e["ok"], ok, "{outcome:?}");
            assert_eq!(e["downloaded"], false, "{outcome:?}");
            assert_eq!(e["current"], "0.12.1", "{outcome:?}");
            assert!(e.get("latest").is_some(), "latest is always present: {e}");
            assert!(e["path"].is_null(), "nothing written, so no path: {e}");
            assert!(e["reason"].is_string(), "says why nothing was written: {e}");
        }

        // Refused before the release check: the newest version was never
        // asked for, so it is not reported as known.
        let e = DownloadOutcome::NotWritten {
            latest: None,
            why: DownloadError::Resolve(ResolveError::UnknownLinuxFormat),
        }
        .to_json("0.12.1");
        assert!(e["latest"].is_null(), "{e}");
    }
}
