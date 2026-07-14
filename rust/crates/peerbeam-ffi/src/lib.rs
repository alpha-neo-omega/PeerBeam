//! PeerBeam FFI — a stable C-ABI bridge exposing the engine to Flutter.
//!
//! Design invariants:
//! - **Only strings + one callback pointer cross.** No domain/internal structs.
//! - **JSON DTOs** are the versioned wire contract ([`dto`]); every
//!   `char*`-returning function yields a result envelope ([`error`]).
//! - **Panic-safe:** every `extern "C"` function is `catch_unwind`-wrapped, so a
//!   Rust panic becomes a structured `internal` error, never UB across FFI.
//! - **Ownership:** Rust allocates every returned string; Dart frees it with
//!   [`pb_free_string`]. Dart allocates argument strings and frees them itself.
//! - **No bytes cross.** Files are referred to by path; streaming stays in Rust.

mod clipboard;
mod dto;
mod error;
mod events;
mod logs;
mod runtime;
mod settings;
mod status;
mod transfer;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use serde_json::{json, Value};

use error::Code;

/// ABI version. Bump on any breaking change to a function signature or the
/// envelope/DTO contract. Dart checks this at startup.
pub const ABI_VERSION: u32 = 1;

// ── string helpers ──────────────────────────────────────────────

/// Turn a value into an owned C string pointer (caller frees via
/// [`pb_free_string`]). Never returns null for valid JSON.
fn to_cstring(value: Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => CString::new(
            "{\"ok\":false,\"error\":{\"code\":\"internal\",\"message\":\"nul in json\"}}",
        )
        .unwrap()
        .into_raw(),
    }
}

/// Read a borrowed C string argument (null → empty).
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated UTF-8 string for the duration
/// of the call.
unsafe fn read_str(ptr: *const c_char) -> Result<String, (Code, String)> {
    if ptr.is_null() {
        return Ok(String::new());
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map(|s| s.to_string())
        .map_err(|_| (Code::InvalidArgument, "argument is not valid UTF-8".into()))
}

/// Run `body`, catching any panic and turning it into an `internal` envelope —
/// a panic must never unwind across the FFI boundary.
fn guard(body: impl FnOnce() -> Value) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    match result {
        Ok(value) => to_cstring(value),
        Err(_) => to_cstring(error::err(Code::Internal, "internal error (panic caught)")),
    }
}

// ── lifecycle / meta ────────────────────────────────────────────

/// Integer ABI version for a fast compatibility check.
#[no_mangle]
pub extern "C" fn pb_abi_version() -> u32 {
    ABI_VERSION
}

/// `{ "abi": <u32>, "semver": "<crate version>" }` (a bare object, not an
/// envelope — this call cannot fail).
#[no_mangle]
pub extern "C" fn pb_version_json() -> *mut c_char {
    to_cstring(json!({ "abi": ABI_VERSION, "semver": env!("CARGO_PKG_VERSION") }))
}

/// Initialise the engine. `config_json` may be empty for defaults.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_init(config_json: *const c_char) -> *mut c_char {
    guard(
        || match read_str(config_json).and_then(|s| runtime::init(&s)) {
            Ok(data) => error::ok(data),
            Err((code, msg)) => error::err(code, msg),
        },
    )
}

/// Stop all work and release the engine.
#[no_mangle]
pub extern "C" fn pb_shutdown() {
    let _ = std::panic::catch_unwind(runtime::shutdown);
}

/// Register (or clear, with a null pointer) the event callback.
#[no_mangle]
pub extern "C" fn pb_set_event_callback(cb: Option<events::EventCallback>) {
    events::set_callback(cb);
}

/// Free a string previously returned by any `pb_*` function or delivered to the
/// event callback.
///
/// # Safety
/// `ptr` must be a pointer returned by this library and not already freed.
#[no_mangle]
pub unsafe extern "C" fn pb_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

// ── discovery ───────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn pb_discovery_start() -> *mut c_char {
    guard(|| error::envelope(runtime::discovery_start()))
}

#[no_mangle]
pub extern "C" fn pb_discovery_stop() -> *mut c_char {
    guard(|| error::envelope(runtime::discovery_stop()))
}

/// Snapshot of the current merged device list.
#[no_mangle]
pub extern "C" fn pb_devices_json() -> *mut c_char {
    guard(|| error::envelope(runtime::devices()))
}

// ── transfer ────────────────────────────────────────────────────

/// Parse a JSON argument into a value.
unsafe fn read_json(ptr: *const c_char) -> Result<Value, (Code, String)> {
    let s = read_str(ptr)?;
    serde_json::from_str(&s).map_err(|e| (Code::InvalidArgument, format!("bad json: {e}")))
}

/// Parse an optional JSON argument; null/empty/invalid → empty object.
unsafe fn read_json_or_empty(ptr: *const c_char) -> Value {
    match read_str(ptr) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        _ => json!({}),
    }
}

/// Extract a required string `id` field.
fn id_of(v: &Value) -> Result<String, (Code, String)> {
    v.get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .ok_or((Code::InvalidArgument, "id required".into()))
}

/// Queue file(s) to a peer: `{peer:{name,addresses[],port}, paths:[…]}` → `{ids}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_send(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let v = read_json(json)?;
            runtime::manager()?.send(&v)
        })())
    })
}

