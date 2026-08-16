//! M3 runtime-management FFI: clipboard, settings, daemon, status, logs. Uses
//! the C-ABI functions directly (serialized — shared global engine state).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use serde_json::{json, Value};

use peerbeam_config::EngineConfig;
use peerbeam_ffi::*;

fn take(ptr: *mut c_char) -> Value {
    let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
    unsafe { pb_free_string(ptr) };
    serde_json::from_str(&s).unwrap()
}

fn call(f: unsafe extern "C" fn(*const c_char) -> *mut c_char, v: &Value) -> Value {
    let c = CString::new(v.to_string()).unwrap();
    take(unsafe { f(c.as_ptr()) })
}

fn init(dir: &std::path::Path) {
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    cfg.transfer.port = 49840;
    let c = CString::new(serde_json::to_string(&cfg).unwrap()).unwrap();
    let v = take(unsafe { pb_init(c.as_ptr()) });
    assert_eq!(v["ok"], true, "init: {v}");
}

#[test]
#[serial_test::serial]
fn clipboard_set_get_and_classify() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    // URL is auto-classified.
    let r = call(
        pb_clipboard_set,
        &json!({ "text": "https://example.com/x" }),
    );
    assert_eq!(r["ok"], true);
    let g = take(pb_clipboard_get());
    assert_eq!(g["data"]["item"]["kind"], "url");
    assert_eq!(g["data"]["item"]["text"], "https://example.com/x");

    // Image stores metadata only (no bytes).
    call(
        pb_clipboard_set,
        &json!({ "kind": "image", "mime": "image/png", "size": 2048 }),
    );
    let g = take(pb_clipboard_get());
    assert_eq!(g["data"]["item"]["kind"], "image");
    assert_eq!(g["data"]["item"]["size"], 2048);

    // Bad input → typed error.
    let bad = call(pb_clipboard_set, &json!({ "nope": 1 }));
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "invalid_argument");
    pb_shutdown();
}

#[test]
#[serial_test::serial]
fn settings_get_set_reset_persist() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    let g = take(pb_settings_get());
    assert_eq!(g["data"]["version"], 1);
    assert!(g["data"]["transfer_directory"].is_string());
    assert!(g["data"]["trusted_devices"].is_array());

    // Set persists.
    call(
        pb_settings_set,
        &json!({ "theme": "dark", "auto_accept": true }),
    );
    let g = take(pb_settings_get());
    assert_eq!(g["data"]["theme"], "dark");
    assert_eq!(g["data"]["auto_accept"], true);
    assert!(dir.path().join("data/ffi_settings.json").exists());

    // Reset restores defaults.
    take(pb_settings_reset());
    let g = take(pb_settings_get());
    assert_eq!(g["data"]["theme"], "system");
    pb_shutdown();
}

#[test]
#[serial_test::serial]
fn daemon_lifecycle_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path()); // init starts the daemon

    let s = take(pb_daemon_status());
    assert_eq!(s["data"]["running"], true);

    // start again → idempotent.
    let r = take(pb_daemon_start());
    assert_eq!(r["ok"], true);

    let r = take(pb_daemon_stop());
    assert_eq!(r["ok"], true);
    assert_eq!(take(pb_daemon_status())["data"]["running"], false);

    let r = take(pb_daemon_restart());
    assert_eq!(r["ok"], true);
    assert_eq!(take(pb_daemon_status())["data"]["running"], true);
    pb_shutdown();
}

#[test]
#[serial_test::serial]
fn status_reports_runtime_shape() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let s = take(pb_status());
    assert_eq!(s["ok"], true);
    let d = &s["data"];
    assert_eq!(d["runtime"], "running");
    assert_eq!(d["build"]["abi"], 1);
    assert!(d["build"]["version"].is_string());
    assert!(d["active_transfers"].is_number());
    assert_eq!(d["daemon"]["running"], true);
    pb_shutdown();
}

