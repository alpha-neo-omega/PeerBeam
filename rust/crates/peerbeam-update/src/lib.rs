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
//!   nothing installs, no behaviour anywhere changes on the strength of it.
//! * **Never a precondition.** Every failure is a plain `Err` the caller is
//!   expected to shrug at. Offline is normal for this app.
//! * **Opt-in per use.** There is no timer and no constructor that starts
//!   anything; a check happens because someone called [`check`].
//!
//! # Why the parsing is separate from the fetching
//!
//! [`newest`] is a pure function over a JSON body. The release feed's shape —
//! that this project publishes pre-releases, so `/releases/latest` is not the
//! answer — is a rule worth testing without a network, and worth stating
//! somewhere it cannot silently drift.

use serde::Deserialize;

/// Where releases are published. Constant, and deliberately not derived from
/// the crate manifest: `rust/Cargo.toml` carried a `repository` URL naming a
/// repo that does not exist for most of this project's life, and an updater
/// pointed at the wrong repository fails in a way nobody debugs.
pub const RELEASES_API: &str = "https://api.github.com/repos/alpha-neo-omega/PeerBeam/releases";

/// Where a person goes to read about and download a release.
pub const RELEASES_PAGE: &str = "https://github.com/alpha-neo-omega/PeerBeam/releases";

/// What went wrong. Every variant is something the caller should treat as "no
/// answer", never as a reason to block anything.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The request did not complete — offline, DNS, TLS, timeout, refused.
    #[error("could not reach the release feed: {0}")]
    Unreachable(String),
    /// It completed and said something this build cannot read.
    #[error("the release feed was unreadable: {0}")]
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

#[derive(Deserialize)]
struct Entry {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    html_url: String,
}

/// The newest release in a feed body, or `None` when there is none to report.
///
/// **Not `/releases/latest`.** GitHub excludes pre-releases from that endpoint,
/// and this project published every release as one for its whole history — so
/// the "latest" endpoint answered 404 while five releases existed. Reading the
/// full list and taking the first entry is the only form that survives either
/// choice. Drafts are skipped: they are not published to anybody.
///
/// The feed is newest-first, which is GitHub's documented order.
pub fn newest(body: &str) -> Result<Option<Release>, UpdateError> {
    let entries: Vec<Entry> =
        serde_json::from_str(body).map_err(|e| UpdateError::Unreadable(e.to_string()))?;
    Ok(entries.into_iter().find(|e| !e.draft).map(|e| Release {
        version: e.tag_name.trim_start_matches('v').to_string(),
        url: if e.html_url.is_empty() {
            RELEASES_PAGE.to_string()
        } else {
            e.html_url
        },
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

/// Ask the release feed what the newest published version is.
///
/// One GET, no identifiers, no retry. A caller that wants to try again asks
/// again — a retry loop here would be the "ongoing, unattended" shape A1 does
/// not cover.
pub async fn check() -> Result<Option<Release>, UpdateError> {
    // A User-Agent is required by the GitHub API, and this one names the
    // product and nothing else: no version, no device, no install id. That is
    // the most a bare request can avoid disclosing while still being served.
    let client = reqwest::Client::builder()
        .user_agent("PeerBeam")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| UpdateError::Unreachable(e.to_string()))?;
    let body = client
        .get(RELEASES_API)
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

    const FEED: &str = r#"[
      {"tag_name":"v0.9.0","draft":false,"html_url":"https://example.invalid/9"},
      {"tag_name":"v0.8.2","draft":false,"html_url":"https://example.invalid/8"}
    ]"#;

    #[test]
    fn the_first_published_entry_is_the_newest() {
        let r = newest(FEED).unwrap().expect("a release");
        assert_eq!(r.version, "0.9.0");
        assert_eq!(r.url, "https://example.invalid/9");
    }

    /// This project published every release as a pre-release, so
    /// `/releases/latest` answered 404 while five existed. A pre-release must
    /// still be reported here, or the check says "up to date" forever.
    #[test]
    fn a_prerelease_still_counts_as_published() {
        let body = r#"[{"tag_name":"v1.0.0-rc1","draft":false,"prerelease":true,"html_url":"u"}]"#;
        assert_eq!(newest(body).unwrap().unwrap().version, "1.0.0-rc1");
    }

    /// A draft is not published to anybody, so reporting one would point a user
    /// at a page they cannot open.
    #[test]
    fn a_draft_is_skipped() {
        let body = r#"[
          {"tag_name":"v2.0.0","draft":true,"html_url":"u"},
          {"tag_name":"v1.0.0","draft":false,"html_url":"v"}
        ]"#;
        assert_eq!(newest(body).unwrap().unwrap().version, "1.0.0");
    }

    #[test]
    fn an_empty_feed_reports_nothing_rather_than_failing() {
        assert_eq!(newest("[]").unwrap(), None);
    }

    #[test]
    fn a_body_that_is_not_a_feed_is_an_error_not_a_panic() {
        assert!(newest("not json").is_err());
        assert!(newest(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn a_missing_url_falls_back_to_the_releases_page() {
        let body = r#"[{"tag_name":"v1.2.3","draft":false}]"#;
        assert_eq!(newest(body).unwrap().unwrap().url, RELEASES_PAGE);
    }

    #[test]
    fn the_leading_v_is_not_part_of_the_version() {
        let body = r#"[{"tag_name":"v1.2.3","draft":false,"html_url":"u"}]"#;
        assert_eq!(newest(body).unwrap().unwrap().version, "1.2.3");
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
