//! Rust → Dart event delivery. Dart registers one C callback (via
//! `NativeCallable.listener`); Rust invokes it with an owned JSON C-string that
//! Dart must free with `pb_free_string`. No polling anywhere.

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::RwLock;

use serde_json::{json, Value};

/// The C callback type Dart registers. Receives an owned `char*` (JSON) that
/// the callee frees via `pb_free_string`.
pub type EventCallback = extern "C" fn(*const c_char);

/// An `RwLock`, not a plain `Mutex`: `emit()` takes the shared (read) guard
/// and holds it for the entire callback invocation (see below), while
/// `set_callback(None)` takes the exclusive (write) guard. That makes
/// "clear the callback" block until every in-flight `emit()` has finished
/// calling it, which is what prevents a use-after-free if Dart tears down the
/// `NativeCallable` concurrently with a background emit.
static CALLBACK: RwLock<Option<EventCallback>> = RwLock::new(None);

/// Register (or clear with `None`) the event sink. Clearing blocks until any
/// `emit()` currently invoking the previous callback has returned.
pub fn set_callback(cb: Option<EventCallback>) {
    *CALLBACK.write().unwrap_or_else(|e| e.into_inner()) = cb;
}

/// Emit a pre-built event value to Dart, if a callback is registered. Ownership
/// of the string transfers to the callee (Dart frees it) — required because
/// `NativeCallable.listener` processes it asynchronously on the Dart isolate.
///
/// The read guard is held across the `cb(...)` call itself, not just the
/// pointer read: copying the pointer out and invoking it after the lock was
/// released would let a concurrent `set_callback(None)` (as part of
/// shutdown) race a teardown of the callback on the Dart side, invoking a
/// potentially-freed function pointer. Holding the guard across the call is
/// safe here because the registered callback (`NativeCallable.listener`) only
/// posts to the Dart isolate's port — it never blocks and never re-enters
/// `set_callback`/`emit`, so there is no deadlock risk.
pub fn emit(event: &Value) {
    let guard = CALLBACK.read().unwrap_or_else(|e| e.into_inner());
    if let Some(cb) = *guard {
        if let Ok(s) = CString::new(event.to_string()) {
            cb(s.into_raw());
        }
    }
}

/// Alias for [`emit`] used where a full event object is already assembled.
pub fn event(value: &Value) {
    emit(value);
}

/// Emit a transfer event with the standard envelope: `type`, `transfer_id`,
/// `timestamp`, `payload`.
pub fn transfer(id: &str, ty: &str, payload: Value) {
    emit(&json!({
        "type": ty,
        "transfer_id": id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": payload,
    }));
}
