//! Structured log capture: a bounded in-memory ring, and an optional file.
//!
//! # Why this is its own crate
//!
//! The buffer used to live inside `peerbeam-ffi`. That made it reachable from
//! Flutter and from nothing else — the CLI would have had to depend on the
//! Flutter binding layer to read its own logs, which is backwards. Logs are a
//! core concern that every frontend needs, so they live in a crate every
//! frontend can depend on.
//!
//! # Why the ring alone was not enough
//!
//! The ring is per-process and in-memory. That is exactly right for a
//! long-running engine, where "show me the last 200 lines" is a live question.
//! It is useless for a one-shot command: `peerbeam logs` in a fresh process
//! would read its *own* empty buffer and print nothing, while looking like a
//! working feature. So logs can also be appended to a file, which is what makes
//! them readable across processes and after a crash.
//!
//! # What is never written
//!
//! Whatever the engine chooses to log — this crate adds no fields of its own
//! beyond a timestamp, level and target. It is a transport, not a policy: the
//! rule that sensitive values never reach a log is enforced where the log line
//! is written, because that is the only place that knows what the value means.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Lines held in memory. Older entries drop.
pub const CAPACITY: usize = 500;

/// How large the log file may grow before it is rotated.
///
/// One rotation, not many: logs are for diagnosing a problem you have now, and
/// an ever-growing archive of everything a device ever did is a privacy
/// liability rather than a feature.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

static BUFFER: Mutex<VecDeque<Value>> = Mutex::new(VecDeque::new());
static EMIT: AtomicBool = AtomicBool::new(false);
static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Somewhere to stream each captured line as it happens.
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();

/// Record one line. Public so a frontend can log through the same path.
pub fn record(entry: Value) {
    {
        let mut buf = BUFFER.lock().unwrap();
        if buf.len() == CAPACITY {
            buf.pop_front();
        }
        buf.push_back(entry.clone());
    }
    if let Some(path) = FILE.lock().unwrap().as_ref() {
        append(path, &entry);
    }
    if EMIT.load(Ordering::Relaxed) {
        if let Some(sink) = SINK.get() {
            sink(entry);
        }
    }
}

/// Point file logging at `path`, creating its directory.
///
/// Returns whether the file could be opened. A failure is **not** fatal and not
/// an error to the caller: losing the file copy of the logs must never stop the
/// engine that was trying to write them.
pub fn set_file(path: Option<PathBuf>) -> bool {
    let Some(path) = path else {
        *FILE.lock().unwrap() = None;
        return true;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let ok = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .is_ok();
    if ok {
        *FILE.lock().unwrap() = Some(path);
    }
    ok
}

/// Install a sink for streamed lines. Only the first call takes effect.
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

/// Turn streaming on or off.
pub fn subscribe(enabled: bool) -> Value {
    EMIT.store(enabled, Ordering::Relaxed);
    json!({ "subscribed": enabled })
}

/// The most recent `limit` lines held in memory.
#[must_use]
pub fn get(limit: usize) -> Value {
    let buf = BUFFER.lock().unwrap();
    let start = buf.len().saturating_sub(limit);
    let logs: Vec<Value> = buf.iter().skip(start).cloned().collect();
    json!({ "logs": logs })
}

/// The most recent `limit` lines from the log **file**, newest last.
///
/// This is what a one-shot command reads: a fresh process has an empty ring,
/// and answering from it would print nothing while looking like it worked.
pub fn read_file(path: &Path, limit: usize) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return json!({ "logs": [] });
    };
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let start = lines.len().saturating_sub(limit);
    json!({ "logs": lines[start..].to_vec() })
}

/// Write the buffered lines to `path`, returning where they went.
pub fn export(path: &Path) -> std::io::Result<Value> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let buf = BUFFER.lock().unwrap();
    let mut f = std::fs::File::create(path)?;
    for entry in buf.iter() {
        writeln!(f, "{entry}")?;
    }
    Ok(json!({ "path": path.to_string_lossy(), "count": buf.len() }))
}

/// Forget everything held in memory.
pub fn clear() {
    BUFFER.lock().unwrap().clear();
}

/// Append one line, rotating first if the file has grown past its bound.
fn append(path: &Path, entry: &Value) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= MAX_FILE_BYTES) {
        // One generation kept. A rename is atomic, so a reader either sees the
        // old file or the new one, never a half-truncated mix.
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{entry}");
    }
}