/// Queue a folder to a peer: `{peer, path}` → `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_send_folder(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let v = read_json(json)?;
            runtime::manager()?.send_folder(&v)
        })())
    })
}

/// Pause a transfer: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_pause(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.pause(&id_of(&read_json(json)?)?))()))
}

/// Resume a transfer: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_resume(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.resume(&id_of(&read_json(json)?)?))()))
}

/// Cancel a transfer: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_cancel(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.cancel(&id_of(&read_json(json)?)?))()))
}

/// Accept an incoming transfer: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_accept(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.accept(&id_of(&read_json(json)?)?))()))
}

/// Reject an incoming transfer: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_reject(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.reject(&id_of(&read_json(json)?)?))()))
}

/// All active transfers with live stats.
#[no_mangle]
pub extern "C" fn pb_transfers_active() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.active_list())()))
}

/// One transfer by id: `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_get(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.get(&id_of(&read_json(json)?)?))()))
}

/// Completed-transfer history.
#[no_mangle]
pub extern "C" fn pb_history_get() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.history())()))
}

/// Clear all transfer history (persisted). Emits `history_updated`.
#[no_mangle]
pub extern "C" fn pb_history_clear() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.history_clear())()))
}

// ── trust ───────────────────────────────────────────────────────

/// Pinned (trusted) devices: `{devices:[{id,name,fingerprint,trusted_at}]}`.
#[no_mangle]
pub extern "C" fn pb_trust_list() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.trust_list())()))
}

/// Revoke a pinned device: `{id}` → `{removed}`. Emits `trust_changed`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_trust_remove(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.trust_remove(&read_json(json)?))()))
}

// ── clipboard ───────────────────────────────────────────────────

/// Current clipboard item, or `{item:null}`.
#[no_mangle]
pub extern "C" fn pb_clipboard_get() -> *mut c_char {
    guard(|| error::envelope(clipboard::get()))
}

/// Set the clipboard: `{text}` (auto-classified) or `{kind:"image",mime,size}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_clipboard_set(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| clipboard::set(&read_json(json)?))()))
}

/// Enable clipboard events (they flow through the event callback).
#[no_mangle]
pub extern "C" fn pb_clipboard_subscribe() -> *mut c_char {
    guard(|| error::envelope(clipboard::subscribe()))
}

// ── settings ────────────────────────────────────────────────────

/// Current settings (with trusted-devices list).
#[no_mangle]
pub extern "C" fn pb_settings_get() -> *mut c_char {
    guard(|| error::envelope(settings::get()))
}

/// Merge a partial settings object, persist, emit `settings_changed`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_settings_set(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| settings::set(&read_json(json)?))()))
}

/// Restore default settings.
#[no_mangle]
pub extern "C" fn pb_settings_reset() -> *mut c_char {
    guard(|| error::envelope(settings::reset()))
}

// ── daemon ──────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn pb_daemon_start() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.start_daemon())()))
}

#[no_mangle]
pub extern "C" fn pb_daemon_stop() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.stop_daemon())()))
}

#[no_mangle]
pub extern "C" fn pb_daemon_restart() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.restart_daemon())()))
}

#[no_mangle]
pub extern "C" fn pb_daemon_status() -> *mut c_char {
    guard(|| error::envelope((|| Ok(runtime::manager()?.daemon_status()))()))
}

// ── status ──────────────────────────────────────────────────────

/// Aggregate runtime status (runtime/build/devices/transfers/daemon/memory).
#[no_mangle]
pub extern "C" fn pb_status() -> *mut c_char {
    guard(|| error::envelope(runtime::status()))
}

// ── logs ────────────────────────────────────────────────────────

/// Recent structured logs: `{limit?}` → `{logs:[…]}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_logs_get(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope(logs::get(&read_json_or_empty(json))))
}

/// Toggle `log_received` event streaming: `{enabled:bool}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_logs_subscribe(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope(logs::subscribe(&read_json_or_empty(json))))
}

/// Export buffered logs to a file: `{path?}` → `{path,count}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_logs_export(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope(logs::export(&read_json_or_empty(json))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read + free a `pb_*` return value as JSON.
    fn take(ptr: *mut c_char) -> Value {
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        unsafe { pb_free_string(ptr) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn version_reports_abi() {
        assert_eq!(pb_abi_version(), ABI_VERSION);
        let v = take(pb_version_json());
        assert_eq!(v["abi"], ABI_VERSION);
        assert!(v["semver"].is_string());
    }

    #[test]
    #[serial_test::serial]
    fn calls_before_init_error_cleanly() {
        pb_shutdown(); // ensure clean state
        let v = take(pb_devices_json());
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "not_initialised");
    }

    #[test]
    #[serial_test::serial]
    fn init_then_list_devices() {
        let v = take(unsafe { pb_init(std::ptr::null()) });
        assert_eq!(v["ok"], true, "init with defaults: {v}");
        let v = take(pb_devices_json());
        assert_eq!(v["ok"], true);
        assert!(v["data"]["devices"].is_array());
        pb_shutdown();
    }

    #[test]
    #[serial_test::serial]
    fn bad_config_is_invalid_argument() {
        let bad = CString::new("{ not json ]").unwrap();
        let v = take(unsafe { pb_init(bad.as_ptr()) });
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "invalid_argument");
    }
}
