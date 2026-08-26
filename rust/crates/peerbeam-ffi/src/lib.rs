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

mod browse;
mod chat_receipts;
mod clipboard;
mod dto;
mod error;
mod events;
mod hook;
mod logs;
mod notes_sync;
mod presence;
mod resume;
mod rules;
mod runtime;
mod session;
mod session_exec;
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

/// `{ "abi": <u32>, "semver": "<crate version>", "features": [...] }` (a bare
/// object, not an envelope — this call cannot fail).
///
/// `abi` stays `1`: M8 is **purely additive** (new `pb_*` functions and event
/// types), so no existing signature or DTO changed. New capability is advertised
/// through the additive `features` array, which old frontends ignore.
#[no_mangle]
pub extern "C" fn pb_version_json() -> *mut c_char {
    to_cstring(json!({
        "abi": ABI_VERSION,
        "semver": env!("CARGO_PKG_VERSION"),
        "features": ["peersession_diagnostics", "transport_diagnostics", "session_events", "presence", "resumable_transfers"],
    }))
}

/// Ask whether a newer release exists. **Requires an explicit request.**
///
/// The only outbound request this app makes to anything but a peer, permitted
/// by amendment A1 in `docs/ARCHITECTURAL_INVARIANTS.md` on terms this function
/// has to keep: it runs when a person asks and never on a timer or at startup;
/// it sends no device id, install id, or anything identifying; and the answer is
/// a version string that nothing acts on. There is no download and no install.
///
/// Being unable to reach the feed is reported as `reachable: false`, not as an
/// error — offline is an ordinary state for this app, and nothing here is
/// allowed to become a precondition for using it.
#[no_mangle]
pub extern "C" fn pb_check_updates() -> *mut c_char {
    guard(|| {
        let current = env!("CARGO_PKG_VERSION");
        error::envelope(Ok(match runtime::block_on(peerbeam_update::check()) {
            Ok(Some(r)) => json!({
                "reachable": true,
                "current": current,
                "latest": r.version,
                "update_available": peerbeam_update::is_newer(&r.version, current),
                "url": r.url,
            }),
            Ok(None) => json!({
                "reachable": true,
                "current": current,
                "latest": Value::Null,
                "update_available": false,
            }),
            Err(e) => json!({
                "reachable": false,
                "current": current,
                "reason": e.to_string(),
            }),
        }))
    })
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

/// The optional `confirmed` flag on an accept — the user's explicit "the two
/// pairing codes match".
///
/// **Only a literal `true` counts.** Absent, `null`, `false`, `"true"`, `1` —
/// all confirm nothing. A caller that has not been taught this prompt cannot
/// accidentally satisfy it, which is the same reason the CLI's gate treats
/// everything that is not an explicit `y` as a decline.
fn confirmed_of(v: &Value) -> bool {
    v.get("confirmed") == Some(&Value::Bool(true))
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

/// Accept an incoming transfer: `{id, confirmed?}`. One-time only — does not
/// trust the sending device.
///
/// `confirmed` is the optional first-contact pairing answer: `true` means the
/// user compared this session's pairing code against the other device's screen
/// and they match. It matters only when the sending device was pinned by this
/// very handshake *and* `require_pairing_confirmation` is on; absent or `false`
/// then refuses the accept and leaves the transfer pending, so the user can
/// verify and accept again. Everywhere else it is ignored.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_accept(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let v = read_json(json)?;
            runtime::manager()?.accept(&id_of(&v)?, confirmed_of(&v))
        })())
    })
}

/// Accept an incoming transfer AND trust the sending device:
/// `{id, confirmed?}`. Future transfers from it are auto-accepted whenever
/// auto-accept is enabled.
///
/// `confirmed` means what it does on [`pb_transfer_accept`], and is gated the
/// same way.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_accept_trust(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let v = read_json(json)?;
            runtime::manager()?.accept_trust(&id_of(&v)?, confirmed_of(&v))
        })())
    })
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