/// The `tracing` layer that feeds [`record`].
pub struct CaptureLayer;

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        record(json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "level": meta.level().to_string(),
            "target": meta.target(),
            "message": visitor.message,
        }));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: &str) -> Value {
        json!({ "at": "t", "level": "INFO", "target": "test", "message": msg })
    }

    // **`#[serial]`, not a comment saying "serialised".** These tests share one
    // process-global buffer: run in parallel, seven of the eight fail by
    // clobbering each other's state. An earlier version of this module claimed
    // serialisation in prose and relied on `--test-threads=1`, which nobody
    // passes and CI would not have used.
    #[test]
    #[serial_test::serial]
    fn the_buffer_bounds_itself_and_keeps_the_newest() {
        clear();
        for i in 0..(CAPACITY + 50) {
            record(line(&format!("line {i}")));
        }
        let out = get(CAPACITY * 2);
        let logs = out["logs"].as_array().unwrap();
        assert_eq!(logs.len(), CAPACITY, "the ring grew past its bound");
        // The oldest were dropped, not the newest.
        assert_eq!(
            logs.last().unwrap()["message"],
            format!("line {}", CAPACITY + 49)
        );
        clear();
    }

    #[test]
    #[serial_test::serial]
    fn get_returns_the_most_recent_lines_not_the_first() {
        clear();
        for i in 0..10 {
            record(line(&format!("{i}")));
        }
        let logs = get(3)["logs"].as_array().unwrap().clone();
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0]["message"], "7");
        assert_eq!(logs[2]["message"], "9");
        clear();
    }

    #[test]
    #[serial_test::serial]
    fn exporting_writes_one_json_object_per_line() {
        clear();
        record(line("first"));
        record(line("second"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        let out = export(&path).unwrap();
        assert_eq!(out["count"], 2);

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1]["message"], "second");
        clear();
    }

    /// **What makes a one-shot command possible.** A fresh process has an empty
    /// ring; reading the file is the only way `peerbeam logs` can show anything
    /// at all.
    #[test]
    #[serial_test::serial]
    fn lines_written_to_the_file_are_readable_by_another_process() {
        clear();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("peerbeam.jsonl");
        assert!(set_file(Some(path.clone())));
        record(line("persisted"));
        set_file(None);

        // The ring is irrelevant here — this is what a *different* process sees.
        clear();
        let out = read_file(&path, 10);
        let logs = out["logs"].as_array().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["message"], "persisted");
    }

    #[test]
    #[serial_test::serial]
    fn reading_a_missing_log_file_is_empty_rather_than_an_error() {
        let out = read_file(Path::new("/definitely/not/here.jsonl"), 10);
        assert!(out["logs"].as_array().unwrap().is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn a_file_that_cannot_be_opened_does_not_stop_logging() {
        // Losing the file copy must never stop the engine trying to write it.
        clear();
        assert!(!set_file(Some(PathBuf::from("/proc/nope/deeper/x.jsonl"))));
        record(line("still buffered"));
        assert_eq!(get(10)["logs"].as_array().unwrap().len(), 1);
        clear();
    }

    #[test]
    #[serial_test::serial]
    fn the_file_rotates_rather_than_growing_without_bound() {
        clear();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peerbeam.jsonl");
        // Pre-fill past the bound so the next append rotates.
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(set_file(Some(path.clone())));
        record(line("after rotation"));
        set_file(None);

        assert!(
            path.with_extension("jsonl.1").exists(),
            "the oversized file was not rotated aside"
        );
        let out = read_file(&path, 10);
        assert_eq!(
            out["logs"].as_array().unwrap().len(),
            1,
            "the new file should hold only what came after"
        );
        clear();
    }

    #[test]
    #[serial_test::serial]
    fn streaming_is_off_until_asked_for() {
        clear();
        let seen = Arc::new(Mutex::new(0usize));
        let s = seen.clone();
        set_sink(Arc::new(move |_| *s.lock().unwrap() += 1));

        record(line("unwatched"));
        assert_eq!(
            *seen.lock().unwrap(),
            0,
            "a line streamed before subscribing"
        );

        subscribe(true);
        record(line("watched"));
        assert_eq!(*seen.lock().unwrap(), 1);

        subscribe(false);
        record(line("unwatched again"));
        assert_eq!(
            *seen.lock().unwrap(),
            1,
            "unsubscribing did not stop the stream"
        );
        clear();
    }
}
