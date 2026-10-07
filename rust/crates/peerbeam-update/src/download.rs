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
//! * **Condition 2 — the URL is compiled in, never served.**
//!   [`RELEASE_DOWNLOAD_BASE`] is a constant, and the path is built from the
//!   version and [`crate::artifact`]'s naming rules. A redirect is followed
//!   only to a host in [`REDIRECT_ALLOWLIST`], which is also compiled in (A4).
//!   Nothing read from the network chooses where any byte comes from.
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

/// Where release artifacts are served from: the project's own GitHub release
/// download base, with no trailing slash.
///
/// A release asset lives at `<this>/v<version>/<file>` — the tag carries a
/// leading `v`, the file does not.
///
/// # Why GitHub, and not somewhere the project runs
///
/// Because it is already there, already free, and already serving these bytes.
/// Self-hosting was examined and rejected: R2 needs a subscription and brings a
/// credential, a quota and a pruning chore; Cloudflare Pages caps a file at
/// 25 MiB against a 33.4 MiB DMG; GitHub Pages commits artifacts to a
/// repository that keeps them forever. A4 records all of it.
///
/// # Why this is not a hole
///
/// This constant is compiled in, so nothing served chooses it, and a response
/// can only move the fetch between hosts in [`REDIRECT_ALLOWLIST`] — also
/// compiled in. The property A3 condition 2 protects is that a served document
/// never decides what lands on a user's disk, and that holds: the set of
/// possible destinations is fixed before any request is made.
pub const RELEASE_DOWNLOAD_BASE: &str =
    "https://github.com/alpha-neo-omega/PeerBeam/releases/download";

/// The only hosts a redirect may lead to.
///
/// A literal, per A4 condition 1: a list that could be edited at runtime by
/// whoever can write a config file is not a pin.
///
/// Exactly the two observed to be required (A4 condition 2). `github.com` is
/// where the request starts; every asset on the live v0.12.0 release answers
/// `302` to `release-assets.githubusercontent.com`.
/// `objects.githubusercontent.com` is deliberately absent — nothing observed
/// needs it, and A4 forbids adding a host pre-emptively. No wildcard:
/// `*.githubusercontent.com` would be a far larger permission, covering every
/// user-uploaded file on the platform.
///
/// If GitHub ever serves assets from somewhere else, downloads refuse with
/// [`DownloadError::RedirectedOffHost`] naming the host, and it is added here
/// in a commit citing the redirect that required it.
pub const REDIRECT_ALLOWLIST: &[&str] = &["github.com", "release-assets.githubusercontent.com"];

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

    /// The request did not complete: offline, DNS, TLS, timeout, a 404, a
    /// redirect loop, or a redirect with nowhere to go.
    #[error("could not fetch {what}: {why}")]
    Unreachable {
        /// Which file was being fetched.
        what: String,
        /// The transport's own words.
        why: String,
    },

    /// A redirect pointed outside [`REDIRECT_ALLOWLIST`].
    ///
    /// Its own variant rather than folded into [`Self::Unreachable`] because
    /// it may be an attack rather than a bad day, and a person reading a log
    /// should be able to tell them apart (A4 condition 4).
    #[error("refusing a redirect to {host}, which is not on this build's allowlist")]
    RedirectedOffHost {
        /// The host the redirect pointed at.
        host: String,
    },

    /// A redirect pointed at plain `http`, whatever the host (A4 condition 3).
    /// Kept apart from [`Self::Unreachable`] for the same reason as
    /// [`Self::RedirectedOffHost`].
    #[error("refusing a redirect to plain http at {host}; every hop must be https")]
    InsecureRedirect {
        /// The host the redirect pointed at.
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

/// An HTTP client that will only follow redirects within [`REDIRECT_ALLOWLIST`].
///
/// `reqwest`'s default follows up to ten redirects anywhere. A4 permits a hop
/// only to a host this build already trusted before it made the request.
fn client_allowlisted() -> Result<reqwest::Client, DownloadError> {
    client_builder()
        .build()
        .map_err(|e| DownloadError::Unreachable {
            what: "the download client".to_string(),
            why: e.to_string(),
        })
}

/// Everything a download request carries, decided in one place so that a test
/// can check exactly that.
fn client_builder() -> reqwest::ClientBuilder {
    // A refusal is an error, never a stop. `reqwest` hands a stopped 3xx back
    // as the response, and the redirect's own body would then be verified as
    // though it were the file.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        match judge_redirect(attempt.url().as_str(), attempt.previous().len()) {
            Ok(()) => attempt.follow(),
            Err(refusal) => attempt.error(refusal),
        }
    });
    reqwest::Client::builder()
        // The same User-Agent the release check sends: the bare product name,
        // no version, so the request does not disclose which build is asking.
        // A3 condition 6.
        .user_agent("PeerBeam")
        .redirect(policy)
        // `reqwest` adds a `Referer` to every redirect it follows, naming the
        // URL it came from. A1 condition 2, carried to the download by A3
        // condition 6, lets nothing travel that a bare GET does not
        // unavoidably carry, and the User-Agent is the only header that may
        // name the product.
        .referer(false)
        .timeout(std::time::Duration::from_secs(600))
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
        .map_err(|e| classify(e, what))?;
    let res = the_file(res, what)?;
    res.text().await.map_err(|e| classify(e, what))
}