/// Every **interrupted** transfer — one whose checkpoint outlived it — newest
/// first: `{transfers:[{id,direction,peer_id,file,path,status:"interrupted",
/// started_at,is_resume,resumable,stats}]}`.
///
/// Deliberately separate from [`pb_transfers_active`]: these are not running,
/// nothing will ever emit a progress event for one, and `resumable` is false
/// for the inbound half of them (see [`pb_transfer_resume_interrupted`]). A
/// surface merges the two lists knowing which is which; folding them into one
/// call would make "is this thing alive?" a field nobody remembers to read.
#[no_mangle]
pub extern "C" fn pb_transfers_interrupted() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.interrupted_list())()))
}

/// Restart an interrupted transfer from its checkpoint: `{id, peer?}` →
/// `{id,resumed}`.
///
/// **Not [`pb_transfer_resume`]**, which un-pauses a transfer this process is
/// already running. These are two different verbs and they deliberately do not
/// share a name: one needs a live transfer and fails without one, the other
/// needs a checkpoint and a peer to dial, and a surface that called the wrong
/// one would get a confusing error at best and the wrong action at worst.
///
/// Verifies the checkpoint still binds to its transfer — peer, file name and
/// size, against the source file as it is on disk *now* — before anything is
/// queued. Refuses an **incoming** transfer with `unsupported`: the transfer
/// protocol is sender-driven, so an interrupted receive resumes when its
/// sender offers it again, and it keeps its consent and its partial bytes
/// until then.
///
/// `peer` is optional and carries only *how to reach* the device — the same
/// `{id,name,addresses,port}` object [`pb_transfer_send`] takes — for a surface
/// that already has it in front of the user. Its `id` must be the checkpoint's
/// or the call is refused, so it can never redirect a resume at a different
/// device. Omit it and the addresses come from live discovery.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_resume_interrupted(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let v = read_json(json)?;
            runtime::manager()?.resume_interrupted(&v)
        })())
    })
}

/// Forget an interrupted transfer and the partial bytes it was holding:
/// `{id}` → `{discarded,partial_removed}`.
///
/// Without this an interrupted transfer is permanent clutter: it cannot
/// complete, no event will ever clear it, and its `.part` file sits beside the
/// destination forever. Refuses while a transfer is running under the same id
/// — cancel that instead.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_transfer_discard_interrupted(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let id = id_of(&read_json(json)?)?;
            runtime::manager()?.discard_interrupted(&id)
        })())
    })
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

/// Grant or withhold one per-device permission:
/// `{id, permission, granted}` → `{changed}`. Emits `trust_changed`.
///
/// `permission` is a name — `files`, `chat`, `clipboard`, `presence`, `pipe` —
/// as listed in each device's `permissions` array from `pb_trust_list`. An
/// unknown name is `invalid_argument`, so a surface built against a newer
/// engine is told rather than silently ignored.
///
/// Takes effect on the **next** operation, not the next reconnect: every gate
/// re-reads the trust store per message, clip, heartbeat and accept.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_trust_set_permission(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.trust_set_permission(&read_json(json)?)
        })())
    })
}

// ── presence ────────────────────────────────────────────────────

/// Live device presence:
/// `{sharing:bool, self:{…}, devices:[{device_id, …, received_at, age_seconds}]}`.
///
/// A peer that shares nothing appears with its `device_id` and timing only —
/// the status keys are **omitted**, never `null` or `0`, so a surface can tell
/// "no battery" from "0% battery". `self` is what this device *would* share, so
/// a user can see what the opt-in reveals before turning it on; it is not on
/// the wire while `sharing` is false.
#[no_mangle]
pub extern "C" fn pb_presence_json() -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.presence_snapshot())()))
}

