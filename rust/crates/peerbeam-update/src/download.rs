//! Fetching a release the user asked for, and refusing to hand over anything
//! unproven.
//!
//! # What this is allowed to be
//!
//! Amendment **A3** in `docs/ARCHITECTURAL_INVARIANTS.md` permits exactly this,
//! on eight binding conditions. Four of them are properties of this file and
//! are named at the code that keeps them:
//!
//! * **Condition 1 — opt-in per use.** Nothing here runs on a timer, at
//!   startup, or on the heels of a check. There is no constructor that starts
//!   anything and no background pre-fetch; a download happens because someone
//!   called [`fetch`].
//! * **Condition 2 — the URL is compiled in, never served.** [`ARTIFACT_HOST`]
//!   is a constant, the path is built from the version and
//!   [`crate::artifact`]'s naming rules, and a redirect that leaves the host is
//!   refused rather than followed. Nothing read from the network chooses where
//!   any byte comes from.
//! * **Condition 3 — verified before it is usable.** The bytes land in a
//!   temporary file, and that file is only ever renamed into place after the
//!   signature over `SHA256SUMS` and the artifact's own digest both check out.
//!   A failure deletes it. There is no flag, argument or prompt that skips
//!   this.
//! * **Condition 4 — never installs, never executes, never elevates.** This
//!   writes one file and stops. It does not unpack it, does not mark it
//!   executable, does not invoke a package manager, and does not touch the
//!   running binary.
//!
//! # Memory
//!
//! The artifact is streamed to disk and hashed as it streams, so a 33 MiB DMG
//! is never held in memory — CLAUDE.md: "No file should ever be fully loaded
//! into RAM." Only `SHA256SUMS` and its signature, both a few kilobytes, are
//! read whole.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::artifact::{self, ResolveError};
use crate::verify::{self, VerifyError};

/// Where release artifacts are served from, scheme and host, no trailing slash.
///
/// **Empty until the project's bucket exists**, and empty means every download
/// refuses — the same fail-closed default as
/// [`crate::verify::SIGNING_PUBLIC_KEY`], for the same reason: a build that
/// ships before the infrastructure does must decline rather than guess.
///
/// # Why not GitHub
///
/// The artifacts live on a GitHub release, and a GitHub release asset answers
/// `302` to `release-assets.githubusercontent.com` — a different host. A3's
/// second condition refuses a redirect that leaves the compiled-in host, so
/// fetching from GitHub cannot satisfy it. Serving the files from one origin
/// the project controls keeps the condition literally true instead of
/// negotiating it down to an allowlist.
///
/// # Why one host, spelled out
///
/// Everything fetched is built from this constant. A served document cannot
/// redirect a download anywhere, because nothing served is ever consulted about
/// where to go — which is the property A1 established for the release check by
/// ignoring the manifest's `url`, applied to bytes instead of a link.
pub const ARTIFACT_HOST: &str = "";

