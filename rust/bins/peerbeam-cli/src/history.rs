//! Persistent transfer history for the CLI.
//!
//! Same JSON schema and bound as the FFI engine's history
//! (`{id, direction, peer, file, path, bytes, success, at}` in
//! `<data_dir>/history.json`, newest last, capped), so tooling can read
//! either uniformly.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most recent entries kept on disk.
const MAX_HISTORY: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// "sending" | "receiving".
    pub direction: String,
    pub peer: String,
    pub file: String,
    /// Local path of the item (source for sends, saved location for receives).
    #[serde(default)]
    pub path: String,
    pub bytes: u64,
    pub success: bool,
    /// RFC 3339 timestamp.
    pub at: String,
}

/// History file under the engine's data directory.
pub fn path_for(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("history.json")
}

/// All entries, oldest first (empty on missing/corrupt file).
pub fn load(path: &Path) -> Vec<Entry> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Append one entry (bounded) — best-effort: history must never fail a
/// transfer, so errors are swallowed.
pub fn record(path: &Path, entry: Entry) {
    let mut entries = load(path);
    entries.push(entry);
    if entries.len() > MAX_HISTORY {
        let drop = entries.len() - MAX_HISTORY;
        entries.drain(..drop);
    }
    record_save(path, &entries);
}

/// Remove all entries, reporting whether the file was actually rewritten.
///
/// **The result is not decoration.** `history --clear` is what somebody runs
/// before handing a machine to someone else; reporting success over a write
/// that failed — a read-only volume, a full disk, a permissions change — leaves
/// them believing a record is gone when it is still on disk. That is the one
/// failure on this command that must never be quiet.
pub fn clear(path: &Path) -> std::io::Result<()> {
    save(path, &[])
}

/// Recording is best-effort and stays that way: a transfer must not fail
/// because its history line could not be written.
fn record_save(path: &Path, entries: &[Entry]) {
    let _ = save(path, entries);
}

fn save(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}

/// A ready-to-record entry stamped with "now".
pub fn entry(
    direction: &str,
    peer: &str,
    file: &str,
    path: &str,
    bytes: u64,
    success: bool,
) -> Entry {
    Entry {
        id: format!(
            "cli-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ),
        direction: direction.to_string(),
        peer: peer.to_string(),
        file: file.to_string(),
        path: path.to_string(),
        bytes,
        success,
        at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_load_clear_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = path_for(dir.path().to_str().unwrap());

        assert!(load(&p).is_empty());
        record(&p, entry("sending", "Bob", "a.bin", "/tmp/a.bin", 42, true));
        record(
            &p,
            entry("receiving", "Ann", "b.bin", "/tmp/b.bin", 7, false),
        );

        let got = load(&p);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].peer, "Bob");
        assert!(got[0].success);
        assert_eq!(got[1].direction, "receiving");
        assert!(!got[1].success);

        clear(&p).expect("clearing a writable history file must succeed");
        assert!(load(&p).is_empty());
    }

    /// **A clear that did not happen must not report success.**
    ///
    /// `history --clear` is what somebody runs before handing a machine to
    /// someone else. This used to return unit and swallow the write error, so a
    /// read-only volume, a full disk or a permissions change produced "history
    /// cleared" and exit 0 over a file that was still there — and a script had
    /// no way to notice.
    ///
    /// A directory standing where the file should be is the cheapest way to
    /// make the write fail on every platform.
    #[test]
    fn a_clear_that_cannot_write_reports_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("history.json");
        std::fs::create_dir(&blocked).expect("a directory where the file goes");

        let err = clear(&blocked).expect_err("writing over a directory must fail");
        assert!(
            !format!("{err}").is_empty(),
            "the failure must carry something the caller can show"
        );
    }

    #[test]
    fn bounded_to_max() {
        let dir = tempfile::tempdir().unwrap();
        let p = path_for(dir.path().to_str().unwrap());
        for i in 0..(MAX_HISTORY + 25) {
            record(&p, entry("sending", "x", &format!("f{i}"), "", 1, true));
        }
        let got = load(&p);
        assert_eq!(got.len(), MAX_HISTORY);
        assert_eq!(got.last().unwrap().file, format!("f{}", MAX_HISTORY + 24));
    }
}