/// Push a platform-supplied battery reading: `{percent?, charging?}` → `{ok}`.
///
/// For Android, whose `BatteryManager` the Rust platform layer cannot reach —
/// the Flutter side reads it over the existing `peerbeam/android` method
/// channel and hands it down here. Omitting `percent` clears the reading, so a
/// surface that loses battery access stops asserting a stale one. An
/// out-of-range value is ignored rather than clamped.
///
/// This only changes *what* would be shared; it does not share anything. The
/// opt-in setting and the trusted-only gate are unaffected.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_presence_battery(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            let req = read_json(json)?;
            let percent = req
                .get("percent")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u8::try_from(p).ok());
            let charging = req.get("charging").and_then(serde_json::Value::as_bool);
            presence::set_battery(percent, charging);
            Ok(serde_json::json!({ "ok": true }))
        })())
    })
}

// ── clipboard sync ──────────────────────────────────────────────

/// Push the clipboard to trusted peers: `{text, peers:[{name,addresses[],port}]}`
/// → `{queued, sync}`.
///
/// Called by the desktop watcher when the user copies something. `sync: false`
/// with `queued: 0` means the opt-in setting is off, and nothing was dialed —
/// off is silent, not merely undelivered.
///
/// Refuses an empty or over-cap clip with `invalid_argument` **before**
/// dialing anything, so a surface can say "too large to sync" rather than
/// leaving the user to wonder. An over-cap clip is never truncated: a
/// shortened clipboard silently corrupts what the user believes they copied.
///
/// Naming a peer here does not send to it. The trusted-only rule and the
/// peer's negotiated capability are still checked per peer, after the
/// handshake, by `peerbeam_clipboard::may_share_clip` — an untrusted device
/// listed here is sent nothing.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_clipboard_sync(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.clipboard_sync(&read_json(json)?))()))
}

// ── chat ────────────────────────────────────────────────────────

/// Send a chat message: `{peer:{name,addresses[],port},text}` → `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_send(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_send(&read_json(json)?))()))
}

/// Share a file inside a chat thread: `{peer:{name,addresses[],port},path}` →
/// `{id}`. The id names both the conversation row and the transfer carrying the
/// bytes. Online only: an unreachable peer fails (via `chat_status`) rather
/// than queueing.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_send_file(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_send_file(&read_json(json)?))()))
}

/// Conversation history: `{peer_id}` → `{messages:[...]}`. A pure read.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_history(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_history(&read_json(json)?))()))
}

/// Search this device's stored chat history:
/// `{query, limit?}` → `{hits:[{peer_id,message_id,timestamp,direction,kind,snippet}], truncated, limit}`.
///
/// **A pure local read.** Nothing goes on the wire, no peer is dialled, and no
/// peer can observe that it happened — it reads the same conversation
/// namespaces `pb_chat_history` reads. A thread whose device is long gone is
/// searchable; one the user deleted is not.
///
/// `query` matches **case-insensitively** as a plain substring of a message's
/// text body or a file message's *name*. A file's `local_path` is never
/// searched: that is this device's filesystem layout, not conversation content.
/// An empty or whitespace-only query finds nothing rather than everything.
///
/// `limit` is optional (default 50, max 500). **`truncated` says there were
/// more matches than it allowed**, and a surface must show it — silently
/// returning the first `n` reads as "that is all there is", which for a search
/// over your own history is a wrong answer rather than a partial one.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_search(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_search(&read_json(json)?))()))
}

/// Sync a folder with a peer, both ways:
/// `{peer, path, into}` → `{fetching, pushing, deleted, renamed, conflicts:[…]}`.
///
/// **Bidirectional, with conflicts kept rather than resolved.** Per-file version
/// vectors distinguish "their copy is newer" from "we both changed it"; when
/// both did, their copy arrives as `name.sync-conflict-<peer>.ext` and the local
/// file is untouched. Only the changed parts of a file cross the wire, and a
/// file that merely moved is renamed locally rather than fetched again.
///
/// Returns once the work has been planned and requested; bytes arrive as
/// ordinary inbound transfers through the usual approval path, because that is
/// what they are.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_sync_pull(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.sync_pull(&read_json(json)?))()))
}

