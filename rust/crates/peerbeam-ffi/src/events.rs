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

/// Emit a `chat_received` event carrying one persisted record. The handler
/// that decoded and persisted the record calls this only to notify — it does
/// not re-persist (the `ChatStore` write already happened in `ChatHandler`).
pub fn chat(rec: &peerbeam_chat::ChatRecord) {
    emit(&json!({
        "type": "chat_received",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message": {
            "id": rec.id,
            "peer_id": rec.peer_id,
            "direction": rec.direction,
            "timestamp": rec.timestamp,
            "body": rec.body,
            "status": rec.status,
        },
    }));
}

/// Additive PeerSession lifecycle event vocabulary. New `type` strings only —
/// existing consumers ignore unknown types, so this never breaks the callback
/// contract. Published so consumers (Dart) can subscribe additively; marked
/// `allow(dead_code)` until the session runtime wires emission.
#[allow(dead_code)]
pub mod kind {
    /// A PeerSession was established.
    pub const SESSION_CREATED: &str = "session_created";
    /// A PeerSession closed.
    pub const SESSION_CLOSED: &str = "session_closed";
    /// A PeerSession lost its transport and is reconnecting.
    pub const SESSION_RECOVERING: &str = "session_recovering";
    /// A PeerSession resumed after a reconnect.
    pub const SESSION_RESUMED: &str = "session_resumed";
    /// Recovery gave up; the session terminated.
    pub const RECOVERY_FAILED: &str = "recovery_failed";
    /// A channel opened on a session.
    pub const CHANNEL_OPENED: &str = "channel_opened";
    /// A channel closed.
    pub const CHANNEL_CLOSED: &str = "channel_closed";
    /// Version + capability negotiation completed.
    pub const CAPABILITY_NEGOTIATED: &str = "capability_negotiated";
}

/// Emit a session-scoped event: `type`, `session_id`, `timestamp`, `payload`.
#[allow(dead_code)]
pub fn session(session_id: &str, ty: &str, payload: Value) {
    emit(&json!({
        "type": ty,
        "session_id": session_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": payload,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_chat::{ChatRecord, Direction, Status};
    use std::ffi::CStr;
    use std::sync::Mutex;

    // Collects the raw JSON strings the callback receives, for this test only
    // (guarded by `#[serial_test::serial]` — `CALLBACK` is process-global, same
    // pattern as `lib.rs`'s `session_events_route_in_order_through_the_callback`).
    static COLLECTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn collect(ptr: *const c_char) {
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        // Free exactly as `pb_free_string` does (the callback owns the string).
        unsafe { drop(CString::from_raw(ptr as *mut c_char)) };
        COLLECTED.lock().unwrap().push(s);
    }

    #[test]
    #[serial_test::serial]
    fn chat_emits_chat_received_with_expected_shape() {
        COLLECTED.lock().unwrap().clear();
        set_callback(Some(collect));

        let rec = ChatRecord {
            id: "m1".to_string(),
            peer_id: "pb-bob".to_string(),
            direction: Direction::In,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            body: "hello".to_string(),
            status: Status::Received,
        };
        chat(&rec);

        set_callback(None);

        let got: Vec<Value> = COLLECTED
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(got.len(), 1);
        let v = &got[0];
        assert_eq!(v["type"], "chat_received");
        assert!(v["timestamp"].is_string(), "envelope carries a timestamp");
        assert_eq!(v["message"]["id"], "m1");
        assert_eq!(v["message"]["peer_id"], "pb-bob");
        assert_eq!(v["message"]["direction"], "in");
        assert_eq!(v["message"]["timestamp"], "2024-01-01T00:00:00Z");
        assert_eq!(v["message"]["body"], "hello");
        assert_eq!(v["message"]["status"], "received");
    }
}
