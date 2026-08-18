//! `peerbeam watch` — send whatever lands in a folder.
//!
//! # Why a poll rather than a filesystem watcher
//!
//! A watcher needs a platform-specific API per OS, and the folders people
//! actually point this at — a network share, a synced directory, a phone's USB
//! mount — are exactly the ones where those APIs report nothing. A poll is
//! slower to notice and correct everywhere.
//!
//! # Why a file must stop growing first
//!
//! The moment a file appears is not the moment it is complete: a large copy
//! shows up at zero bytes and grows for minutes. Sending on sight would deliver
//! a truncated file that looks perfectly fine on both ends — the worst kind of
//! failure, because nothing reports it. A file is sent only once its size has
//! been unchanged across two consecutive scans.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::{SendArgs, WatchArgs};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

/// What the watcher knows about one file between scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    size: u64,
    /// Scans this size has held. A file is sent at 2 — seen once, then seen
    /// again unchanged.
    stable: u32,
}

/// How many consecutive equal-size scans mean "finished being written".
///
/// Two, not one: one scan proves nothing, and more than two only delays every
/// send to guard against a writer that stalls for exactly the poll interval —
/// which the next scan catches anyway, because a file that grows again is
/// simply not sent yet.
const STABLE_SCANS: u32 = 2;

/// Marks an entry as dealt with: already sent, or present before the watch
/// began. Distinct from any real scan count, so nothing can count up into it.
const DONE: u32 = u32::MAX;

/// Fold one scan's observation into what is known about a file, and answer the
/// only question that matters: **send it now?**
///
/// Extracted rather than inlined in the loop because it is the whole
/// correctness argument of this command. Tested through the map it lives in,
/// the logic could be quietly rewritten — "seen once is enough", say — while
/// every test still passed, and the failure would be a truncated file that
/// looks fine on both ends.
fn advance(entry: &mut Seen, size: u64) -> bool {
    if entry.stable == DONE {
        return false;
    }
    if entry.size != size {
        // Still being written. The count restarts: what matters is consecutive
        // unchanged scans, not how many times the file has been seen.
        entry.size = size;
        entry.stable = 1;
        return false;
    }
    entry.stable += 1;
    if entry.stable < STABLE_SCANS {
        return false;
    }
    entry.stable = DONE;
    true
}

pub async fn watch(ctx: &Ctx, args: WatchArgs, path_override: Option<&str>) -> CliResult {
    let dir = PathBuf::from(&args.directory);
    if !dir.is_dir() {
        return Err(CliError::NotFound(format!(
            "{} is not a directory",
            args.directory
        )));
    }
    if args.interval == 0 {
        return Err(CliError::Usage("interval must be at least 1 second".into()));
    }

    let mut seen: HashMap<PathBuf, Seen> = HashMap::new();
    // Everything already there is recorded as *already sent* unless the user
    // asked otherwise: pointing a watch at a full folder should not fire off
    // its entire contents.
    if !args.existing {
        for (path, size) in scan(&dir) {
            seen.insert(path, Seen { size, stable: DONE });
        }
    }

    ctx.line(&format!(
        "watching {} — sending new files to {}",
        ctx.bold(&args.directory),
        ctx.bold(&args.to)
    ));

    loop {
        for (path, size) in scan(&dir) {
            let entry = seen.entry(path.clone()).or_insert(Seen { size, stable: 0 });
            if !advance(entry, size) {
                continue;
            }

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ctx.line(&format!("sending {name}"));
            let sent = crate::commands::send_paths(
                ctx,
                SendArgs {
                    // A watch sends the moment a file settles; a delay here
                    // would leave finished files sitting unsent.
                    at: None,
                    paths: vec![path.to_string_lossy().into_owned()],
                    to: Some(args.to.clone()),
                    addr: None,
                },
                path_override,
            )
            .await;
            if let Err(e) = sent {
                // A failed send does not stop the watch: the folder is still
                // being watched, the peer may come back, and exiting would mean
                // a laptop closing its lid ends the automation.
                ctx.line(&ctx.dim(&format!("  {name} not sent: {e}")));
            }
        }
        tokio::time::sleep(Duration::from_secs(args.interval)).await;
    }
}

/// Top-level regular files and their sizes.
///
/// Not recursive: a watch that descended would fire on a directory tree being
/// copied in, one file at a time, in whatever order the OS happened to create
/// them — and there is no way to know when such a tree is complete.
fn scan(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // A file that is still zero bytes has nothing to send yet, and a
        // zero-length file is indistinguishable from one a writer has only just
        // created. Skipped until it has content.
        if meta.len() == 0 {
            continue;
        }
        out.push((e.path(), meta.len()));
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![b'x'; bytes]).unwrap();
        p
    }

    #[test]
    fn scan_reports_top_level_files_with_content() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", 3);
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        write(&dir.path().join("sub"), "b.txt", 3);
        std::fs::write(dir.path().join("empty.txt"), b"").unwrap();

        let found = scan(dir.path());
        assert_eq!(found.len(), 1, "scan saw more than the one real file");
        assert!(found[0].0.ends_with("a.txt"));
    }

    /// The property the whole design rests on: **a file is never sent while it
    /// is still growing.** A half-written file arrives looking perfectly fine
    /// on both ends, which is the worst kind of failure because nothing reports
    /// it.
    #[test]
    fn a_file_is_sent_only_after_it_stops_growing() {
        let mut e = Seen {
            size: 10,
            stable: 0,
        };
        assert!(!advance(&mut e, 10), "sent on first sight");
        assert!(!advance(&mut e, 20), "sent while still growing");
        assert!(!advance(&mut e, 30), "sent while still growing");
        assert!(advance(&mut e, 30), "never settled once it stopped growing");
    }

    #[test]
    fn a_settled_file_is_sent_exactly_once() {
        let mut e = Seen { size: 5, stable: 0 };
        assert!(!advance(&mut e, 5));
        assert!(advance(&mut e, 5), "never settled");
        for _ in 0..5 {
            assert!(!advance(&mut e, 5), "sent again on a later scan");
        }
    }

    #[test]
    fn a_file_that_grows_again_restarts_its_count() {
        // What matters is *consecutive* unchanged scans. Counting total
        // sightings would send a file that paused mid-copy.
        let mut e = Seen {
            size: 10,
            stable: 0,
        };
        assert!(!advance(&mut e, 10));
        assert!(!advance(&mut e, 99), "growth did not reset the count");
        assert!(advance(&mut e, 99));
    }

    #[test]
    fn a_file_present_before_the_watch_started_is_never_sent() {
        // Pointing a watch at a full folder must not fire off its contents.
        let mut e = Seen {
            size: 7,
            stable: DONE,
        };
        for _ in 0..5 {
            assert!(!advance(&mut e, 7));
        }
    }
}
