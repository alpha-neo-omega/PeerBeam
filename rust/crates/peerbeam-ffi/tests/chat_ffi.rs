//! End-to-end FFI chat tests. `Manager`/its `chat` `ChatStore` are process-
//! global singletons behind `pb_init`/`pb_shutdown` (see `runtime.rs`), so —
//! exactly like `transfer_ffi.rs`'s "real peer on the other end" tests —
//! there is one FFI engine per test and a manually-built real peer (its own
//! QUIC transport + identity + `ChatStore`) standing in for "the other
//! device". Each test gives the FFI engine one role (sender or receiver);
//! together they cover `pb_chat_send` and `pb_chat_history` from both sides,
//! mirroring how `transfer_ffi.rs` covers its own send/receive split.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};

use peerbeam_chat::{ChatHandler, ChatRecord, ChatStore, ReceivedSink};
use peerbeam_config::EngineConfig;
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, MessageHandler};
use peerbeam_ffi::*;
use peerbeam_transfer::{HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole};
use peerbeam_transfer_quic::{direct_route, QuicTransport};
use peerbeam_trust_fs::FsTrust;

/// The session config a manual chat-only peer advertises: just CHAT (plus the
/// always-implicit CONTROL) — matches `peerbeam-chat/tests/roundtrip.rs`.
/// Negotiation is an intersection (`CapabilitySet::intersect`), never an
/// error on mismatch, so this interoperates fine with the FFI engine's
/// `session_exec::session_cfg`, which additionally advertises TRANSFER.
fn chat_only_caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::new(ChannelType::CHAT))
}

// ── event capture (mirrors transfer_ffi.rs) ─────────────────────

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

/// Poll captured events for one matching `pred`, up to `secs` (bounded, no
/// fixed sleep).
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

/// Poll a `Mutex<Vec<ChatRecord>>` (a manual peer's captured records — it has
/// no FFI event callback) for a first entry, up to `secs`.
fn wait_record(secs: u64, records: &Mutex<Vec<ChatRecord>>) -> Option<ChatRecord> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(r) = records.lock().unwrap().first().cloned() {
            return Some(r);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    None
}

// ── FFI call helpers (mirrors transfer_ffi.rs) ──────────────────

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

// ── a real manual peer on the other end (distinct identity) ─────

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

/// A fresh, on-disk `ChatStore` for a manual peer (own AppStore namespace).
fn peer_chat_store(dir: &std::path::Path, name: &str, secret_seed: u8) -> ChatStore {
    let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
    let key = derive_subkey(&[secret_seed; 32], b"peerbeam-appstore-v1");
    let app = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
        dir.join(format!("{name}-appstore")),
        key,
        enc,
    ));
    ChatStore::new(app)
}