/// What went wrong. Every variant means no file was produced.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// This build has no artifact host compiled in.
    #[error("this build has no release download host compiled in, so it cannot fetch anything")]
    NoHostCompiledIn,

    /// The compiled-in host is not `https://`.
    ///
    /// A3 permits "one HTTPS GET of one release artifact". Plaintext would put
    /// an installer on the wire for anyone on the path to replace -- and while
    /// the signature would catch a substitution, it would not hide *which*
    /// build a user is fetching from whoever is watching. Refused at the first
    /// call rather than trusted to be set correctly.
    #[error("the compiled-in release host is not https ({0}); A3 permits only an HTTPS fetch")]
    InsecureHost(String),

    /// No artifact could be named for this machine — see [`ResolveError`].
    #[error(transparent)]
    Resolve(#[from] ResolveError),

    /// The bytes arrived and did not prove to be the project's — see
    /// [`VerifyError`]. The partial file has already been deleted.
    #[error(transparent)]
    Unverified(#[from] VerifyError),

    /// The request did not complete: offline, DNS, TLS, timeout, 404.
    #[error("could not fetch {what}: {why}")]
    Unreachable {
        /// Which file was being fetched.
        what: String,
        /// The transport's own words.
        why: String,
    },

    /// A redirect pointed off [`ARTIFACT_HOST`].
    ///
    /// Its own variant rather than folded into [`Self::Unreachable`] because
    /// it is the one failure that might be an attack rather than a bad day,
    /// and a person reading a log should be able to tell them apart.
    #[error("refusing a redirect that leaves {host}")]
    RedirectedOffHost {
        /// The host this build is willing to talk to.
        host: String,
    },

    /// The file could not be written, or could not be moved into place.
    #[error("could not write {path}: {why}")]
    Io {
        /// Where it was trying to write.
        path: String,
        /// The OS's own words.
        why: String,
    },
}

/// A completed, verified download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Where the file now is.
    pub path: PathBuf,
    /// The basename the release published it under.
    pub name: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Whether a URL is `https://`, case-insensitively.
///
/// Scheme comparison is ASCII-case-insensitive per RFC 3986, so `HTTPS://` is
/// the same scheme. Nothing else is accepted: not `http`, and not a
/// scheme-relative `//host` that would inherit whatever a caller assumed.
fn is_https(url: &str) -> bool {
    let url = url.trim();
    url.len() > 8 && url[..8].eq_ignore_ascii_case("https://")
}

/// The host part of a URL, lowercased, or `None` if it has none.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// An HTTP client that will not follow a redirect off `host`.
///
/// `reqwest`'s default follows up to ten redirects anywhere. A3 condition 2
/// does not permit that: the point of compiling the host in is lost if the
/// first response can send the client somewhere else.
fn client_pinned_to(host: &str) -> Result<reqwest::Client, DownloadError> {
    let host = host.to_string();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        // A same-host redirect that downgrades the scheme is still a
        // downgrade, and `host_of` does not look at schemes.
        if !is_https(attempt.url().as_str()) {
            return attempt.stop();
        }
        match host_of(attempt.url().as_str()) {
            // Same host: an ordinary redirect, and still bounded.
            Some(h) if h == host => {
                if attempt.previous().len() > 5 {
                    attempt.error("too many redirects")
                } else {
                    attempt.follow()
                }
            }
            _ => attempt.stop(),
        }
    });
    reqwest::Client::builder()
        // The same User-Agent the release check sends: the bare product name,
        // no version, so the request does not disclose which build is asking.
        // A3 condition 6.
        .user_agent("PeerBeam")
        .redirect(policy)
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| DownloadError::Unreachable {
            what: "the download client".to_string(),
            why: e.to_string(),
        })
}

/// Fetch a small text file whole. Used for the checksums and the signature.
async fn get_text(
    client: &reqwest::Client,
    url: &str,
    what: &str,
) -> Result<String, DownloadError> {
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| classify(e, what))?
        .error_for_status()
        .map_err(|e| DownloadError::Unreachable {
            what: what.to_string(),
            why: e.to_string(),
        })?;
    res.text().await.map_err(|e| classify(e, what))
}

/// A redirect refused by the policy surfaces as an ordinary request error, so
/// it is separated out here rather than reported as "offline".
fn classify(e: reqwest::Error, what: &str) -> DownloadError {
    if e.is_redirect() {
        return DownloadError::RedirectedOffHost {
            host: host_of(ARTIFACT_HOST).unwrap_or_else(|| ARTIFACT_HOST.to_string()),
        };
    }
    DownloadError::Unreachable {
        what: what.to_string(),
        why: e.to_string(),
    }
}