/// The most redirects one fetch will follow. GitHub uses one.
const MAX_REDIRECTS: usize = 5;

/// Why the redirect policy refused a hop.
///
/// The policy hands this to `reqwest` as the error for the request, and
/// [`classify`] takes it back out, so what was refused survives the trip and
/// is reported as itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RedirectRefusal {
    /// A host not on [`REDIRECT_ALLOWLIST`].
    OffHost(String),
    /// A hop that is not `https`.
    Insecure(String),
    /// More hops than [`MAX_REDIRECTS`].
    TooMany,
}

impl std::fmt::Display for RedirectRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RedirectRefusal::OffHost(host) => write!(f, "redirect to {host} refused"),
            RedirectRefusal::Insecure(host) => write!(f, "redirect to http at {host} refused"),
            RedirectRefusal::TooMany => write!(f, "more than {MAX_REDIRECTS} redirects"),
        }
    }
}

impl std::error::Error for RedirectRefusal {}

impl RedirectRefusal {
    /// The error a refused hop ends the fetch of `what` with.
    fn into_error(self, what: &str) -> DownloadError {
        match self {
            RedirectRefusal::OffHost(host) => DownloadError::RedirectedOffHost { host },
            RedirectRefusal::Insecure(host) => DownloadError::InsecureRedirect { host },
            // Every host in the loop was one this build trusts, so this is not
            // a diversion. It is a fetch that could not complete.
            RedirectRefusal::TooMany => DownloadError::Unreachable {
                what: what.to_string(),
                why: format!(
                    "more than {MAX_REDIRECTS} redirects, all between hosts on the allowlist"
                ),
            },
        }
    }
}

/// Whether to follow a redirect to `next`, with `requested` URLs already
/// fetched in this chain, the first included.
///
/// Pure, so the rule is testable without a server. Where a hop leads is judged
/// before how it gets there: a hop off the list is a diversion whatever its
/// scheme. After that, every hop must be https (A4 condition 3), and the chain
/// is bounded (condition 4).
fn judge_redirect(next: &str, requested: usize) -> Result<(), RedirectRefusal> {
    let host = host_of(next).unwrap_or_else(|| next.to_string());
    if !REDIRECT_ALLOWLIST.contains(&host.as_str()) {
        return Err(RedirectRefusal::OffHost(host));
    }
    if !is_https(next) {
        return Err(RedirectRefusal::Insecure(host));
    }
    if requested > MAX_REDIRECTS {
        return Err(RedirectRefusal::TooMany);
    }
    Ok(())
}

/// Turn a failed request into what went wrong.
///
/// A redirect the policy refused comes back from `reqwest` as an ordinary
/// request error with the [`RedirectRefusal`] inside it. It is taken back out
/// here so that the refusal is reported as itself, and never as being offline.
fn classify(e: reqwest::Error, what: &str) -> DownloadError {
    let mut source = std::error::Error::source(&e);
    while let Some(inner) = source {
        if let Some(refusal) = inner.downcast_ref::<RedirectRefusal>() {
            return refusal.clone().into_error(what);
        }
        source = inner.source();
    }
    DownloadError::Unreachable {
        what: what.to_string(),
        why: e.to_string(),
    }
}