/// Keep a folder in sync until stopped:
/// `{peer, path, into, interval?}` → `{watching:true}`.
///
/// A file is only acted on once it has stopped changing, so saving a large file
/// mid-poll never syncs a half-written copy. The interval is in seconds and
/// clamped to a floor: a one-second poll would re-hash the folder continuously
/// and cost more than the change it is trying to notice.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_sync_watch(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.sync_watch(&read_json(json)?))()))
}

/// Stop watching a folder: `{path, into}` → `{watching:false, was_watching}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_sync_unwatch(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.sync_unwatch(&read_json(json)?))()))
}

/// Which folders are being watched: `{}` → `{watching:[{path,into}]}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_sync_watches(_json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.sync_watches())()))
}

/// Ask a device what is in one of its shared folders:
/// `{peer, path?}` → `{path, entries:[…], truncated, denied}`.
///
/// `path` is **share-relative** — `photos/2026`, never absolute. Empty asks
/// what the device shares at all.
///
/// `denied: true` with no entries means the device is not showing this. It may
/// not have granted this machine `browse`, may share nothing, or the path may
/// not exist — **deliberately indistinguishable**, because a caller able to
/// tell them apart could map a filesystem it may not see, one request at a
/// time.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_browse_list(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.browse_list(&read_json(json)?))()))
}

/// What this device shares for browsing: `{}` → `{shares:[…]}`.
///
/// Names only, the same view a peer gets. **Empty by default** — sharing a
/// folder is a deliberate act, not a consequence of trusting someone.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_browse_shares(json: *const c_char) -> *mut c_char {
    let _ = json;
    guard(|| error::envelope(browse::list_shares()))
}

/// One chronological view of this device's activity: `{limit?}` →
/// `{events:[…], truncated, limit}`, newest first.
///
/// A **read across stores that already exist** — transfers, conversations, and
/// clipboard history when it is on. Nothing is recorded to build it: a timeline
/// with its own log would be a second copy of the same facts, free to disagree
/// with them, and a record of activity nobody separately agreed to.
///
/// Entries carry no message bodies and no clip text. The timeline says *that*
/// something happened and when; reading it is what the conversation and
/// clipboard screens are for.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_timeline(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.timeline(&read_json(json)?))()))
}

/// Every remembered clip, newest first: `{}` → `{entries:[…]}`.
///
/// Empty unless the user turned clipboard history on. Bounded, and never put on
/// the wire — only the current clip syncs; what a device remembers is its own.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_clipboard_history(json: *const c_char) -> *mut c_char {
    let _ = json;
    guard(|| error::envelope(clipboard::history_list()))
}

/// Forget every remembered clip: `{}` → `{cleared: n}`.
///
/// Turning the setting off stops new entries; it does not erase what was
/// already recorded. This does.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_clipboard_history_clear(json: *const c_char) -> *mut c_char {
    let _ = json;
    guard(|| error::envelope(clipboard::history_clear()))
}

/// Ask a device to make itself findable: `{peer, seconds?}` → `{sent}`.
///
/// *Find my device.* `sent` reports only that the request went out; whether the
/// device makes a sound is its own decision, gated there on the presence
/// permission, and this side never learns it. Deliberately independent of the
/// presence sharing opt-in: that governs what this device reveals, and ringing
/// reveals nothing here.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_presence_ring(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.presence_ring(&read_json(json)?))()))
}

/// Sync notes with a peer: `{peer}` → `{sent}`.
///
/// Sends this device's whole set (tombstones included) and merges what the peer
/// answers with. `sent: false` is normal, not a failure: the peer may not have
/// been granted the `notes` permission, may be unreachable, or may predate
/// notes.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_notes_sync(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.notes_sync(&read_json(json)?))()))
}

/// Record where to send a wake packet: `{device, mac}` → `{mac}`.
///
/// A note this machine keeps. It grants nothing and the device is never told.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_wake_set(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.wake_set(&read_json(json)?))()))
}