fn session_meta() -> TransferSession {
    TransferSession {
        id: TransferId::from("chat"),
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

// ── tests ─────────────────────────────────────────────────────

/// FFI engine SENDS via `pb_chat_send` to a manually-built real peer.
/// Covers `Manager::chat_send`/`pb_chat_send` end to end (real QUIC dial,
/// Chat channel open, one Message frame) and `Manager::chat_history`/
/// `pb_chat_history` reading back the FFI's own persisted Sent record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_send_from_ffi_reaches_peer_and_persists_sent_record() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49900, dir.path());

    // Manual receiving peer: its own QUIC transport, identity, and ChatStore,
    // with a ChatHandler wired to capture received records (it has no FFI
    // event callback to observe, so a plain Vec capture stands in for that).
    let (enc, trust, identity) = peer_identity(dir.path(), "receiver");
    let recv_quic = QuicTransport::new().unwrap();
    let (addr, mut incoming) = recv_quic
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_port = addr.port();
    let peer_store = peer_chat_store(dir.path(), "receiver", 42);

    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler, peer_slot) = ChatHandler::new(peer_store.clone(), sink);

    let recv_fut = async move {
        use futures::StreamExt;
        let qc = incoming.next().await.unwrap().unwrap();
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_only_caps())
            .with_handlers(HandlerRegistry::new().with(handler as Arc<dyn MessageHandler>));
        let mut ps = PeerSession::open(
            transport,
            SessionRole::Responder,
            cfg,
            ev,
            ch,
            inc,
            None,
            identity,
            enc,
            trust,
        )
        .await
        .unwrap();
        // Bind the handler's peer slot to the FFI engine's device id before
        // the run loop can dispatch any Chat frame.
        let _ = peer_slot.set(ps.peer().clone());
        ps.run().await
    };
    tokio::spawn(recv_fut);

    // FFI sends: `chat_send({peer, text}) -> {id}`.
    let sender_device = json!({
        "id": "receiver",
        "name": "receiver",
        "addresses": ["127.0.0.1"],
        "port": peer_port,
    });
    let sent = call_json(
        pb_chat_send,
        &json!({ "peer": sender_device, "text": "hi" }),
    );
    assert_eq!(sent["ok"], true, "chat_send: {sent}");
    let msg_id = sent["data"]["id"].as_str().expect("id string").to_string();
    assert!(!msg_id.is_empty(), "chat_send returns a non-empty id");

    // The manual peer actually received the message over the wire.
    let got = wait_record(5, &received).expect("peer did not receive the chat message in time");
    assert_eq!(got.body, "hi");
    assert_eq!(got.id, msg_id, "same message id round-trips");

    // The FFI engine's own chat_history shows the record it just sent.
    let hist = call_json(pb_chat_history, &json!({ "peer_id": "receiver" }));
    assert_eq!(hist["ok"], true, "chat_history: {hist}");
    let messages = hist["data"]["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], msg_id);
    assert_eq!(messages[0]["body"], "hi");
    assert_eq!(messages[0]["peer_id"], "receiver");
    assert_eq!(messages[0]["direction"], "out");
    assert_eq!(messages[0]["status"], "sent");

    pb_shutdown();
}

/// A manually-built real peer SENDS chat into the FFI engine (which is
/// RECEIVING — `handle_incoming`'s already-wired `ChatHandler`, Task 5).
/// Covers the FFI engine actually emitting a `chat_received` event and
/// `Manager::chat_history`/`pb_chat_history` reading back the received
/// record from the receiving side.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_received_into_ffi_and_history_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49901;
    init_ffi(port, dir.path());
    tokio::time::sleep(Duration::from_millis(300)).await; // let the server bind

    let (enc, trust, identity) = peer_identity(dir.path(), "sender");
    let sender_store = peer_chat_store(dir.path(), "sender", 7);
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    let send_fut = async move {
        let qc = quic.dial_channels(&route, &session_meta()).await.unwrap();
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_only_caps());
        let mut ps = PeerSession::open(
            transport,
            SessionRole::Initiator,
            cfg,
            ev,
            ch,
            inc,
            None,
            identity,
            enc,
            trust,
        )
        .await
        .unwrap();
        let handle = ps.handle();
        tokio::spawn(async move {
            let _ = ps.run().await;
        });
        peerbeam_chat::send_message(&handle, &sender_store, &DeviceId::from("ffi-peer"), "hi")
            .await
            .expect("send_message succeeds")
    };

    let sent_rec = send_fut.await;
    assert_eq!(sent_rec.body, "hi");

    // FFI must have emitted `chat_received` for this message.
    let event = tokio::task::spawn_blocking(|| {
        wait_event(5, |e| {
            e["type"] == "chat_received" && e["message"]["body"] == "hi"
        })
    })
    .await
    .unwrap()
    .expect("expected a chat_received event");
    assert_eq!(event["message"]["direction"], "in");
    assert_eq!(event["message"]["status"], "received");
    let sender_peer_id = event["message"]["peer_id"]
        .as_str()
        .expect("peer_id string")
        .to_string();

    // `pb_chat_history` on the FFI (the receiving side) returns it.
    let hist = call_json(pb_chat_history, &json!({ "peer_id": sender_peer_id }));
    assert_eq!(hist["ok"], true, "chat_history: {hist}");
    let messages = hist["data"]["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["body"], "hi");
    assert_eq!(messages[0]["direction"], "in");
    assert_eq!(messages[0]["status"], "received");

    pb_shutdown();
}
