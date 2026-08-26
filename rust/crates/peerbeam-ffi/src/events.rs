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

/// Whether a callback is currently registered.
///
/// For asserting the difference between the two teardown paths: a re-init keeps
/// the caller's callback, a real shutdown clears it so the pointer can be freed.
#[cfg(test)]
#[must_use]
pub fn has_callback() -> bool {
    CALLBACK.read().unwrap_or_else(|e| e.into_inner()).is_some()
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

/// The JSON projection of one persisted chat record — the single shape every
/// surface sees, whether a record arrives live on a `chat_received` event or is
/// read back from `pb_chat_history`. Shared by both so the two can never drift
/// (a field visible in history but missing from the event, or the reverse, is
/// a bug that only shows up in one code path).
///
/// `kind`/`file` are additive: a text record serializes `kind: "text"` and a
/// null `file`, exactly as before for every consumer that ignores them.
pub fn record_dto(rec: &peerbeam_chat::ChatRecord) -> Value {
    json!({
        "id": rec.id,
        "peer_id": rec.peer_id,
        "direction": rec.direction,
        "timestamp": rec.timestamp,
        "body": rec.body,
        "status": rec.status,
        "kind": rec.kind,
        "file": rec.file,
        // Always present, `[]` when there are none, so a surface can render
        // reactions without distinguishing "no reactions" from "this build
        // does not report them".
        "reactions": rec.reactions,
        // Null unless the peer told us it read this message. Null covers both
        // "not read" and "this peer does not send receipts" — deliberately
        // indistinguishable, because a peer that opted out owes no explanation
        // and a surface must not imply one was withheld.
        "read_at": rec.read_at,
        // The message this one answers, or null. A *reference*, never a copy of
        // the quoted text: a snapshot would outlive the message it quoted, so a
        // disappearing-message window could be defeated by anyone replying to
        // something. A surface resolves it against the rows it already holds,
        // which is also why a parent that has gone renders as an orphan rather
        // than being quoted from somewhere else.
        "in_reply_to": rec.in_reply_to,
    })
}

/// Emit a `chat_received` event carrying one persisted record. The handler
/// that decoded and persisted the record calls this only to notify — it does
/// not re-persist (the `ChatStore` write already happened in `ChatHandler`).
pub fn chat(rec: &peerbeam_chat::ChatRecord) {
    emit(&json!({
        "type": "chat_received",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message": record_dto(rec),
    }));
}

/// Emit a `chat_status` event when a record's delivery status changes: `"sent"`
/// once a background flush delivers a previously-queued message (see
/// `Manager::chat_flush_peer` and the flush-on-connect path in
/// `handle_incoming`), and — for a file shared inside a conversation — the
/// terminal status its transfer settled on.
///
/// `status` is always the record's own serialized [`peerbeam_chat::Status`]
/// spelling, so a surface can apply it to the row without a second vocabulary.
pub fn chat_status(peer_id: &str, message_id: &str, status: &str) {
    chat_status_detail(peer_id, message_id, status, None);
}

/// [`chat_status`] plus a human-readable reason, for a status a user is owed an
/// explanation for — a file refused because the peer's build cannot receive
/// chat attachments, or a send that failed before any byte moved. The `error`
/// key is present only when there is something to say, so every existing
/// consumer of the plain event is unaffected.
pub fn chat_status_detail(peer_id: &str, message_id: &str, status: &str, error: Option<&str>) {
    let mut ev = json!({
        "type": "chat_status",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message_id": message_id,
        "peer_id": peer_id,
        "status": status,
    });
    if let (Value::Object(m), Some(e)) = (&mut ev, error) {
        m.insert("error".to_string(), Value::String(e.to_string()));
    }
    emit(&ev);
}

/// How far a file's staging copy has got: the same `chat_status` event, with
/// `status: "staging"` and one extra `progress` object.
///
/// **Deliberately not a new event type.** Staging is a status a row is *in*,
/// not a different kind of thing happening to it, and a surface already
/// subscribes to `chat_status` to move that row — a second type would mean
/// every surface learning a second vocabulary to render the same bubble, and
/// a surface that had not yet learned it would show a file frozen on
/// "Staging…" with no bar. `progress` is additive: a consumer that ignores it
/// sees exactly the `chat_status` event [`chat_status`] already emits.
///
/// `total` is the source's size as `begin_file_send` measured it, so
/// `done`/`total` is a determinate fraction. `done` may exceed it when the
/// source is being appended to while we copy (a log, a download still
/// running); a surface should clamp rather than assume, since the truth about
/// how many bytes exist is only settled when the copy ends.
///
/// **Callers must throttle.** Staging reports every 64 KiB — ~262,000 reports
/// for a 16 GiB file — and this function emits every time it is called. See
/// `StagingThrottle` in `transfer.rs`, which is what decides.
pub fn chat_staging(peer_id: &str, message_id: &str, done: u64, total: u64) {
    emit(&json!({
        "type": "chat_status",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "message_id": message_id,
        "peer_id": peer_id,
        "status": "staging",
        "progress": { "done": done, "total": total },
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
    #[test]
    fn record_dto_always_reports_reactions_even_when_there_are_none() {
        // The key must be present and empty rather than absent: a surface
        // reading it cannot otherwise tell "nobody reacted" from "this build
        // does not send reactions", and would have to guess.
        let rec = peerbeam_chat::ChatRecord {
            in_reply_to: None,
            stored_at: None,
            id: "m1".to_string(),
            peer_id: "pb-bob".to_string(),
            direction: peerbeam_chat::Direction::In,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            body: "hello".to_string(),
            status: peerbeam_chat::Status::Received,
            kind: peerbeam_chat::Kind::Text,
            file: None,
            read_at: None,
            reactions: Vec::new(),
        };
        let dto = record_dto(&rec);
        assert_eq!(dto["reactions"], serde_json::json!([]));
        assert!(
            dto["read_at"].is_null(),
            "unread must report null, not absent"
        );

        let rec = peerbeam_chat::ChatRecord {
            reactions: vec![peerbeam_chat::StoredReaction {
                emoji: "\u{1F44D}".to_string(),
                by: peerbeam_chat::Direction::Out,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            }],
            ..rec
        };
        let dto = record_dto(&rec);
        assert_eq!(dto["reactions"][0]["emoji"], "\u{1F44D}");
        assert_eq!(dto["reactions"][0]["by"], "out");
    }

    use super::*;
    use peerbeam_chat::{ChatRecord, Direction, Kind, Status};
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
            in_reply_to: None,
            stored_at: None,
            id: "m1".to_string(),
            peer_id: "pb-bob".to_string(),
            direction: Direction::In,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            body: "hello".to_string(),
            status: Status::Received,
            kind: Kind::Text,
            file: None,
            read_at: None,
            reactions: Vec::new(),
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

    /// Staging progress rides the EXISTING `chat_status` type — a surface that
    /// already routes those to a row needs no new event kind — and adds the
    /// `progress` object beside the fields that event always carries.
    #[test]
    #[serial_test::serial]
    fn chat_staging_is_a_chat_status_event_with_an_extra_progress_object() {
        COLLECTED.lock().unwrap().clear();
        set_callback(Some(collect));
        chat_staging("pb-bob", "m9", 4096, 8192);
        set_callback(None);

        let got: Vec<Value> = COLLECTED
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(got.len(), 1);
        let v = &got[0];
        assert_eq!(v["type"], "chat_status", "no new event kind");
        assert_eq!(v["peer_id"], "pb-bob");
        assert_eq!(v["message_id"], "m9");
        assert_eq!(v["status"], "staging");
        assert_eq!(v["progress"]["done"], 4096);
        assert_eq!(v["progress"]["total"], 8192);
        assert!(v["timestamp"].is_string());
    }

    /// …and the plain status event still carries no `progress`, so a consumer
    /// can tell "a row is staging, here is how far" from every other status
    /// change without a second event type.
    #[test]
    #[serial_test::serial]
    fn a_plain_chat_status_carries_no_progress_key() {
        COLLECTED.lock().unwrap().clear();
        set_callback(Some(collect));
        chat_status("pb-bob", "m9", "sent");
        set_callback(None);

        let v: Value =
            serde_json::from_str(&COLLECTED.lock().unwrap()[0]).expect("one status event");
        assert_eq!(v["type"], "chat_status");
        assert!(v["progress"].is_null(), "additive: absent unless staging");
        assert!(v["error"].is_null());
    }
}