/// The hardware address recorded for a device: `{device}` → `{mac}` (null when
/// none is recorded).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_wake_get(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.wake_get(&read_json(json)?))()))
}

/// Forget a device's hardware address: `{device}` → `{forgotten}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_wake_forget(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.wake_forget(&read_json(json)?))()))
}

/// Send the wake packet: `{device}` → `{mac, sent_to}`.
///
/// **Reports what was sent, never that the device woke** — Wake-on-LAN has no
/// reply, so a surface claiming otherwise would be inventing a fact. Local
/// network only: a broadcast does not travel over Tailscale or a VPN. Only an
/// approved device may be woken (I6).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_wake_send(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.wake_send(&read_json(json)?))()))
}

/// This conversation's disappearing-message window: `{peer_id}` → `{seconds|null}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_retention_get(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.chat_retention_get(&read_json(json)?)
        })())
    })
}

/// Set or clear the window: `{peer_id, seconds?}` → `{seconds|null}`.
///
/// **Local.** Nothing is sent to the peer and no surface may suggest the peer's
/// copy is affected — that is a promise this architecture cannot keep.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_retention_set(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.chat_retention_set(&read_json(json)?)
        })())
    })
}

/// Delete what has aged out: `{peer_id?}` → `{messages, queued}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_prune(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_prune(&read_json(json)?))()))
}

/// Every Space and its members: `{}` → `{spaces: [...]}`.
///
/// A Space is a label this device keeps over peers it already trusts. Nothing
/// about it is ever sent, and no peer learns it exists — which is what keeps
/// group send peer-to-peer rather than hub-brokered (I3).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_list(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.spaces_list(&read_json(json)?))()))
}

/// Create a Space: `{name}` → `{space}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_create(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.spaces_create(&read_json(json)?))()))
}

/// Rename a Space: `{id, name}` → `{space}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_rename(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.spaces_rename(&read_json(json)?))()))
}

/// Delete a Space: `{id}` → `{deleted}`. Members keep their trust.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_delete(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.spaces_delete(&read_json(json)?))()))
}

/// Add a trusted device to a Space: `{id, device}` → `{added}`.
///
/// Grants nothing. Each fan-out send passes the same per-capability gate a
/// hand-typed send would (I6).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_add_member(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.spaces_add_member(&read_json(json)?)
        })())
    })
}

/// Remove a device from a Space: `{id, device}` → `{removed}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_spaces_remove_member(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.spaces_remove_member(&read_json(json)?)
        })())
    })
}

/// Mark a device as one of the user's own: `{device, mine}` → `{changed}`.
///
/// A local label, not a grant: it widens no permission and the device is never
/// told.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_trust_set_mine(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.trust_set_mine(&read_json(json)?))()))
}

/// The devices the user marked as their own: `{}` → `{devices: [...]}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_trust_my_devices(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.trust_my_devices(&read_json(json)?))()))
}

/// Every live note, newest edit first: `{}` → `{notes: [...]}`.
///
/// Deleted notes are not returned: a tombstone exists so a deletion can reach a
/// peer, not to be read back as a note.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_notes_list(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.notes_list(&read_json(json)?))()))
}

/// Create a note: `{title?, body}` → `{id}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_notes_create(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.notes_create(&read_json(json)?))()))
}

/// Replace a note's content: `{id, title?, body}` → `{updated}`.
///
/// `updated: false` means no such note, or one that has been deleted.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_notes_edit(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.notes_edit(&read_json(json)?))()))
}

/// Delete a note, leaving a tombstone: `{id}` → `{deleted}`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_notes_delete(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.notes_delete(&read_json(json)?))()))
}

/// Tell a peer we have read its messages up to `read_through`:
/// `{peer, read_through}` → `{sent}`.
///
/// `sent` is false — with no error — when the user has not opted into read
/// receipts (`share_read_receipts`, default off), when the peer is unreachable,
/// or when its build predates receipts. Silence is the default state of this
/// feature, not a fault.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_mark_read(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_mark_read(&read_json(json)?))()))
}

