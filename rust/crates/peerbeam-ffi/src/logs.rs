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
    let buf = BUFFER.lock().unwrap();
    let body: String = buf
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, body).map_err(|e| (Code::Storage, format!("export logs: {e}")))?;
    Ok(json!({ "path": path, "count": buf.len() }))
}
