//! Rust → Dart event delivery. Dart registers one C callback (via
//! `NativeCallable.listener`); Rust invokes it with an owned JSON C-string that
//! Dart must free with `pb_free_string`. No polling anywhere.

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::Mutex;

use serde_json::{json, Value};

/// The C callback type Dart registers. Receives an owned `char*` (JSON) that
/// the callee frees via `pb_free_string`.
pub type EventCallback = extern "C" fn(*const c_char);

static CALLBACK: Mutex<Option<EventCallback>> = Mutex::new(None);

/// Register (or clear with `None`) the event sink.
pub fn set_callback(cb: Option<EventCallback>) {
    *CALLBACK.lock().unwrap() = cb;
}

/// Emit a pre-built event value to Dart, if a callback is registered. Ownership
/// of the string transfers to the callee (Dart frees it) — required because
/// `NativeCallable.listener` processes it asynchronously on the Dart isolate.
pub fn emit(event: &Value) {
    let cb = *CALLBACK.lock().unwrap();
    if let Some(cb) = cb {
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