/// Download the release `version`'s artifact for this machine into `dir`.
///
/// Returns where it landed. Nothing is written to `dir` unless every check
/// passed: the bytes go to a temporary file beside the destination and are
/// renamed in only at the end, so an interrupted or refused download cannot
/// leave something that looks like a release sitting in a download folder.
///
/// `progress` is called with (bytes so far, total if the server said) as the
/// stream arrives. It is for drawing a bar and nothing else — no decision here
/// depends on it.
pub async fn fetch<F>(
    version: &str,
    dir: &Path,
    mut progress: F,
) -> Result<Downloaded, DownloadError>
where
    F: FnMut(u64, Option<u64>),
{
    if ARTIFACT_HOST.trim().is_empty() {
        return Err(DownloadError::NoHostCompiledIn);
    }
    if !is_https(ARTIFACT_HOST) {
        return Err(DownloadError::InsecureHost(
            ARTIFACT_HOST.trim().to_string(),
        ));
    }
    let host = host_of(ARTIFACT_HOST).ok_or(DownloadError::NoHostCompiledIn)?;
    let client = client_pinned_to(&host)?;

    // Resolving happens before anything is fetched: on a machine where the
    // package format cannot be established there is nothing to ask for, and
    // asking anyway would disclose the platform for no reason.
    let name = artifact::artifact_for_this_build(version)?;
    let v = version.trim().trim_start_matches('v');
    let base = format!("{}/{}", ARTIFACT_HOST.trim_end_matches('/'), v);

    // Checksums and signature first, and both whole: they are kilobytes, and
    // fetching megabytes before knowing whether they can be checked wastes a
    // user's bandwidth on bytes that may be thrown away.
    let sums = get_text(&client, &format!("{base}/SHA256SUMS"), "the checksums").await?;
    let sig = get_text(
        &client,
        &format!("{base}/SHA256SUMS.minisig"),
        "the checksum signature",
    )
    .await?;

    // And the signature is checked before a single artifact byte is requested.
    // A list that is not the project's cannot be made true by what arrives
    // afterwards.
    verify::verify_sums(sums.as_bytes(), &sig)?;

    let expected =
        verify::digest_for(&sums, &name).ok_or_else(|| VerifyError::NotListed(name.clone()))?;

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| DownloadError::Io {
            path: dir.display().to_string(),
            why: e.to_string(),
        })?;

    // A temporary name in the destination directory, so the rename at the end
    // is within one filesystem and therefore atomic. `.part` rather than a
    // hidden file: a person who goes looking should be able to see it.
    let part = dir.join(format!("{name}.part"));
    let dest = dir.join(&name);

    let mut res = client
        .get(format!("{base}/{name}"))
        .send()
        .await
        .map_err(|e| classify(e, &name))?
        .error_for_status()
        .map_err(|e| DownloadError::Unreachable {
            what: name.clone(),
            why: e.to_string(),
        })?;
    let total = res.content_length();

    let outcome = stream_to(&part, &mut res, total, &mut progress).await;
    let (written, actual) = match outcome {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };

    if actual != expected {
        // Unconditionally, before returning: A3 condition 3 says a mismatch
        // deletes the bytes. Leaving them behind would be leaving an unproven
        // installer in somebody's downloads folder.
        let _ = tokio::fs::remove_file(&part).await;
        return Err(VerifyError::DigestMismatch {
            name,
            expected,
            actual,
        }
        .into());
    }

    tokio::fs::rename(&part, &dest)
        .await
        .map_err(|e| DownloadError::Io {
            path: dest.display().to_string(),
            why: e.to_string(),
        })?;

    Ok(Downloaded {
        path: dest,
        name,
        bytes: written,
    })
}