/// Smoke test for the chat-drain shutdown fix: a full two-cycle
/// init/shutdown/init/shutdown must complete promptly (an un-aborted drain
/// task would at worst hang `shutdown()`) and each `init()` must succeed
/// (`init()`'s own `assert_eq!(v["ok"], true, ...)` above fails loudly
/// otherwise). Note this doesn't by itself distinguish the fix from the
/// regression — `stop_daemon()` already explicitly kills the previous
/// daemon/port-bind regardless of the drain task, so port reuse alone
/// succeeds either way; see `runtime::tests::shutdown_drops_the_chat_drain_and_lets_the_engine_deallocate`
/// for the precise white-box regression guard (Engine actually deallocates
/// once the drain's `Arc` clone is aborted).
#[test]
#[serial_test::serial]
fn shutdown_releases_chat_drain_so_repeated_init_shutdown_cycles_stay_clean() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    pb_shutdown();

    let dir2 = tempfile::tempdir().unwrap();
    init(dir2.path());
    pb_shutdown();
}

/// The blob root `init` sweeps, and the outbox namespace directory that
/// decides whether it may. Both are laid out by `runtime::init` /
/// `FsAppStore` (`<data>/outbox-blobs/<id>` and
/// `<data>/appstore/<namespace>/<hex(key)>`).
fn blob_root(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("data/outbox-blobs")
}

/// Bytes staged by a run that crashed between staging and enqueue are owned by
/// nothing — no queue entry will ever send them, and no settle will ever
/// delete them, so without this they sit on disk forever.
///
/// This is the *genuinely empty queue* half of the startup decision, and it is
/// what stops the guard in the next test from being satisfied by a sweep that
/// simply never runs: an empty outbox is a complete answer, and every blob
/// really is an orphan.
#[test]
#[serial_test::serial]
fn init_sweeps_a_staged_blob_that_no_queue_entry_owns() {
    let dir = tempfile::tempdir().unwrap();
    // A first run creates the identity the appstore key is derived from, so
    // the second init below reads back the same store (readable, and empty).
    init(dir.path());
    pb_shutdown();

    let blobs = blob_root(dir.path());
    std::fs::create_dir_all(&blobs).unwrap();
    let orphan = blobs.join("0000000000001");
    std::fs::write(&orphan, b"bytes nothing owns").unwrap();

    init(dir.path());
    assert!(
        !orphan.exists(),
        "a staged blob no queue entry owns must not survive startup"
    );
    pb_shutdown();
}

/// The trap this task exists to close, end to end.
///
/// Every ordinary outbox reader *contains* damage rather than propagating it —
/// an undecodable row is skipped so the rest of the queue still delivers — so
/// a corrupted outbox reads back as "nothing is queued", indistinguishable
/// from an empty one. `sweep` deletes every blob its `keep` set does not name.
/// Wiring those two together naively destroys the only copy of every queued
/// file, at boot, while each conversation row still says the file is waiting.
///
/// `init` consults `ChatStore::outbox_owned_blobs`, which refuses rather than
/// under-report, and a refusal sweeps nothing at all.
#[test]
#[serial_test::serial]
fn init_sweeps_nothing_when_the_outbox_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    pb_shutdown();

    let blobs = blob_root(dir.path());
    std::fs::create_dir_all(&blobs).unwrap();
    let blob = blobs.join("0000000000002");
    std::fs::write(&blob, b"a queued file's only copy").unwrap();

    // Make the outbox unreadable. `FsAppStore` stores each record sealed at
    // `<root>/<namespace>/<hex(key)>`; a file whose name is valid hex — so the
    // store treats it as one of its own records rather than skipping it as
    // debris — but whose bytes it cannot open takes the whole listing down.
    let hex: String = "0000000000002"
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    let outbox = dir.path().join("data/appstore/chat.outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    std::fs::write(outbox.join(hex), b"not a sealed record").unwrap();

    init(dir.path());
    assert!(
        blob.exists(),
        "a corrupted outbox must not cost the user a queued file's only copy"
    );
    assert_eq!(std::fs::read(&blob).unwrap(), b"a queued file's only copy");
    pb_shutdown();
}

#[test]
#[serial_test::serial]
fn logs_get_subscribe_export() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    // Produce a log line the capture layer will record.
    tracing::info!("m3 test log line");
    let g = call(pb_logs_get, &json!({ "limit": 50 }));
    assert_eq!(g["ok"], true);
    assert!(g["data"]["logs"].is_array());

    // Subscribe toggles emission (returns the flag).
    let s = call(pb_logs_subscribe, &json!({ "enabled": true }));
    assert_eq!(s["data"]["subscribed"], true);

    // Export writes a file.
    let out = dir.path().join("logs.jsonl");
    let e = call(pb_logs_export, &json!({ "path": out.to_string_lossy() }));
    assert_eq!(e["ok"], true);
    assert!(out.exists());
    pb_shutdown();
}