/// React to one message in a conversation: `{peer, id, emoji, remove?}` →
/// `{applied, delivered}`.
///
/// `applied` says whether **this device's** history changed; `delivered` says
/// whether the peer was told. They are separate answers on purpose: the
/// reaction is our own record either way, and a peer that is offline — or too
/// old to have negotiated the reaction capability — leaves `delivered` false
/// rather than turning the whole call into a failure. A caller that shows a
/// reaction as "sent" should read `delivered`, not the absence of an error.
///
/// Reactions are not queued for later delivery; see `Manager::chat_react`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_react(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_react(&read_json(json)?))()))
}

/// Settle one conversation's rows that no event will ever finish — a file left
/// mid-flight by a crash or a hard restart: `{peer_id}` → `{changed}`. Emits a
/// `chat_status` per settled row.
///
/// Call it when a thread is opened, before rendering its history. Startup
/// reconciliation now reaches every conversation, file-only ones included, so
/// this is not that: it is the entry point for what a *running* process
/// strands — a row left mid-flight by a session that died without the process,
/// which no restart will come along to settle. Rows whose transfer is live
/// right now are deliberately left alone.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_reconcile(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_reconcile(&read_json(json)?))()))
}

/// Every conversation this device holds:
/// `{peers:[{peer_id,last_timestamp,unread_hint}]}`, newest first. Takes no
/// arguments — pass `{}` or null.
///
/// Derived from the conversation namespaces that exist, so a peer discovery
/// cannot see right now still has an openable thread. `unread_hint` is the
/// number of inbound file offers still awaiting a decision in that thread —
/// **not** a count of unread messages, which PeerBeam has no read receipts to
/// compute (see `Manager::chat_conversations`).
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_conversations(json: *const c_char) -> *mut c_char {
    // `read_json_or_empty`, not `read_json`: this call takes no arguments, so a
    // null pointer must be a call with no arguments rather than a JSON error
    // (same as `pb_channels_json` and `pb_logs_get`).
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.chat_conversations(&read_json_or_empty(json))
        })())
    })
}

/// Delete this device's copy of one conversation: `{peer_id}` →
/// `{removed, kept}`.
///
/// **Local only** — nothing goes on the wire, and the peer keeps its own copy.
///
/// Everything in the thread is removed **except the records still backing
/// queued outbound messages**, which are kept along with their queue entries
/// and staged bytes: the drain reads a missing record as "nothing will ever
/// settle this" and would throw the queued file away. `removed` and `kept` are
/// both counted from what actually happened, so a surface can report the
/// outcome honestly rather than guess at it. See `Manager::chat_delete`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_delete(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_delete(&read_json(json)?))()))
}

/// Delete some of one conversation's messages:
/// `{peer_id, message_ids:[…]}` → `{removed, kept:[…]}`.
///
/// **Local only** — nothing goes on the wire, and the peer keeps its own copy.
///
/// The named rows are removed **except those still backing queued outbound
/// messages**, which are kept along with their queue entries and staged bytes:
/// the drain reads a missing record as "nothing will ever settle this" and would
/// throw the queued file away. `kept` names those ids rather than counting them,
/// so a surface can tell the user which of the messages they picked are still on
/// their way out. An id the conversation does not hold is neither removed nor
/// kept. Shares its keep rule with `pb_chat_delete` — one implementation, not
/// two. See `Manager::chat_delete_messages`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_delete_messages(json: *const c_char) -> *mut c_char {
    guard(|| {
        error::envelope((|| {
            runtime::manager()?.chat_delete_messages(&read_json(json)?)
        })())
    })
}

/// Call off a file we are sharing: `{peer_id, message_id}` → `{cancelled}`.
///
/// Stops the staging copy if one is running, stops the transfer if the bytes
/// are moving, dequeues the entry, deletes the staged blob, and settles the row
/// `failed` with reason `cancelled`. Safe in every state, including an
/// already-settled or unknown id — those are a clean `{cancelled:false}`, not
/// an error. Only ever reaches this device's **own outgoing** share in the named
/// peer's thread; an inbound offer is refused with the approval prompt, never
/// here.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_chat_cancel(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| runtime::manager()?.chat_cancel(&read_json(json)?))()))
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

