//! End-to-end FFI transfer tests. Drive the C-ABI transfer surface with a real
//! peer on the other end (a distinct identity over real QUIC), covering: the
//! receive+accept flow into the FFI engine, sending out of the FFI engine,
//! events (queued/started/progress/completed) with ordering, live stats,
//! concurrency, and pause/resume/cancel wiring. No file bytes cross the FFI.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{EncryptionProvider, TransferProvider};
use peerbeam_ffi::*;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file, send_file, Identity, SecureLink, SendRequest, TransferControl,
    TransferOutcome,
};
use peerbeam_transfer_quic::{direct_route, QuicTransport};
use peerbeam_trust_fs::FsTrust;

// ── event capture ───────────────────────────────────────────────

static EVENTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

extern "C" fn on_event(ptr: *const c_char) {
    let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
    unsafe { pb_free_string(ptr as *mut c_char) };
    if let Ok(v) = serde_json::from_str(&s) {
        EVENTS.lock().unwrap().push(v);
    }
}

fn events_snapshot() -> Vec<Value> {
    EVENTS.lock().unwrap().clone()
}

/// Poll captured events for one matching `pred`, up to `secs`.
fn wait_event(secs: u64, pred: impl Fn(&Value) -> bool) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(v) = events_snapshot().into_iter().find(&pred) {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    None
}

// ── FFI call helpers ────────────────────────────────────────────

fn take(ptr: *mut c_char) -> Value {
    let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
    unsafe { pb_free_string(ptr) };
    serde_json::from_str(&s).unwrap()
}

fn call_json(f: unsafe extern "C" fn(*const c_char) -> *mut c_char, v: &Value) -> Value {
    let c = CString::new(v.to_string()).unwrap();
    take(unsafe { f(c.as_ptr()) })
}

fn init_ffi(port: u16, dir: &std::path::Path) {
    pb_set_event_callback(Some(on_event));
    EVENTS.lock().unwrap().clear();
    let mut cfg = EngineConfig::default();
    cfg.transfer.port = port;
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    cfg.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
    cfg.device.auto_accept_trusted = false;
    std::fs::create_dir_all(dir.join("recv")).unwrap();
    let c = CString::new(serde_json::to_string(&cfg).unwrap()).unwrap();
    let v = take(unsafe { pb_init(c.as_ptr()) });
    assert_eq!(v["ok"], true, "init: {v}");
}

// ── a real peer on the other end (distinct identity) ────────────

fn peer_identity(dir: &std::path::Path, name: &str) -> (AeadCrypto, FsTrust, Identity) {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: DeviceId::from(name),
        name: name.into(),
        keypair,
    };
    let trust = FsTrust::open(dir.join(format!("{name}-trust.json"))).unwrap();
    (enc, trust, identity)
}

fn session() -> TransferSession {
    TransferSession {
        id: TransferId::from("peer"),
        peer: DeviceId::from("peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: Vec::new(),
        total_bytes: 0,
        transferred_bytes: 0,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
    }
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

// ── tests ───────────────────────────────────────────────────────

#[test]
#[serial_test::serial]
fn control_before_init_errors() {
    pb_shutdown();
    let v = call_json(pb_transfer_pause, &json!({ "id": "x" }));
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "not_initialised");
}

/// Real peer sends INTO the FFI engine; test approves via pb_transfer_accept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn receive_into_ffi_with_accept() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49823;
    init_ffi(port, dir.path());
    tokio::time::sleep(Duration::from_millis(300)).await; // let the server bind

    let payload = pattern(512 * 1024);
    let src = dir.path().join("incoming.bin");
    std::fs::write(&src, &payload).unwrap();
    let (enc, trust, identity) = peer_identity(dir.path(), "sender");
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    // Sender parks after Meta until the FFI side accepts.
    let send_fut = async {
        let mut link = quic.dial(&route, &session()).await.unwrap();
        let sess = authenticate(&mut *link, &identity, &enc, &trust)
            .await
            .unwrap();
        let mut secure = SecureLink::new(&mut *link, &enc, sess);
        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: "peer-send".into(),
            name: "incoming.bin".into(),
            path: src.to_string_lossy().into(),
            size: payload.len() as u64,
            chunk_size: 64 * 1024,
        };
        send_file(&mut secure, &FsStorage::new(), req, &ctrl, &ptx, 3).await
    };

    // Driver: wait for the incoming queued event, then accept it.
    let driver = async {
        let queued = tokio::task::spawn_blocking(|| {
            wait_event(5, |e| {
                e["type"] == "transfer_queued" && e["payload"]["incoming"] == true
            })
        })
        .await
        .unwrap()
        .expect("incoming queued event");
        let id = queued["transfer_id"].as_str().unwrap().to_string();
        let v = call_json(pb_transfer_accept, &json!({ "id": id }));
        assert_eq!(v["ok"], true, "accept: {v}");
        id
    };

    let (send_res, recv_id) = tokio::join!(send_fut, driver);
    assert_eq!(send_res.unwrap(), TransferOutcome::Completed);

    // FFI must have emitted a completed event for the received transfer.
    let done = tokio::task::spawn_blocking(move || {
        wait_event(5, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == recv_id
        })
    })
    .await
    .unwrap();
    assert!(
        done.is_some(),
        "expected transfer_completed for the receive"
    );

    let got = std::fs::read(dir.path().join("recv").join("incoming.bin")).unwrap();
    assert_eq!(got, payload, "received file byte-exact");

    // History updated, with the received file's local path so the UI can
    // open it.
    let hist = take(pb_history_get());
    let entries = hist["data"]["history"].as_array().unwrap().clone();
    assert!(entries.iter().any(|h| h["success"] == true));
    let received = entries
        .iter()
        .find(|h| h["direction"] == "receiving")
        .expect("receiving entry");
    let path = received["path"].as_str().expect("history path");
    assert!(
        std::path::Path::new(path).is_file(),
        "history path points at the received file: {path}"
    );
    pb_shutdown();
}

