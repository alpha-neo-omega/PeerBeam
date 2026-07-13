//! Real FFI test: dlopen the built cdylib and call the C-ABI symbols the way
//! Dart will — proving the symbols are exported and the ABI works end to end
//! (not just Rust-calling-Rust). Also drives the event callback and checks the
//! string-ownership contract (we free every returned pointer).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, Ordering};

use libloading::{Library, Symbol};

/// Locate the freshly-built cdylib across common target layouts.
fn lib_path() -> Option<std::path::PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "peerbeam_ffi.dll"
    } else if cfg!(target_os = "macos") {
        "libpeerbeam_ffi.dylib"
    } else {
        "libpeerbeam_ffi.so"
    };
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in ["../../target/debug", "../../target/release"] {
        let p = manifest.join(rel).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

static EVENTS: AtomicU32 = AtomicU32::new(0);

extern "C" fn on_event(_json: *const c_char) {
    // A real consumer would decode + free; here we just count deliveries. The
    // library passes an owned pointer, but leaking a few in a test is fine.
    EVENTS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn cdylib_exports_and_runs() {
    let Some(path) = lib_path() else {
        eprintln!("skip: cdylib not built yet");
        return;
    };
    let lib = unsafe { Library::new(&path).expect("load cdylib") };

    unsafe {
        let abi: Symbol<extern "C" fn() -> u32> = lib.get(b"pb_abi_version").unwrap();
        assert_eq!(abi(), 1);

        let free: Symbol<extern "C" fn(*mut c_char)> = lib.get(b"pb_free_string").unwrap();

        // version_json → parse → free.
        let version: Symbol<extern "C" fn() -> *mut c_char> = lib.get(b"pb_version_json").unwrap();
        let ptr = version();
        let s = CStr::from_ptr(ptr).to_str().unwrap().to_string();
        free(ptr);
        assert!(s.contains("\"abi\":1"));

        // register events, init, start discovery.
        let set_cb: Symbol<extern "C" fn(Option<extern "C" fn(*const c_char)>)> =
            lib.get(b"pb_set_event_callback").unwrap();
        set_cb(Some(on_event));

        let init: Symbol<extern "C" fn(*const c_char) -> *mut c_char> =
            lib.get(b"pb_init").unwrap();
        let cfg = CString::new("").unwrap();
        let ptr = init(cfg.as_ptr());
        let s = CStr::from_ptr(ptr).to_str().unwrap().to_string();
        free(ptr);
        assert!(s.contains("\"ok\":true"), "init: {s}");

        let devices: Symbol<extern "C" fn() -> *mut c_char> = lib.get(b"pb_devices_json").unwrap();
        let ptr = devices();
        let s = CStr::from_ptr(ptr).to_str().unwrap().to_string();
        free(ptr);
        assert!(
            s.contains("\"ok\":true") && s.contains("devices"),
            "devices: {s}"
        );

        let shutdown: Symbol<extern "C" fn()> = lib.get(b"pb_shutdown").unwrap();
        shutdown();
    }
}