// ── auto-save rules ─────────────────────────────────────────────

/// Replace the ordered auto-save rule list: `{rules:[{device?, extension?,
/// min_bytes?, max_bytes?, directory}]}` → `{count}`.
///
/// A rule decides **where** an accepted file is written, never **whether** it
/// is accepted — this export cannot approve anything, and the approval path
/// never reads a rule (I6). The **first** matching rule wins, so the order of
/// the list is the tie-break; a rule with no criteria matches everything, and
/// an empty list means every file goes to the save directory exactly as
/// before.
///
/// The whole list at once, because reordering is the point: add, remove and
/// reorder are one call and there is no window where a half-applied edit is on
/// disk. Every rule is validated first — absolute destination, no `..`, an
/// existing parent, a satisfiable size range — and **one bad rule refuses the
/// whole write**, naming which. Reads go through [`pb_settings_get`]
/// (`save_rules`, plus the managed `rules_supported`); there is no separate
/// getter to drift from it.
///
/// Returns `unsupported` on Android/iOS, where an app cannot write to an
/// arbitrary absolute path at all.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_rules_set(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| rules::set(&read_json(json)?))()))
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

// ── PeerSession diagnostics (M8, additive) ──────────────────────

/// All active PeerSessions: `{ sessions:[{id,peer,state,version,capabilities}], count }`.
#[no_mangle]
pub extern "C" fn pb_sessions_json() -> *mut c_char {
    guard(|| error::envelope(session::sessions()))
}

/// One session by id: `{id}` → `{ session: {...}|null }`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_session_get(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope((|| session::session_get(&read_json(json)?))()))
}

/// Live channel snapshot: `{id}` for one session, or `{}`/null for all tracked
/// sessions → `{ channels:[...] }` / `{ sessions:[...] }`.
///
/// # Safety
/// `json` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn pb_channels_json(json: *const c_char) -> *mut c_char {
    guard(|| error::envelope(session::channels(&read_json_or_empty(json))))
}

/// Transport summary: the active transport (always PeerSession) + live session
/// counts. (Endpoint name retained for ABI stability.)
#[no_mangle]
pub extern "C" fn pb_migration_json() -> *mut c_char {
    guard(|| error::envelope(session::migration()))
}

/// Recovery state: sessions currently reconnecting/resuming.
#[no_mangle]
pub extern "C" fn pb_recovery_json() -> *mut c_char {
    guard(|| error::envelope(session::recovery()))
}

