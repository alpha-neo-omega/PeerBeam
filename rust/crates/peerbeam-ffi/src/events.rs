//! Rust → Dart event delivery. Dart registers one C callback (via
//! `NativeCallable.listener`); Rust invokes it with an owned JSON C-string that
//! Dart must free with `pb_free_string`. No polling anywhere.

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::Mutex;

use serde_json::Value;

/// The C callback type Dart registers. Receives an owned `char*` (JSON) that
/// the callee frees via `pb_free_string`.
pub type EventCallback = extern "C" fn(*const c_char);

static CALLBACK: Mutex<Option<EventCallback>> = Mutex::new(None);

/// Register (or clear with `None`) the event sink.
pub fn set_callback(cb: Option<EventCallback>) {
    *CALLBACK.lock().unwrap() = cb;
}

/// Emit an event to Dart, if a callback is registered. Ownership of the string
/// transfers to the callee (Dart frees it) — required because
/// `NativeCallable.listener` processes it asynchronously on the Dart isolate.
pub fn emit(event: &Value) {
    let cb = *CALLBACK.lock().unwrap();
    if let Some(cb) = cb {
        if let Ok(s) = CString::new(event.to_string()) {
            cb(s.into_raw());
        }
    }
}
