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
use peerbeam_discovery_udp::DEFAULT_DISCOVERY_PORT;
use peerbeam_domain::entity::{Direction, Route, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, MessageHandler};
use peerbeam_ffi::*;
use peerbeam_transfer::{HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole};
use peerbeam_transfer_quic::{direct_route, QuicChannels, QuicTransport};
use peerbeam_trust_fs::FsTrust;
use tokio::net::UdpSocket;

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

/// Poll `pb_chat_history` for `peer_id` until the message `msg_id` shows
/// `status`, up to `secs` (bounded, ~50ms steps — no fixed sleep). Needed
/// because `chat_send` now enqueues (`Pending`) and returns immediately,
/// delivering — and flipping the record to `Sent` — via a spawned background
/// flush (`Manager::chat_flush_peer`), so the status update can lag the
/// `chat_send` call by a beat instead of being visible synchronously.
fn wait_chat_status(secs: u64, peer_id: &str, msg_id: &str, status: &str) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let hist = call_json(pb_chat_history, &json!({ "peer_id": peer_id }));
        if let Some(messages) = hist["data"]["messages"].as_array() {
            if let Some(m) = messages.iter().find(|m| m["id"] == msg_id) {
                if m["status"] == status {
                    return Some(m.clone());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
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

/// Dial the FFI daemon's QUIC listener with a bounded retry/backoff instead
/// of a fixed sleep: `pb_init` returns as soon as `start_daemon()` has
/// *spawned* the task that binds the listener (`Manager::serve` ->
/// `serve_channels_on`), not once it has actually finished binding — a single
/// unretried dial right after `pb_init` can flake if that bind hasn't landed
/// yet. Retries in short steps for up to `budget` (test-only: panics if the
/// listener never comes up in time, mirroring `send_message`'s own
/// fail-after-budget convention).
async fn dial_channels_retrying(
    quic: &QuicTransport,
    route: &Route,
    meta: &TransferSession,
    budget: Duration,
) -> QuicChannels {
    let deadline = Instant::now() + budget;
    loop {
        match quic.dial_channels(route, meta).await {
            Ok(qc) => return qc,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("dial_channels did not succeed within {budget:?}: {e}");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

// ── driving discovery without real LAN/mDNS hardware ───────────

/// A raw discovery `Announce` datagram in the same wire shape as
/// `peerbeam_discovery_udp`'s (private) `Wire` type — mirrored by hand here
/// the same way `peerbeam-discovery-udp/tests/loopback.rs` does, since the
/// type itself isn't exported. Sent straight at the FFI engine's own
/// discovery socket below to make a manually-built chat peer show up as
/// `online` in `engine.devices()`, without depending on real broadcast
/// traffic reaching the test sandbox.
fn announce_json(id: &str, port: u16) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "v": 1,
        "kind": "announce",
        "id": id,
        "name": id,
        "device_type": "Desktop",
        "platform": "linux",
        "port": port,
    }))
    .unwrap()
}

/// Repeatedly announce `id`/`port` to the FFI engine's UDP discovery port
/// (bound by `pb_discovery_start`) every 750ms — comfortably inside
/// `peerbeam_discovery_udp`'s default 6s liveness TTL — so the peer stays
/// visible (and `online`) in `engine.devices()` for as long as the returned
/// task keeps running. Abort it once the test no longer needs the peer to
/// look reachable.
fn spawn_periodic_announce(id: &str, port: u16) -> tokio::task::JoinHandle<()> {
    let id = id.to_string();
    tokio::spawn(async move {
        let sock = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind announce socket");
        loop {
            let _ = sock
                .send_to(
                    &announce_json(&id, port),
                    ("127.0.0.1", DEFAULT_DISCOVERY_PORT),
                )
                .await;
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    })
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

    // The FFI engine's own chat_history shows the record it just sent, once
    // the background flush (spawned by `chat_send`) has delivered it and
    // flipped it from `Pending` to `Sent` — bounded poll, not an immediate
    // assertion, since `chat_send` no longer blocks until delivery.
    let sent_msg = wait_chat_status(5, "receiver", &msg_id, "sent")
        .expect("message did not reach status \"sent\" within 5s");
    assert_eq!(sent_msg["id"], msg_id);
    assert_eq!(sent_msg["body"], "hi");
    assert_eq!(sent_msg["peer_id"], "receiver");
    assert_eq!(sent_msg["direction"], "out");

    let hist = call_json(pb_chat_history, &json!({ "peer_id": "receiver" }));
    assert_eq!(hist["ok"], true, "chat_history: {hist}");
    let messages = hist["data"]["messages"].as_array().expect("messages array");
    assert_eq!(
        messages.len(),
        1,
        "exactly one history record for this peer"
    );

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

    let (enc, trust, identity) = peer_identity(dir.path(), "sender");
    let sender_store = peer_chat_store(dir.path(), "sender", 7);
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    let send_fut = async move {
        // Bounded retry instead of a fixed sleep: the daemon's listener bind
        // races this dial, and a single unretried attempt right after
        // `pb_init` can flake if the bind hasn't landed yet.
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
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

/// A manually-built real peer dials into the FFI engine purely to deliver
/// chat — no transfer stream is ever opened, exactly like `chat_flush_peer`,
/// the drain loop, and flush-on-connect since chat 1b. Regression test for the
/// bug where `handle_incoming` registered an "(incoming)" transfer and
/// blocked on the approval gate before it knew whether any transfer stream
/// was coming at all — so every inbound chat message raised a phantom
/// file-approval prompt on the receiver, and (with auto-accept on) a
/// failed-transfer history row for what was only a chat message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_only_dial_does_not_register_phantom_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49903;
    init_ffi(port, dir.path());

    let (enc, trust, identity) = peer_identity(dir.path(), "sender");
    let sender_store = peer_chat_store(dir.path(), "sender", 9);
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    let send_fut = async move {
        // Bounded retry instead of a fixed sleep: the daemon's listener bind
        // races this dial, and a single unretried attempt right after
        // `pb_init` can flake if the bind hasn't landed yet.
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
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

    // A chat-only dial must not fabricate a transfer. Pre-fix, handle_incoming
    // registered an "(incoming)" transfer and blocked on the approval gate
    // before it knew whether any stream channel was coming — so every inbound
    // chat message raised a phantom file-approval prompt on the receiver.
    let events = events_snapshot();
    let phantom: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("transfer_queued"))
        .collect();
    assert!(
        phantom.is_empty(),
        "a chat-only dial must emit no transfer_queued; got {phantom:?}"
    );

    // ...and no transfer may be left sitting in `active`.
    // `pb_transfers_active` takes no arguments, so it is safe to call directly;
    // its envelope is `{ok, data: {transfers: [...]}}` (Manager::active_list).
    let active = take(pb_transfers_active());
    let list = active
        .get("data")
        .and_then(|d| d.get("transfers"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        list.is_empty(),
        "a chat-only dial must leave no active transfer; got {list:?}"
    );

    pb_shutdown();
}

/// The background chat drain (`runtime::chat_drain_loop`, spawned from
/// `pb_init`) retries delivery to a peer that was unreachable at `chat_send`
/// time, once discovery reports it back online — this is a *distinct*
/// delivery path from the two already covered above/in 1a:
/// - `chat_send`'s own one-shot opportunistic flush already ran (and failed,
///   since nothing was listening yet) by the time the peer comes online.
/// - `handle_incoming`'s flush-on-connect never fires here because the
///   manual peer only ever *listens*; it never dials into the FFI engine.
///
/// So any delivery observed below can only be the new periodic drain tick.
///
/// Real LAN/mDNS/Tailscale broadcast hardware isn't available in a test
/// sandbox, so "discovery reports the peer online" is driven directly: a raw
/// UDP `Announce` datagram is sent straight at the FFI engine's own
/// discovery socket (`pb_discovery_start` binds
/// `peerbeam_discovery_udp::DEFAULT_DISCOVERY_PORT`), re-sent periodically so
/// the peer doesn't age out of the provider's liveness TTL before the
/// drain's 15s tick fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_drain_delivers_queued_message_once_peer_comes_online() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49902, dir.path());

    // `runtime::discovery_start` blocks on the shared runtime directly (no
    // `Handle::try_current` fallback like `shutdown`/`init` have) — calling
    // it from this `#[tokio::test]`'s own async context would hit tokio's
    // "cannot start a runtime from within a runtime" panic. A plain OS thread
    // has no tokio context at all, matching how a real (non-async) Dart
    // caller invokes it.
    let discovery = std::thread::spawn(|| take(pb_discovery_start()))
        .join()
        .unwrap();
    assert_eq!(discovery["ok"], true, "discovery_start: {discovery}");

    let peer_id = "drain-peer";
    let peer_port: u16 = 49964;

    // 1) Enqueue while nothing is listening at `peer_port` — `chat_send` must
    // return immediately with the message Pending, not block on delivery.
    let peer_device = json!({
        "id": peer_id,
        "name": peer_id,
        "addresses": ["127.0.0.1"],
        "port": peer_port,
    });
    let sent = call_json(
        pb_chat_send,
        &json!({ "peer": peer_device, "text": "offline then online" }),
    );
    assert_eq!(sent["ok"], true, "chat_send: {sent}");
    let msg_id = sent["data"]["id"].as_str().expect("id string").to_string();

    let hist = call_json(pb_chat_history, &json!({ "peer_id": peer_id }));
    let messages = hist["data"]["messages"].as_array().expect("messages array");
    let pending = messages
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("enqueued message present in history");
    assert_eq!(
        pending["status"], "pending",
        "chat_send must enqueue Pending without blocking while the peer is unreachable"
    );

    // `chat_send` already spawned its own one-shot opportunistic flush attempt
    // (see `Manager::chat_send`), dialing `peer_port` in the background right
    // now while nothing is listening there. That dial is bounded by
    // `peerbeam_transfer_quic`'s 8s `CONNECT_TIMEOUT`; if the peer's listener
    // came up *during* that window, a QUIC retransmit could still land on it
    // and this attempt — not the drain loop — would be what delivers the
    // message, making the test pass for the wrong reason. Wait past that
    // timeout first so the opportunistic flush has conclusively failed (the
    // record is still Pending, asserted above) before the peer ever exists —
    // only the periodic drain tick can succeed after this point.
    tokio::time::sleep(Duration::from_secs(9)).await;

    // 2) Bring the peer online: a real QUIC listener at the exact address the
    // Pending message targets, plus a periodic discovery announce so
    // `engine.devices()` reports it online for the drain loop to find.
    let (enc, trust, identity) = peer_identity(dir.path(), peer_id);
    let recv_quic = QuicTransport::new().unwrap();
    let (_addr, mut incoming) = recv_quic
        .serve_channels_on(format!("127.0.0.1:{peer_port}").parse().unwrap())
        .await
        .unwrap();
    let peer_store = peer_chat_store(dir.path(), peer_id, 99);

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

    // The manual peer never dials the FFI engine, so `handle_incoming`'s own
    // flush-on-connect cannot be what delivers this message — only the new
    // periodic drain tick can, since it dials out on the FFI's own schedule.
    let announce_task = spawn_periodic_announce(peer_id, peer_port);

    // 3) Wait (bounded, generous enough for one ~15s `DRAIN_EVERY` tick
    // measured from `pb_init`, of which ~9s has already elapsed above) for the
    // drain to actually flush it: the peer receives the message, the FFI
    // emits `chat_status: "sent"`, and `pb_chat_history` flips the record
    // from Pending to Sent.
    let got = wait_record(20, &received)
        .expect("drain loop did not deliver the queued message to the peer in time");
    assert_eq!(got.body, "offline then online");
    assert_eq!(got.id, msg_id, "same message id round-trips");

    let event = wait_event(5, |e| {
        e["type"] == "chat_status" && e["message_id"] == msg_id && e["status"] == "sent"
    })
    .expect("expected a chat_status \"sent\" event for the drained message");
    assert_eq!(event["peer_id"], peer_id);

    let sent_msg = wait_chat_status(5, peer_id, &msg_id, "sent")
        .expect("pb_chat_history did not flip the record to Sent after the drain flush");
    assert_eq!(sent_msg["status"], "sent");
    assert_eq!(sent_msg["direction"], "out");

    // Stop refreshing the peer's discovered presence before shutdown; no
    // further assertions depend on it.
    announce_task.abort();
    pb_shutdown();
}
