//! Live checks against the project's real GitHub release.
//!
//! **All `#[ignore]`d**, and deliberately so: they need the network and a
//! published release, and a unit suite that fails because GitHub is slow is a
//! suite people learn to ignore. CI runs the rest; these are run by hand:
//!
//! ```text
//! cargo test -p peerbeam-update --test live_release -- --ignored --nocapture
//! ```
//!
//! What they exist to prove is the one thing no offline test can: that A4's
//! allowlist matches where GitHub actually sends a download. Every asset on
//! v0.12.0 answers `302` to `release-assets.githubusercontent.com`, and if that
//! ever changes these fail with the new host named, which is exactly the signal
//! needed to amend the list.

use peerbeam_update::download::{self, DownloadError, REDIRECT_ALLOWLIST, RELEASE_DOWNLOAD_BASE};

/// A release that exists and is not going to be deleted.
const KNOWN_VERSION: &str = "0.12.0";

/// The redirect is followed, the checksums arrive, and the run then stops for
/// the *right* reason: nothing signs `SHA256SUMS` yet, so there is no
/// signature to check it against.
///
/// This is the whole point of A4. If the allowlist were wrong, the failure
/// would be `RedirectedOffHost` and the checksums would never arrive at all.
#[tokio::test]
#[ignore = "needs the network and a published release"]
async fn the_redirect_to_githubusercontent_is_followed() {
    let dir = std::env::temp_dir().join("peerbeam-live-release-test");
    let _ = std::fs::remove_dir_all(&dir);

    let err = download::fetch(KNOWN_VERSION, &dir, |_, _| {})
        .await
        .expect_err("nothing signs SHA256SUMS yet, so this cannot succeed");

    match err {
        // The expected stopping point: checksums fetched (so the redirect was
        // followed), signature absent.
        DownloadError::Unreachable { ref what, ref why } => {
            assert!(
                what.contains("signature"),
                "expected to get as far as the signature, but failed on {what}: {why}"
            );
            assert!(
                why.contains("404"),
                "expected a 404 for the missing signature, got: {why}"
            );
        }
        // The failure A4 exists to prevent.
        DownloadError::RedirectedOffHost { ref host } => panic!(
            "GitHub redirected somewhere the allowlist does not cover. \
             Allowed: {host}. Re-check where release assets are served from \
             and amend REDIRECT_ALLOWLIST with evidence (A4 condition 2)."
        ),
        // On a machine where the Linux package format cannot be established
        // this is correct behaviour, not a failure of the allowlist.
        DownloadError::Resolve(e) => {
            eprintln!("skipped: this machine cannot resolve its artifact ({e})");
        }
        other => panic!("unexpected failure: {other}"),
    }

    assert!(
        !dir.join(format!("peerbeam-{KNOWN_VERSION}-x86_64.AppImage"))
            .exists(),
        "an unverifiable download must leave nothing behind"
    );
}

/// The compiled-in base really does serve this project's releases.
#[tokio::test]
#[ignore = "needs the network"]
async fn the_release_base_serves_a_known_asset() {
    let url = format!("{RELEASE_DOWNLOAD_BASE}/v{KNOWN_VERSION}/SHA256SUMS");
    let body = reqwest::Client::builder()
        .user_agent("PeerBeam")
        .build()
        .expect("client")
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("could not fetch {url}: {e}"))
        .error_for_status()
        .unwrap_or_else(|e| panic!("{url} did not return success: {e}"))
        .text()
        .await
        .expect("body");

    // It is a checksum list, and it describes files this release published.
    assert!(
        body.contains(&format!("peerbeam-{KNOWN_VERSION}-amd64.deb")),
        "SHA256SUMS at {url} does not list the .deb; is the URL shape right?"
    );
    assert!(
        REDIRECT_ALLOWLIST.contains(&"github.com"),
        "the origin must be allowlisted"
    );
}

/// The first release the whole chain can be run against in production.
///
/// v0.12.1 is the first signed release. Everything before it was exercised
/// against a test server and a throwaway key; this is the real key compiled
/// into this binary, the real `SHA256SUMS.minisig`, GitHub's real redirect to
/// `release-assets.githubusercontent.com`, and the artifact's real bytes.
///
/// It needs an artifact this machine can name. On a Linux box with neither
/// `dpkg` nor `rpm` owning the binary, set `APPIMAGE` to anything to select the
/// AppImage, exactly as a genuine AppImage user's environment does -- without
/// it the test reports that it skipped rather than failing, because that is
/// correct behaviour for the resolver, not a defect in the download.
#[tokio::test]
#[ignore = "needs the network and downloads ~16 MB"]
async fn the_first_signed_release_downloads_and_verifies() {
    const FIRST_SIGNED: &str = "0.12.1";
    let dir = std::env::temp_dir().join("peerbeam-live-signed-release-test");
    let _ = std::fs::remove_dir_all(&dir);

    let done = match download::fetch(FIRST_SIGNED, &dir, |_, _| {}).await {
        Ok(done) => done,
        Err(DownloadError::Resolve(e)) => {
            eprintln!(
                "SKIPPED: this machine cannot name its artifact ({e}); set APPIMAGE to run it"
            );
            return;
        }
        Err(e) => {
            panic!("v{FIRST_SIGNED} did not download and verify against the compiled-in key: {e}")
        }
    };

    assert!(
        done.path.is_file(),
        "reported {} but no file is there",
        done.path.display()
    );
    assert!(
        done.name.contains(FIRST_SIGNED),
        "fetched {} for v{FIRST_SIGNED}",
        done.name
    );
    assert_eq!(
        std::fs::metadata(&done.path).expect("stat").len(),
        done.bytes,
        "the reported size is not the size on disk"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a .part file survived a successful download"
    );

    eprintln!("verified and saved {} ({} bytes)", done.name, done.bytes);
    let _ = std::fs::remove_dir_all(&dir);
}