/// The response, if it is the file that was asked for.
///
/// `error_for_status` passes a 3xx through. One that gets this far is a
/// redirect the client did not follow because it had nowhere to go, and its
/// body is not the file. Verifying that body anyway would report the release
/// as tampered with when the server had only misbehaved.
fn the_file(res: reqwest::Response, what: &str) -> Result<reqwest::Response, DownloadError> {
    let res = res
        .error_for_status()
        .map_err(|e| DownloadError::Unreachable {
            what: what.to_string(),
            why: e.to_string(),
        })?;
    if !res.status().is_success() {
        return Err(DownloadError::Unreachable {
            what: what.to_string(),
            why: format!(
                "the server answered {} without saying where to go",
                res.status()
            ),
        });
    }
    Ok(res)
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
    // First, because it is the one input that came from a served document:
    // a "version" that is not one must not name a request or a file. Before
    // platform resolution too, so the refusal does not depend on whether this
    // machine happens to be able to name its artifact.
    let v = artifact::release_version(version)?;

    if RELEASE_DOWNLOAD_BASE.trim().is_empty() {
        return Err(DownloadError::NoHostCompiledIn);
    }
    if !is_https(RELEASE_DOWNLOAD_BASE) {
        return Err(DownloadError::InsecureHost(
            RELEASE_DOWNLOAD_BASE.trim().to_string(),
        ));
    }
    let client = client_allowlisted()?;

    // Resolving happens before anything is fetched: on a machine where the
    // package format cannot be established there is nothing to ask for, and
    // asking anyway would disclose the platform for no reason.
    let name = artifact::artifact_for_this_build(version)?;
    // `v<version>` because that is the tag; the files inside carry no `v`.
    let base = format!("{}/v{}", RELEASE_DOWNLOAD_BASE.trim_end_matches('/'), v);

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

    let res = client
        .get(format!("{base}/{name}"))
        .send()
        .await
        .map_err(|e| classify(e, &name))?;
    let mut res = the_file(res, &name)?;
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

    /// The compiled-in base is a real https GitHub release URL.
    #[test]
    fn the_release_base_is_https_and_points_at_this_projects_releases() {
        assert!(is_https(RELEASE_DOWNLOAD_BASE));
        assert_eq!(
            host_of(RELEASE_DOWNLOAD_BASE).as_deref(),
            Some("github.com")
        );
        assert!(RELEASE_DOWNLOAD_BASE.ends_with("/releases/download"));
        assert!(!RELEASE_DOWNLOAD_BASE.ends_with('/'));
    }

    /// A4 condition 2: exactly the hosts observed to be required, no wildcard,
    /// and the origin itself must be on the list or the first hop fails.
    #[test]
    fn the_allowlist_is_minimal_and_contains_the_origin() {
        assert_eq!(
            REDIRECT_ALLOWLIST,
            &["github.com", "release-assets.githubusercontent.com"]
        );
        let origin = host_of(RELEASE_DOWNLOAD_BASE).expect("base has a host");
        assert!(
            REDIRECT_ALLOWLIST.contains(&origin.as_str()),
            "the origin must be allowlisted or nothing can be fetched at all"
        );
        for h in REDIRECT_ALLOWLIST {
            assert!(
                !h.contains('*'),
                "{h} is a wildcard; A4 condition 2 forbids it"
            );
            assert!(!h.is_empty());
        }
    }

    /// A refused redirect names the host it would have gone to. That is the
    /// evidence A4 condition 2 asks for before any host is added to the list.
    #[test]
    fn a_refused_redirect_names_the_host_it_refused() {
        let m = DownloadError::RedirectedOffHost {
            host: "evil.example".into(),
        }
        .to_string();
        assert!(m.contains("evil.example"), "{m}");
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
    /// A one-request HTTP/1.1 server on localhost. It records the request's
    /// headers, answers with `response`, and hangs up.
    fn one_request_server(
        response: String,
    ) -> (String, std::sync::mpsc::Receiver<Vec<(String, String)>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match conn.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let headers = String::from_utf8_lossy(&head)
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let _ = tx.send(headers);
            let _ = conn.write_all(response.as_bytes());
        });
        (base, rx)
    }

    fn header_names(headers: &[(String, String)]) -> Vec<&str> {
        let mut names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// **A redirected request carries nothing the first one did not.** A1
    /// condition 2, which A3 condition 6 carries over to the download, allows
    /// nothing beyond what a bare GET unavoidably discloses. The User-Agent is
    /// the only header that names the product. `reqwest` adds a `Referer` to
    /// every redirect it follows, and that header would name the release URL
    /// the redirect came from.
    ///
    /// The client here is the real one with a single change. Its policy is
    /// swapped for one that follows a hop between two local servers, a hop the
    /// real allowlist rightly refuses. Everything the request carries still
    /// comes from [`client_builder`].
    #[tokio::test]
    async fn a_redirected_request_carries_only_what_the_first_one_did() {
        let (second, second_seen) = one_request_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into(),
        );
        let (first, first_seen) = one_request_server(format!(
            "HTTP/1.1 302 Found\r\nLocation: {second}/SHA256SUMS\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        let client = client_builder()
            .redirect(reqwest::redirect::Policy::limited(1))
            .no_proxy()
            .build()
            .expect("client");

        let body = get_text(&client, &format!("{first}/SHA256SUMS"), "the checksums")
            .await
            .expect("the local redirect is followed");
        assert_eq!(body, "ok");

        let first = first_seen.recv().expect("the first request");
        let second = second_seen.recv().expect("the redirected request");
        for (hop, headers) in [("first", &first), ("redirected", &second)] {
            assert_eq!(
                header_names(headers),
                ["accept", "host", "user-agent"],
                "the {hop} request carried {headers:?}"
            );
            let agent = headers
                .iter()
                .find(|(k, _)| k == "user-agent")
                .map(|(_, v)| v.as_str());
            assert_eq!(agent, Some("PeerBeam"), "the {hop} request");
        }
    }
    // The redirect rule, decided without a network.

    #[test]
    fn an_allowlisted_https_hop_is_followed() {
        for next in [
            "https://github.com/alpha-neo-omega/PeerBeam/releases/download/v0.13.0/SHA256SUMS",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sig=x",
            "HTTPS://GITHUB.COM/x",
        ] {
            assert_eq!(judge_redirect(next, 1), Ok(()), "{next}");
        }
    }

    /// A4 condition 4: a hop off the list is refused, and the refusal names
    /// the host. So a person can tell a diverted download from a bad day, and a
    /// maintainer can see where GitHub has started sending people.
    #[test]
    fn a_hop_off_the_allowlist_is_refused_naming_the_host() {
        for (next, host) in [
            ("https://evil.example/SHA256SUMS", "evil.example"),
            // A lookalike is another host.
            (
                "https://github.com.evil.example/x",
                "github.com.evil.example",
            ),
            ("https://evilgithub.com/x", "evilgithub.com"),
            // Adjacent to a trusted host is not trusted (A4's Scope).
            (
                "https://objects.githubusercontent.com/x",
                "objects.githubusercontent.com",
            ),
            // Userinfo does not disguise where the request would go.
            ("https://github.com@evil.example/x", "evil.example"),
        ] {
            assert_eq!(
                judge_redirect(next, 1),
                Err(RedirectRefusal::OffHost(host.into())),
                "{next}"
            );
        }
    }

    /// A4 condition 3: every hop is https, even to a host on the list.
    #[test]
    fn a_hop_down_to_http_is_refused_even_to_an_allowlisted_host() {
        assert_eq!(
            judge_redirect("http://github.com/x", 1),
            Err(RedirectRefusal::Insecure("github.com".into()))
        );
    }

    /// A hop that breaks both rules is reported as the diversion. Where the
    /// request was sent matters more than how.
    #[test]
    fn a_diversion_is_reported_before_a_downgrade() {
        assert_eq!(
            judge_redirect("http://evil.example/x", 1),
            Err(RedirectRefusal::OffHost("evil.example".into()))
        );
    }

    /// Hops are bounded (A4 condition 4): five redirects are followed, and a
    /// sixth is not. `requested` counts every URL fetched in the chain so far,
    /// the first one included.
    #[test]
    fn the_sixth_redirect_is_refused() {
        assert_eq!(judge_redirect("https://github.com/x", 5), Ok(()));
        assert_eq!(
            judge_redirect("https://github.com/x", 6),
            Err(RedirectRefusal::TooMany)
        );
    }

    /// A loop between hosts that are both trusted is not an attack, and must
    /// not read like one. It is a fetch that could not complete.
    #[test]
    fn too_many_redirects_is_unreachable_not_a_diversion() {
        match RedirectRefusal::TooMany.into_error("the checksums") {
            DownloadError::Unreachable { what, why } => {
                assert_eq!(what, "the checksums");
                assert!(why.contains("redirects"), "{why}");
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    // The same rule, through the real client and a local server.

    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    /// The production client, policy included, minus the environment's proxy,
    /// so that a test run behind one still reaches the local server.
    fn production_client() -> reqwest::Client {
        client_builder().no_proxy().build().expect("client")
    }

    /// **A diverted fetch says where it was sent.** The policy used to *stop*
    /// at a hop like this. `reqwest` hands a stopped 3xx back as the response,
    /// so the redirect's own body was verified as though it were the file, and
    /// the refusal read as a bad signature. It must name the host instead.
    #[tokio::test]
    async fn a_redirect_off_the_allowlist_is_refused_by_name() {
        let (base, _) = one_request_server(redirect_to("https://evil.example/SHA256SUMS"));
        let err = get_text(
            &production_client(),
            &format!("{base}/SHA256SUMS"),
            "the checksums",
        )
        .await
        .expect_err("a diverted fetch must not produce a body");
        assert!(
            matches!(err, DownloadError::RedirectedOffHost { ref host } if host == "evil.example"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_redirect_down_to_http_is_refused_by_name() {
        let (base, _) = one_request_server(redirect_to("http://github.com/x"));
        let err = get_text(
            &production_client(),
            &format!("{base}/SHA256SUMS"),
            "the checksums",
        )
        .await
        .expect_err("a downgraded fetch must not produce a body");
        assert!(
            matches!(err, DownloadError::InsecureRedirect { ref host } if host == "github.com"),
            "{err:?}"
        );
    }

    /// A redirect with no `Location` cannot be followed, and its body is not
    /// the file either. Taking it for the file would report the release as
    /// tampered with when the server had only misbehaved.
    #[tokio::test]
    async fn a_redirect_with_nowhere_to_go_is_not_taken_for_the_file() {
        let (base, _) = one_request_server(
            "HTTP/1.1 302 Found\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad".into(),
        );
        let err = get_text(
            &production_client(),
            &format!("{base}/SHA256SUMS"),
            "the checksums",
        )
        .await
        .expect_err("a 302 is not the checksums");
        assert!(
            matches!(err, DownloadError::Unreachable { ref why, .. } if why.contains("302")),
            "{err:?}"
        );
    }

    /// Version validation is the first thing `fetch` does that depends on its
    /// input. Ordering is the point: checked only after platform resolution,
    /// a machine that *can* resolve its artifact would carry a hostile string
    /// into a request before anything stopped it. On a Linux box where
    /// nothing owns the test binary, resolution fails first -- so this test
    /// can only pass if the version is checked before it.
    #[tokio::test]
    async fn a_hostile_version_is_refused_before_any_request_or_write() {
        let dir = std::env::temp_dir().join("peerbeam-download-test-hostile-version");
        let _ = std::fs::remove_dir_all(&dir);
        let mut reported = false;
        let err = fetch("99.0.0/../../x", &dir, |_, _| reported = true)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DownloadError::Resolve(crate::artifact::ResolveError::NotAVersion(_))
            ),
            "expected NotAVersion, got {err:?}"
        );
        assert!(!reported, "progress was reported for a refused version");
        assert!(!dir.exists(), "refusing must not create the directory");
    }
}
