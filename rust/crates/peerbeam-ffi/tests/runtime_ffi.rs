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
