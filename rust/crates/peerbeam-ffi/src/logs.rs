//! Structured log capture. A `tracing` layer records engine logs into a bounded
//! ring buffer and (when subscribed) streams them as `log_received` events.
//! `pb_logs_get`/`_subscribe`/`_export` read them out.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};

use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

use crate::error::Code;
use crate::events;

/// Ring-buffer capacity (older entries drop).
const CAP: usize = 500;

static BUFFER: Mutex<VecDeque<Value>> = Mutex::new(VecDeque::new());
static EMIT: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();

type Op = Result<Value, (Code, String)>;

/// Install the capture layer once. Ignores failure if a global subscriber is
/// already set (log capture degrades gracefully, no crash).
pub fn install() {
    INSTALL.call_once(|| {
        let _ = tracing_subscriber::registry().with(CaptureLayer).try_init();
    });
}

struct CaptureLayer;

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        let record = json!({
            "level": level_str(*meta.level()),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "source": meta.target(),
            "component": meta.target().split("::").next().unwrap_or(meta.target()),
            "message": visitor.0,
        });

        {
            let mut buf = BUFFER.lock().unwrap();
            if buf.len() >= CAP {
                buf.pop_front();
            }
            buf.push_back(record.clone());
        }
        if EMIT.load(Ordering::Relaxed) {
            events::emit(&json!({
                "type": "log_received",
                "timestamp": record["timestamp"].clone(),
                "payload": { "log": record },
            }));
        }
    }
}

/// Captures a tracing event's `message` field.
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

fn level_str(l: Level) -> &'static str {
    match l {
        Level::ERROR => "error",
        Level::WARN => "warning",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// The most recent `limit` log records (default 100).
pub fn get(req: &Value) -> Op {
    let limit = req.get("limit").and_then(|l| l.as_u64()).unwrap_or(100) as usize;
    let buf = BUFFER.lock().unwrap();
    let start = buf.len().saturating_sub(limit);
    let logs: Vec<Value> = buf.iter().skip(start).cloned().collect();
    Ok(json!({ "logs": logs }))
}

/// Toggle `log_received` event streaming.
pub fn subscribe(req: &Value) -> Op {
    let enabled = req.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
    EMIT.store(enabled, Ordering::Relaxed);
    Ok(json!({ "subscribed": enabled }))
}

/// Export the buffered logs to a file (`{path}` or a temp path). Returns the
/// path written.
pub fn export(req: &Value) -> Op {
    let path = req
        .get("path")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("peerbeam-logs-{}.jsonl", std::process::id()))
                .to_string_lossy()
                .into_owned()
        });
    // Drain the buffer under the lock, then release it *before* the blocking
    // write: holding the mutex across `std::fs::write` would stall every
    // concurrent `CaptureLayer::on_event` (engine logging) for the duration
    // of a slow/large export, and — since `std::sync::Mutex` is
    // non-reentrant — would deadlock if a tracing event were ever emitted on
    // this same thread during the write.
    let (body, count) = {
        let buf = BUFFER.lock().unwrap();
        (
            buf.iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            buf.len(),
        )
    };
    std::fs::write(&path, body).map_err(|e| (Code::Storage, format!("export logs: {e}")))?;
    Ok(json!({ "path": path, "count": count }))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// `export()` must release `BUFFER`'s lock before the blocking
    /// `std::fs::write`, not hold it across the write.
    ///
    /// Proven by writing to a FIFO with no reader attached yet: opening a
    /// FIFO for writing blocks in the kernel until a reader shows up, giving
    /// a deterministic window during which a concurrent `BUFFER.lock()` must
    /// already succeed if (and only if) the fix is in place. Every wait is
    /// bounded so a regression fails the test instead of hanging the suite.
    #[test]
    fn export_releases_buffer_lock_before_the_blocking_write() {
        {
            let mut buf = BUFFER.lock().unwrap();
            buf.clear();
            buf.push_back(json!({ "level": "info", "message": "one" }));
            buf.push_back(json!({ "level": "info", "message": "two" }));
        }

        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("export.fifo");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().into_owned()).unwrap();
        assert_eq!(
            unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) },
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let export_path = fifo.clone();
        let export_thread =
            std::thread::spawn(move || export(&json!({ "path": export_path.to_string_lossy() })));

        // The export thread is now blocked opening the FIFO for write (no
        // reader exists yet) — std::fs::write reached that point only after
        // the scoped block above already dropped the BUFFER guard. Confirm
        // BUFFER is free while the writer sits blocked.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = tx.send(BUFFER.try_lock().is_ok());
        });
        let lock_was_free = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("probe thread did not report back in time");
        assert!(
            lock_was_free,
            "BUFFER was still locked while export()'s write was blocked on the FIFO"
        );

        // Unblock the writer (pairs with its blocked open) and confirm the
        // export itself still completes correctly.
        let content = std::fs::read_to_string(&fifo).expect("read fifo");
        let result = export_thread
            .join()
            .expect("export thread panicked")
            .expect("export returned an error");
        assert_eq!(result["count"], 2);
        assert_eq!(content.lines().count(), 2);
    }
}
