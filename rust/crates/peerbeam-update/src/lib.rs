//! Asking, once, whether a newer release exists.
//!
//! # What this is allowed to be
//!
//! Amendment **A1** in `docs/ARCHITECTURAL_INVARIANTS.md` narrows invariant I4
//! (which forbids phone-home) to permit exactly this: one HTTPS GET, made only
//! when a person asks for it, returning a version string the app renders and
//! acts on in no other way.
//!
//! The conditions are not style preferences, they are the terms the amendment
//! was granted on:
//!
//! * **No identifiers.** Nothing here sends a device id, an install id, or any
//!   PeerBeam-specific header. What the server learns is what a bare HTTPS
//!   request unavoidably tells it.
//! * **Inert response.** [`Release`] is a version and a URL. Nothing downloads,
//!   nothing installs, no behaviour anywhere changes on the strength of it —
//!   and the URL is compiled in rather than read from the response, so the
//!   document cannot send anyone anywhere. See [`newest`].
//! * **Never a precondition.** Every failure is a plain `Err` the caller is
//!   expected to shrug at. Offline is normal for this app.
//! * **Opt-in per use.** There is no timer and no constructor that starts
//!   anything; a check happens because someone called [`check`].
//!
//! # Where the answer comes from
//!
//! The project's own site, at [`MANIFEST_URL`] — a two-field document generated
//! by `scripts/write-releases-json.mjs` in the website repository, from the same
//! constant that renders the download page's links.
//!
//! It used to be the GitHub release feed. That answers "what is the newest
//! tag", which is a slightly different question: the download page links exact
//! asset filenames, so a tag that exists before the site is rebuilt would send
//! everyone who acted on the prompt to a file that is not there. Asking the page
//! what it can actually offer removes that window, and keeps the request on one
//! origin the project controls instead of a third-party API whose
//! unauthenticated rate limit is shared by everyone behind a NAT.
//!
//! # Why the parsing is separate from the fetching
//!
//! [`newest`] is a pure function over a JSON body, so the document's shape is a
//! rule that can be tested without a network and cannot silently drift from the
//! generator that writes it.

use serde::Deserialize;

/// The update manifest: a two-field JSON document the project's own site
/// publishes for exactly this purpose.
///
/// Constant, and deliberately not derived from the crate manifest:
/// `rust/Cargo.toml` carried a `repository` URL naming a repo that does not
/// exist for most of this project's life, and an updater pointed at the wrong
/// place fails in a way nobody debugs.
///
/// # Why the site and not the GitHub release feed
///
/// The feed answers "what is the newest tag", which is not quite the question.
/// The site's download page links exact asset filenames
/// (`peerbeam-0.11.0-amd64.deb`), so a tag that exists before the page is
/// rebuilt would send everyone who acted on the prompt to a 404. The manifest
/// is generated from the same constant that renders those links, so it can only
/// advertise a version the page can actually hand someone.
///
/// It is also one request to one origin the project controls, rather than a
/// call to a third-party API with an unauthenticated rate limit shared by
/// everyone behind a NAT.
pub const MANIFEST_URL: &str = "https://peerbeam.pages.dev/releases.json";

/// Where a person goes to download a release.
///
/// The project's own download page, not the GitHub releases list. It names the
/// file for the platform the reader is on and says how to install it, which a
/// directory of twenty-three assets does not — and a user told "an update is
/// available" is being sent somewhere to act, not to browse.
///
/// The bytes still come from the GitHub release: the page links each asset to
/// `releases/latest/download/<file>`. So this changes where a person is sent,
/// not where anything is hosted, and it does not put a new party in the path of
/// the download.
pub const DOWNLOAD_PAGE: &str = "https://peerbeam.pages.dev/download";

/// What went wrong. Every variant is something the caller should treat as "no
/// answer", never as a reason to block anything.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The request did not complete — offline, DNS, TLS, timeout, refused.
    #[error("could not reach the update manifest: {0}")]
    Unreachable(String),
    /// It completed and said something this build cannot read.
    #[error("the update manifest was unreadable: {0}")]
    Unreadable(String),
}

/// A published release, as far as this app cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag with any leading `v` removed, e.g. `0.9.0`.
    pub version: String,
    /// The page a person can open to read about it.
    pub url: String,
}

/// The manifest as published. `url` is accepted but never used — see
/// [`newest`].
#[derive(Deserialize)]
struct Manifest {
    version: String,
}

/// The release a manifest body describes, or `None` when it names no version.
///
/// Pure, and separate from the fetching, so the document's shape is a rule that
/// can be tested without a network and cannot silently drift.
///
/// The manifest's own `url` field is deliberately **not** trusted as the place
/// to send the user. It arrives over the network, and the one thing this
/// feature does with its answer is offer to open a link — so the destination is
/// the compiled-in [`DOWNLOAD_PAGE`] and a served document cannot redirect
/// anybody anywhere. The field is still published for other readers, and this
/// accepts a body containing it without complaint.
///
/// A blank version is `None` rather than a release with an empty name:
/// [`is_newer`] parses unknown parts as 0, so an empty string would compare as
/// older than everything and quietly mean "you are up to date, forever".
pub fn newest(body: &str) -> Result<Option<Release>, UpdateError> {
    let manifest: Manifest =
        serde_json::from_str(body).map_err(|e| UpdateError::Unreadable(e.to_string()))?;
    let version = manifest.version.trim().trim_start_matches('v').to_string();
    if version.is_empty() {
        return Ok(None);
    }
    Ok(Some(Release {
        version,
        url: DOWNLOAD_PAGE.to_string(),
    }))
}

