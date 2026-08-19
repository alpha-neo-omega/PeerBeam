//! The FFI's view of log capture.
//!
//! The buffer, the `tracing` layer and the file live in `peerbeam-logs`, which
//! every frontend can depend on. This module is the adapter: it installs the
//! layer, points streamed lines at the FFI event channel, and maps results into
//! the FFI's error type. It deliberately holds no state of its own — the CLI
//! reads the same logs through the same crate, and two copies of a ring buffer
//! would be two answers to one question.

use serde_json::{json, Value};

use crate::error::Code;
use crate::events;

type Op = Result<Value, (Code, String)>;

/// Install the capture layer once, streaming lines as `log_received` events.
///
/// Ignores failure if a global subscriber is already set: log capture degrades
/// to nothing rather than crashing an engine that is otherwise fine.
pub fn install(filter: &str) {
    use std::sync::Once;
    use tracing_subscriber::prelude::*;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        peerbeam_logs::set_sink(std::sync::Arc::new(|entry| {
            events::event(&json!({
                "type": "log_received",
                "log": entry,
            }));
        }));
        // **Filtered, or the buffer is worthless.** Without this the layer
        // captured every event from every dependency at every level: one QUIC
        // handshake emits hundreds of `TRACE` lines from quinn and rustls, so
        // the 500-entry ring filled in milliseconds and anything the engine
        // itself said was evicted before a person could read it. The log file
        // got the same flood, rotating through megabytes of transport trace.
        //
        // `log.filter` (default `peerbeam=info`) already existed to say what
        // should be recorded; it simply was not being applied here.
        let env = tracing_subscriber::EnvFilter::try_new(filter)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("peerbeam=info"));
        let _ = tracing_subscriber::registry()
            .with(peerbeam_logs::CaptureLayer.with_filter(env))
            .try_init();
    });
}

/// Begin writing logs to `path` as well as holding them in memory.
///
/// A failure to open the file is reported but never fatal — losing the file
/// copy must not stop the engine that was trying to write it.
pub fn set_file(path: Option<std::path::PathBuf>) -> bool {
    peerbeam_logs::set_file(path)
}

/// Recent lines: `{limit?}` → `{logs:[…]}`.
pub fn get(req: &Value) -> Op {
    let limit = req
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .min(peerbeam_logs::CAPACITY as u64) as usize;
    Ok(peerbeam_logs::get(limit))
}

/// Toggle `log_received` event streaming: `{enabled:bool}`.
pub fn subscribe(req: &Value) -> Op {
    let enabled = req.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    Ok(peerbeam_logs::subscribe(enabled))
}

/// Export buffered logs to a file: `{path?}` → `{path,count}`.
pub fn export(req: &Value) -> Op {
    let path = req
        .get("path")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("peerbeam-logs-{}.jsonl", std::process::id()))
        });
    peerbeam_logs::export(&path).map_err(|e| (Code::Internal, format!("exporting logs: {e}")))
}