/// Aggregate PeerSession diagnostics (`sessions` + `transport` + `recovery`).
#[no_mangle]
pub extern "C" fn pb_diagnostics_json() -> *mut c_char {
    guard(|| error::envelope(session::diagnostics()))
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

    /// ABI compatibility: M8 stays ABI v1 and only *adds* a `features` array.
    #[test]
    fn abi_unchanged_features_additive() {
        assert_eq!(pb_abi_version(), 1);
        let v = take(pb_version_json());
        assert_eq!(v["abi"], 1);
        let features = v["features"].as_array().expect("features array");
        assert!(features.iter().any(|f| f == "peersession_diagnostics"));
    }

    #[test]
    #[serial_test::serial]
    fn diagnostics_before_init_error_cleanly() {
        pb_shutdown();
        let v = take(pb_sessions_json());
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "not_initialised");
        let v = take(pb_migration_json());
        assert_eq!(v["ok"], false);
    }

    #[test]
    #[serial_test::serial]
    fn diagnostics_snapshots_are_well_formed_after_init() {
        let v = take(unsafe { pb_init(std::ptr::null()) });
        assert_eq!(v["ok"], true, "init: {v}");

        let s = take(pb_sessions_json());
        assert_eq!(s["ok"], true);
        assert_eq!(s["data"]["count"], 0);
        assert!(s["data"]["sessions"].is_array());

        let m = take(pb_migration_json());
        assert_eq!(m["ok"], true);
        assert_eq!(m["data"]["transport"], "peersession");
        assert_eq!(m["data"]["active_sessions"], 0);
        assert_eq!(m["data"]["recovering"], 0);

        let r = take(pb_recovery_json());
        assert_eq!(r["ok"], true);
        assert_eq!(r["data"]["recovering"], 0);

        let d = take(pb_diagnostics_json());
        assert_eq!(d["ok"], true);
        assert!(d["data"]["sessions"].is_object());
        assert!(d["data"]["transport"].is_object());

        // Channel snapshot for all (no sessions) is a well-formed empty list.
        let c = take(unsafe { pb_channels_json(std::ptr::null()) });
        assert_eq!(c["ok"], true);
        assert!(c["data"]["sessions"].is_array());

        // Unknown session id → null session, still ok.
        let g = take(unsafe {
            let arg = CString::new("{\"id\":\"deadbeefdeadbeefdeadbeefdeadbeef\"}").unwrap();
            pb_session_get(arg.as_ptr())
        });
        assert_eq!(g["ok"], true);
        assert_eq!(g["data"]["session"], serde_json::Value::Null);

        pb_shutdown();
    }

    /// The additive event vocabulary is stable (documents every new `type`).
    #[test]
    fn session_event_type_names_are_stable() {
        use events::kind;
        assert_eq!(kind::SESSION_CREATED, "session_created");
        assert_eq!(kind::SESSION_CLOSED, "session_closed");
        assert_eq!(kind::SESSION_RECOVERING, "session_recovering");
        assert_eq!(kind::SESSION_RESUMED, "session_resumed");
        assert_eq!(kind::RECOVERY_FAILED, "recovery_failed");
        assert_eq!(kind::CHANNEL_OPENED, "channel_opened");
        assert_eq!(kind::CHANNEL_CLOSED, "channel_closed");
        assert_eq!(kind::CAPABILITY_NEGOTIATED, "capability_negotiated");
    }

    // Collects events for the callback-ordering test.
    static COLLECTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    extern "C" fn collect(ptr: *const c_char) {
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        unsafe { pb_free_string(ptr as *mut c_char) };
        COLLECTED.lock().unwrap().push(s);
    }

    #[test]
    #[serial_test::serial]
    fn session_events_route_in_order_through_the_callback() {
        COLLECTED.lock().unwrap().clear();
        pb_set_event_callback(Some(collect));

        // Emit the additive session-lifecycle vocabulary in a fixed order.
        events::session("abc", events::kind::SESSION_CREATED, json!({ "peer": "p" }));
        events::session(
            "abc",
            events::kind::CAPABILITY_NEGOTIATED,
            json!({ "version": "1.0" }),
        );
        events::session("abc", events::kind::CHANNEL_OPENED, json!({ "channel": 1 }));
        events::session("abc", events::kind::SESSION_CLOSED, json!({}));

        pb_set_event_callback(None);

        // Filter to this test's own session ("abc"): the global callback is a
        // process-wide sink, and unrelated async lifecycle events (e.g. a prior
        // test's daemon serve task emitting `daemon_stopped`) can interleave. We
        // assert routing + ordering for *our* session, not the absence of others.
        let got: Vec<Value> = COLLECTED
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .filter(|v: &Value| v["session_id"] == "abc")
            .collect();
        let types: Vec<&str> = got.iter().map(|v| v["type"].as_str().unwrap()).collect();
        // Order matches emit order exactly (emit invokes the callback synchronously).
        assert_eq!(
            types,
            vec![
                "session_created",
                "capability_negotiated",
                "channel_opened",
                "session_closed",
            ]
        );
        // Envelope shape: session events carry session_id.
        assert_eq!(got[0]["session_id"], "abc");
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