/// Whether `latest` is newer than `current`, by dotted numeric comparison.
///
/// Unknown or unparseable parts compare as 0 rather than erroring: a version
/// this build cannot parse is not a reason to claim an update exists, and it is
/// certainly not a reason to fail. Equal-length prefixes decide; `1.10.0` is
/// newer than `1.9.0`, which a string comparison would get backwards.
#[must_use]
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(latest), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

/// Ask the project's site what the newest published version is.
///
/// One GET, no identifiers, no retry, **and no fallback**. A caller that wants
/// to try again asks again; a second request to a different host on failure
/// would be the "ongoing, unattended" shape A1 does not cover, and would put
/// the check back on a third-party API the moment the first one hiccuped.
/// Unreachable is an ordinary answer here — offline is normal for this app.
pub async fn check() -> Result<Option<Release>, UpdateError> {
    // A User-Agent naming the product and nothing else: no version, no device,
    // no install id. That is the most a bare request can avoid disclosing while
    // still being served.
    let client = reqwest::Client::builder()
        .user_agent("PeerBeam")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| UpdateError::Unreachable(e.to_string()))?;
    let body = client
        .get(MANIFEST_URL)
        .send()
        .await
        .map_err(|e| UpdateError::Unreachable(e.to_string()))?
        .text()
        .await
        .map_err(|e| UpdateError::Unreachable(e.to_string()))?;
    newest(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest exactly as `scripts/write-releases-json.mjs` publishes it
    /// in the website repo. If that generator's shape changes, this fails.
    const MANIFEST: &str = r#"{
      "version": "0.11.0",
      "url": "https://peerbeam.pages.dev/download"
    }"#;

    #[test]
    fn the_manifest_version_is_the_release() {
        let r = newest(MANIFEST).unwrap().expect("a release");
        assert_eq!(r.version, "0.11.0");
        assert_eq!(r.url, DOWNLOAD_PAGE);
    }

    /// The site publishes a bare version; a `v` prefix is accepted anyway so
    /// the two spellings of the same release cannot mean different things.
    #[test]
    fn a_leading_v_is_not_part_of_the_version() {
        for body in [r#"{"version":"v1.2.3"}"#, r#"{"version":"1.2.3"}"#] {
            assert_eq!(newest(body).unwrap().unwrap().version, "1.2.3");
        }
    }

    /// A pre-release is a version like any other here. The site decides what it
    /// is willing to advertise; this reports whatever it says.
    #[test]
    fn a_prerelease_is_reported_like_anything_else() {
        let body = r#"{"version":"1.0.0-rc1"}"#;
        assert_eq!(newest(body).unwrap().unwrap().version, "1.0.0-rc1");
    }

    /// **A blank version is "nothing to report", not a release named "".**
    /// `is_newer` parses unknown parts as 0, so an empty string would compare
    /// as older than every build and quietly mean "up to date" forever — the
    /// exact failure an update check exists to avoid.
    #[test]
    fn a_blank_version_reports_nothing_rather_than_an_empty_release() {
        for body in [r#"{"version":""}"#, r#"{"version":"   "}"#] {
            assert_eq!(newest(body).unwrap(), None, "{body}");
        }
    }

    #[test]
    fn a_body_that_is_not_a_manifest_is_an_error_not_a_panic() {
        assert!(newest("not json").is_err());
        // The SPA shell, which is what a mis-deployed site serves in place of
        // the manifest — it must read as unreadable rather than as a release.
        assert!(newest("<!doctype html><html></html>").is_err());
        // Valid JSON, no version field.
        assert!(newest(r#"{"url":"https://example.invalid/"}"#).is_err());
    }

    /// A manifest carrying extra fields keeps working: the site may publish
    /// more for other readers than this build knows how to want.
    #[test]
    fn unknown_fields_do_not_break_the_read() {
        let body = r#"{"version":"9.9.9","url":"u","notes":"...","channel":"beta"}"#;
        assert_eq!(newest(body).unwrap().unwrap().version, "9.9.9");
    }

    /// **The served document cannot redirect anybody.** The one action this
    /// feature offers is opening a link, so the destination is compiled in and
    /// a `url` on the wire is ignored however inviting it looks.
    #[test]
    fn the_manifests_own_url_is_never_where_the_user_is_sent() {
        let body = r#"{"version":"1.2.3","url":"https://evil.invalid/payload"}"#;
        assert_eq!(newest(body).unwrap().unwrap().url, DOWNLOAD_PAGE);
    }

    /// The download page is the project's own, and it is HTTPS. A plain-http
    /// URL here would be an update prompt pointing at a downgradeable page.
    #[test]
    fn the_download_page_is_this_projects_own_https_page() {
        assert!(DOWNLOAD_PAGE.starts_with("https://"), "{DOWNLOAD_PAGE}");
        assert!(
            DOWNLOAD_PAGE.contains("peerbeam.pages.dev"),
            "{DOWNLOAD_PAGE}"
        );
    }

    /// A string comparison gets this backwards, which is the classic way an
    /// updater tells everyone on 1.9 that they are current forever.
    #[test]
    fn ten_is_newer_than_nine() {
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(!is_newer("1.9.0", "1.10.0"));
    }

    #[test]
    fn the_same_version_is_not_newer() {
        assert!(!is_newer("0.9.0", "0.9.0"));
    }

    #[test]
    fn a_shorter_version_compares_by_its_prefix() {
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0", "1.0.0"));
    }

    /// An unparseable version must not be reported as an update. Claiming one
    /// exists sends a person looking for a download that is not there.
    #[test]
    fn nonsense_never_claims_to_be_newer() {
        assert!(!is_newer("garbage", "0.9.0"));
        assert!(!is_newer("", "0.9.0"));
    }
}