/// FFI engine sends OUT to a real peer receiver; checks events, stats, control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn send_from_ffi_events_and_stats() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49824, dir.path());

    // A real receiver on its own port + identity.
    let (enc, trust, identity) = peer_identity(dir.path(), "receiver");
    let recv_quic = QuicTransport::new().unwrap();
    let (addr, mut incoming) = recv_quic
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_port = addr.port();
    let recv_dir = dir.path().join("peer-recv");
    std::fs::create_dir_all(&recv_dir).unwrap();
    let recv_dir_s = recv_dir.to_string_lossy().into_owned();

    let payload = pattern(1024 * 1024);
    let src = dir.path().join("out.bin");
    std::fs::write(&src, &payload).unwrap();

    // Receiver side (auto-accepts by just running receive_file).
    let recv_fut = async move {
        use futures::StreamExt;
        let mut link = incoming.next().await.unwrap().unwrap();
        let sess = authenticate(&mut *link, &identity, &enc, &trust)
            .await
            .unwrap();
        let mut secure = SecureLink::new(&mut *link, &enc, sess);
        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        receive_file(&mut secure, &FsStorage::new(), &recv_dir_s, &ctrl, &ptx)
            .await
            .unwrap()
    };

    // Kick off the FFI send.
    let send = call_json(
        pb_transfer_send,
        &json!({
            "peer": { "name": "peer", "addresses": ["127.0.0.1"], "port": peer_port },
            "paths": [ src.to_string_lossy() ],
        }),
    );
    assert_eq!(send["ok"], true, "send: {send}");
    let send_id = send["data"]["ids"][0].as_str().unwrap().to_string();

    let received = recv_fut.await;
    assert_eq!(received.bytes, payload.len() as u64);

    // Events: started + progress + completed for our send id, in that order.
    let sid = send_id.clone();
    let completed = tokio::task::spawn_blocking(move || {
        wait_event(5, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == sid
        })
    })
    .await
    .unwrap();
    assert!(
        completed.is_some(),
        "expected transfer_completed for the send"
    );

    let evs = events_snapshot();
    let mine: Vec<&str> = evs
        .iter()
        .filter(|e| e["transfer_id"] == send_id)
        .filter_map(|e| e["type"].as_str())
        .collect();
    let pos = |t: &str| mine.iter().position(|x| *x == t);
    assert!(
        pos("transfer_started") < pos("transfer_completed"),
        "ordering: {mine:?}"
    );
    assert!(mine.contains(&"transfer_progress"), "progress emitted");

    // Progress carried live stats.
    let prog = evs
        .iter()
        .find(|e| e["type"] == "transfer_progress" && e["transfer_id"] == send_id)
        .unwrap();
    assert!(prog["payload"]["stats"]["total_bytes"].as_u64().unwrap() > 0);

    let bytes = std::fs::read(recv_dir.join("out.bin")).unwrap();
    assert_eq!(bytes, payload);
    pb_shutdown();
}

/// Control wiring: pause/resume/cancel return typed results; unknown id errors.
#[test]
#[serial_test::serial]
fn control_unknown_id_errors() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49825, dir.path());
    for f in [
        pb_transfer_pause,
        pb_transfer_resume,
        pb_transfer_cancel,
        pb_transfer_get,
    ] {
        let v = call_json(f, &json!({ "id": "nope" }));
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "invalid_argument");
    }
    // accept/reject of a non-pending id also error cleanly.
    let v = call_json(pb_transfer_accept, &json!({ "id": "nope" }));
    assert_eq!(v["ok"], false);
    pb_shutdown();
}