/// Stream a response to `path`, hashing as it goes.
///
/// Returns (bytes written, lowercase hex digest). The file is never held in
/// memory: each chunk is written and folded into the hash and then dropped.
async fn stream_to<F>(
    path: &Path,
    res: &mut reqwest::Response,
    total: Option<u64>,
    progress: &mut F,
) -> Result<(u64, String), DownloadError>
where
    F: FnMut(u64, Option<u64>),
{
    let io = |e: std::io::Error| DownloadError::Io {
        path: path.display().to_string(),
        why: e.to_string(),
    };
    let mut file = tokio::fs::File::create(path).await.map_err(io)?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    loop {
        let chunk = match res.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(classify(e, "the download")),
        };
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(io)?;
        written += chunk.len() as u64;
        progress(written, total);
    }
    // Flush and sync before hashing is declared final: a file that is renamed
    // into place while bytes are still in a buffer is a file whose verified
    // digest describes something not yet on disk.
    file.flush().await.map_err(io)?;
    file.sync_all().await.map_err(io)?;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push(HEX[(b >> 4) as usize] as char);
        hex.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok((written, hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_read_out_of_an_ordinary_url() {
        assert_eq!(
            host_of("https://pub-abc.r2.dev"),
            Some("pub-abc.r2.dev".into())
        );
        assert_eq!(
            host_of("https://pub-abc.r2.dev/0.12.0/x.deb"),
            Some("pub-abc.r2.dev".into())
        );
    }

    #[test]
    fn a_port_is_not_part_of_the_host() {
        assert_eq!(
            host_of("https://example.com:8443/x"),
            Some("example.com".into())
        );
    }

    /// `https://evil.com@pub-abc.r2.dev/` reads as evil.com to a careless
    /// parser and is really pub-abc.r2.dev; `https://pub-abc.r2.dev@evil.com/`
    /// is the attack that matters, and is really evil.com.
    #[test]
    fn userinfo_does_not_disguise_the_host() {
        assert_eq!(
            host_of("https://pub-abc.r2.dev@evil.example/0.12.0/x.deb"),
            Some("evil.example".into())
        );
    }

    #[test]
    fn case_does_not_change_a_host() {
        assert_eq!(
            host_of("https://PUB-ABC.R2.DEV/x"),
            Some("pub-abc.r2.dev".into())
        );
    }

    #[test]
    fn only_https_counts_as_https() {
        assert!(is_https("https://pub-abc.r2.dev"));
        assert!(is_https("HTTPS://pub-abc.r2.dev"));
        assert!(!is_https("http://pub-abc.r2.dev"));
        assert!(!is_https("//pub-abc.r2.dev"));
        assert!(!is_https("https://"));
        assert!(!is_https("ftp://pub-abc.r2.dev"));
        // No sneaking it in later in the string.
        assert!(!is_https("http://evil.example/?x=https://pub-abc.r2.dev"));
    }

    #[test]
    fn a_url_without_a_host_has_none() {
        assert_eq!(host_of("not-a-url"), None);
        assert_eq!(host_of("https://"), None);
    }

    /// The shipped default refuses, so a build released before the bucket
    /// exists cannot fetch from somewhere else instead.
    #[tokio::test]
    async fn without_a_compiled_in_host_nothing_downloads() {
        if !ARTIFACT_HOST.trim().is_empty() {
            return;
        }
        let dir = std::env::temp_dir().join("peerbeam-download-test-nohost");
        let err = fetch("0.12.0", &dir, |_, _| {}).await.unwrap_err();
        assert!(
            matches!(err, DownloadError::NoHostCompiledIn),
            "expected NoHostCompiledIn, got {err:?}"
        );
        // And it must not have created anything on the way to refusing.
        assert!(!dir.exists(), "refusing must not create the directory");
    }

    /// Every failure names what failed, so a log says which of the three
    /// fetches went wrong rather than "download failed".
    #[test]
    fn failures_name_what_they_were_fetching() {
        let e = DownloadError::Unreachable {
            what: "the checksums".into(),
            why: "timed out".into(),
        };
        assert!(e.to_string().contains("the checksums"));
        let e = DownloadError::RedirectedOffHost {
            host: "pub-abc.r2.dev".into(),
        };
        assert!(e.to_string().contains("pub-abc.r2.dev"));
    }
}
