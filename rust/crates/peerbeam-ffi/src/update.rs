//! The app's side of downloading a release: the body of `pb_download_update`.
//!
//! The sequence is `peerbeam_update::newest::download_newest`, the same one
//! `peerbeam download-update` runs, and the answer is the same JSON. What is
//! the app's own is three things: the folder must be one the person chose and
//! must be absolute, only one download runs at a time, and progress goes out
//! as `update_download_progress` events for the Settings tile to draw.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use crate::error::Code;

/// The version this build is.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// `{dir}` → the shared download JSON (`DownloadOutcome::to_json`).
pub(crate) fn download(params: &Value) -> Result<Value, (Code, String)> {
    let dir = chosen_dir(params)?;
    let Some(_running) = OneAtATime::begin() else {
        return Ok(json!({
            "ok": false,
            "downloaded": false,
            "current": CURRENT,
            "latest": null,
            "reason": "a download is already running",
        }));
    };
    // The staging throttle's rules suit a download unchanged: the first report
    // at once, then at most one per 250 ms and 1%, and the finishing report.
    // With no length known it reports every 250 ms.
    let mut throttle = crate::transfer::StagingThrottle::new();
    let outcome = crate::runtime::block_on(peerbeam_update::newest::download_newest(
        CURRENT,
        dir,
        |done, total| {
            if throttle.due(done, total.unwrap_or(0)) {
                crate::events::emit(&progress_event(done, total));
            }
        },
    ));
    Ok(outcome.to_json(CURRENT))
}

/// The folder the person chose, from `{dir}`.
///
/// Required, and absolute: a relative path would resolve against wherever the
/// app happened to be started, which is not a place anyone chose.
fn chosen_dir(params: &Value) -> Result<&Path, (Code, String)> {
    let dir = params
        .get("dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if dir.is_empty() {
        return Err((
            Code::InvalidArgument,
            "dir is required: the folder the person chose to save the release in".into(),
        ));
    }
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err((
            Code::InvalidArgument,
            format!("dir must be an absolute path to a folder the person chose, not {dir:?}"),
        ));
    }
    Ok(path)
}

/// One `update_download_progress` event.
///
/// Flat `done` and `total`, the shape the Dart side decodes; `total` is `null`
/// when the server sent no length, never 0.
fn progress_event(done: u64, total: Option<u64>) -> Value {
    json!({
        "type": "update_download_progress",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "done": done,
        "total": total,
    })
}

/// Held while a download runs; a second one is refused rather than racing
/// the first for the same `.part` file.
struct OneAtATime;

static RUNNING: AtomicBool = AtomicBool::new(false);

impl OneAtATime {
    fn begin() -> Option<OneAtATime> {
        RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| OneAtATime)
    }
}

impl Drop for OneAtATime {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A3: the file goes to "a location that person chose". No folder, an
    /// empty one, or a relative one -- which would resolve against wherever
    /// the app happened to be started -- is refused before anything else
    /// happens, network included.
    #[test]
    fn a_download_needs_an_absolute_folder_the_person_chose() {
        for params in [
            json!({}),
            json!({ "dir": "" }),
            json!({ "dir": "   " }),
            json!({ "dir": 7 }),
            json!({ "dir": "Downloads" }),
            json!({ "dir": "./Downloads" }),
        ] {
            let got = download(&params);
            assert!(
                matches!(got, Err((Code::InvalidArgument, _))),
                "{params} should be refused as an invalid folder, got {got:?}"
            );
        }
        // Absolute on every platform: "/home/me" is not, on Windows.
        let abs = std::env::temp_dir();
        assert_eq!(
            chosen_dir(&json!({ "dir": abs.to_str().unwrap() })).unwrap(),
            abs.as_path()
        );
    }

    /// Two downloads into one folder would race each other for the same
    /// `.part` file. The app could ask twice -- leave Settings mid-download,
    /// come back, press Download -- so the second is refused while the first
    /// holds the slot, and the slot frees when the first is done.
    #[test]
    fn only_one_download_runs_at_a_time() {
        let first = OneAtATime::begin().expect("nothing is running yet");
        assert!(OneAtATime::begin().is_none(), "a second was let in");

        // And through the real entry point: refused at once, in the shared
        // shape, without touching the network. (Checked here rather than in a
        // test of its own: the slot is one static, and two tests flipping it
        // in parallel would race.)
        let dir = std::env::temp_dir();
        let busy = download(&json!({ "dir": dir.to_str().unwrap() })).unwrap();
        assert_eq!(busy["downloaded"], false, "{busy}");
        assert_eq!(busy["ok"], false, "{busy}");
        assert!(
            busy.get("latest").is_some(),
            "latest is always present: {busy}"
        );
        assert!(
            busy["reason"]
                .as_str()
                .unwrap_or("")
                .contains("already running"),
            "{busy}"
        );

        drop(first);
        assert!(OneAtATime::begin().is_some(), "the slot never freed");
    }

    /// The wire shape the Dart side decodes: flat `done` and `total`, and an
    /// unknown length as `null` -- never 0, which the tile would draw as 0%
    /// for the whole download.
    #[test]
    fn progress_with_no_length_says_so() {
        let e = progress_event(5_242_880, Some(10_485_760));
        assert_eq!(e["type"], "update_download_progress");
        assert_eq!(e["done"], 5_242_880);
        assert_eq!(e["total"], 10_485_760);

        let e = progress_event(100, None);
        assert_eq!(e["done"], 100);
        assert!(e["total"].is_null(), "an unknown length must be null: {e}");
    }
}
