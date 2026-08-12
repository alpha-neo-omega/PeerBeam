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

use peerbeam_chat::{ChatHandler, ChatRecord, ChatStore, FileRef, ReceivedSink};
use peerbeam_config::EngineConfig;
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_discovery_udp::DEFAULT_DISCOVERY_PORT;
use peerbeam_domain::entity::{Direction, Route, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, Frame, FrameKind, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, CHAT_FEAT_FILEREF,
};
use peerbeam_ffi::*;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_on_channel, send_file_on_session, ChannelReceived, HandlerRegistry, Identity,
    PeerSession, SendRequest, SessionConfig, SessionHandle, SessionRole, TransferControl,
    TransferOutcome,
};
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

/// What a peer that shares a file inside a chat thread advertises: CHAT with
/// the `FileRef` feature bit (so the FFI engine's negotiated set keeps it) AND
/// TRANSFER as a stream capability, since the bytes ride the ordinary transfer
/// channel — the whole point of the design is that this is two existing
/// channels correlated by one id, not a new transport.
fn chat_and_transfer_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::with_features(
            ChannelType::CHAT,
            CHAT_FEAT_FILEREF,
        ))
        .with(Capability::new(ChannelType::TRANSFER))
}

/// What a peer whose build predates file-in-chat advertises: CHAT **without**
/// the feature bit, but still a full TRANSFER stream capability. The transfer
/// half is deliberately present so that a refusal can only be attributable to
/// the missing `CHAT_FEAT_FILEREF` — a peer with no TRANSFER at all could fail
/// for an unrelated reason and would make the test undiscriminating.
fn chat_without_fileref_but_transfer_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::new(ChannelType::CHAT))
        .with(Capability::new(ChannelType::TRANSFER))
}

/// Send one `FileRef` over a live session's CHAT channel.
///
/// Hand-rolled here rather than reusing a library helper because the sending
/// half of file-in-chat does not exist yet — this test stands in for a peer
/// that already has it, which is exactly what the receiving side must
/// interoperate with.
async fn send_file_ref(handle: &SessionHandle, r: &FileRef) {
    let channel = handle
        .open_channel(ChannelType::CHAT)
        .await
        .expect("open chat channel");
    // `open_channel` returns as soon as the open is queued locally; wait for the
    // peer's accept before sending, or the send races it and hard-fails.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let open = handle
            .channels()
            .await
            .expect("channel snapshot")
            .iter()
            .any(|c| c.id == channel && c.state.is_open());
        if open {
            break;
        }
        assert!(Instant::now() < deadline, "chat channel never opened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let frame = r.to_frame(channel).expect("encode FileRef");
    handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
        .await
        .expect("send FileRef");
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

/// Regression test for `STREAM_GRACE` (`peerbeam-ffi/src/transfer.rs`):
/// `handle_incoming` bounds how long it waits for the peer's first transfer
/// stream channel before concluding "chat-only dial, close quietly".
/// `open_stream_channel` sends no probe frame (unlike `open_channel`), so
/// that wait resolves only on the sender's first application WRITE on the
/// stream — not on the `ChannelOpen` control message. A real folder sender's
/// first write is the manifest, which is only emitted after the entire tree
/// has been recursively enumerated, and on cold-cache/network/FUSE storage
/// that can take many seconds.
///
/// This test stands in for that delay directly: a real peer completes the
/// session handshake immediately, then deliberately waits 5s — comfortably
/// longer than the pre-fix 3s grace — before opening its transfer stream and
/// writing a file. The FFI engine must still register the transfer
/// (`transfer_queued`) and let it complete, not time out and close the
/// session out from under the late sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn late_opening_sender_stream_is_not_dropped_by_stream_grace() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49904;
    init_ffi(port, dir.path());

    let payload = b"late stream payload, well past the old 3s grace".to_vec();
    let src = dir.path().join("late.bin");
    std::fs::write(&src, &payload).unwrap();

    let (enc, trust, identity) = peer_identity(dir.path(), "late-sender");
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    let send_fut = async {
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg =
            SessionConfig::new(CapabilitySet::new().with(Capability::new(ChannelType::TRANSFER)))
                .with_stream_channel_type(ChannelType::TRANSFER);
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

        // The handshake above is already done; simulate a slow recursive
        // folder-enumeration delay before the sender's first write — the
        // exact gap `STREAM_GRACE` exists to survive.
        tokio::time::sleep(Duration::from_secs(5)).await;

        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: "late-send".into(),
            name: "late.bin".into(),
            path: src.to_string_lossy().into(),
            size: payload.len() as u64,
            chunk_size: 64 * 1024,
        };
        send_file_on_session(&handle, &FsStorage::new(), req, &ctrl, &ptx, 3).await
    };

    let driver = async {
        let queued = tokio::task::spawn_blocking(|| {
            wait_event(15, |e| {
                e["type"] == "transfer_queued" && e["payload"]["incoming"] == true
            })
        })
        .await
        .unwrap()
        .expect(
            "expected transfer_queued for the late-opening sender's stream: \
             STREAM_GRACE must outlast the sender's delay before its first write",
        );
        let id = queued["transfer_id"].as_str().unwrap().to_string();
        let v = call_json(pb_transfer_accept, &json!({ "id": id }));
        assert_eq!(v["ok"], true, "accept: {v}");
        id
    };

    let (send_res, recv_id) = tokio::join!(send_fut, driver);
    assert_eq!(send_res.unwrap(), TransferOutcome::Completed);

    let done = tokio::task::spawn_blocking(move || {
        wait_event(5, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == recv_id
        })
    })
    .await
    .unwrap();
    assert!(
        done.is_some(),
        "expected transfer_completed for the late-opened stream"
    );

    let got = std::fs::read(dir.path().join("recv").join("late.bin")).unwrap();
    assert_eq!(got, payload, "received file byte-exact");

    pb_shutdown();
}

/// **The correlation crux.** A real peer offers a file inside a chat thread:
/// it sends a `FileRef` on CHAT and then sends the bytes over TRANSFER using
/// the *same* id as `SendRequest.transfer_id`. The FFI engine must bind the two
/// into one thing:
///
/// - the transfer is registered under the SENDER's id, not a locally minted
///   one, so the chat row and the transfer are the same identity on both ends;
/// - the approval prompt carries the real name and size, learned by peeking
///   the transfer's first frame before registering — not the "(incoming)"
///   placeholder that was all the receiver could say before it held the stream;
/// - every transfer event carries `peer_id`, so a surface can route it to a
///   conversation (the human-readable peer name is neither unique nor stable).
///
/// Before this task all four of those assertions failed: the receiver minted
/// `tx-<pid>-<n>`, the payload had no `file`/`size`/`peer_id` at all, and
/// nothing tied the chat row to the transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn file_ref_and_its_transfer_share_one_id_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49905;
    init_ffi(port, dir.path());

    // A 4096-byte "report.pdf" so the size assertion is a real wire value.
    let payload = vec![9u8; 4096];
    let src = dir.path().join("report.pdf");
    std::fs::write(&src, &payload).unwrap();

    let sender_device_id = "file-ref-sender";
    let (enc, trust, identity) = peer_identity(dir.path(), sender_device_id);
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    // The id the sender mints ONCE and uses for both the chat message and the
    // transfer — the entire correlation mechanism in one value.
    let file_ref = FileRef::new("report.pdf", payload.len() as u64).unwrap();
    let file_ref_id = file_ref.id.clone();

    let send_fut = async {
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_and_transfer_caps())
            .with_stream_channel_type(ChannelType::TRANSFER);
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
        // The FFI engine must have negotiated the feature bit with us.
        assert_eq!(
            ps.capabilities().features(ChannelType::CHAT),
            Some(CHAT_FEAT_FILEREF),
            "the engine must advertise CHAT_FEAT_FILEREF so it survives intersection"
        );
        let handle = ps.handle();
        tokio::spawn(async move {
            let _ = ps.run().await;
        });

        // 1) The chat row: a FileRef on the CHAT channel.
        send_file_ref(&handle, &file_ref).await;

        // 2) The bytes: an ordinary transfer, tagged with the SAME id.
        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: file_ref.id.clone(),
            name: "report.pdf".into(),
            path: src.to_string_lossy().into(),
            size: payload.len() as u64,
            chunk_size: 1024,
        };
        send_file_on_session(&handle, &FsStorage::new(), req, &ctrl, &ptx, 3).await
    };

    let expected_id = file_ref_id.clone();
    let driver = async move {
        // The approval prompt must carry the real name and size — not "(incoming)".
        let queued = tokio::task::spawn_blocking(|| {
            wait_event(15, |e| {
                e["type"] == "transfer_queued" && e["payload"]["incoming"] == true
            })
        })
        .await
        .unwrap()
        .expect("expected transfer_queued for the incoming file");

        assert_eq!(
            queued["transfer_id"], expected_id,
            "the receiver must register under the SENDER'S id, not a minted one"
        );
        let p = &queued["payload"];
        assert_eq!(
            p["file"], "report.pdf",
            "the peeked name reaches the prompt"
        );
        assert_eq!(p["size"], 4096, "the peeked size reaches the prompt");
        assert_eq!(
            p["peer_id"], "file-ref-sender",
            "events must carry the peer device id"
        );

        let v = call_json(pb_transfer_accept, &json!({ "id": expected_id }));
        assert_eq!(v["ok"], true, "accept by the sender's id: {v}");
        expected_id
    };

    let (send_res, recv_id) = tokio::join!(send_fut, driver);
    assert_eq!(send_res.unwrap(), TransferOutcome::Completed);

    let done = tokio::task::spawn_blocking(move || {
        wait_event(10, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == recv_id
        })
    })
    .await
    .unwrap()
    .expect("expected transfer_completed under the shared id");
    assert_eq!(
        done["payload"]["peer_id"], "file-ref-sender",
        "the terminal event carries the peer device id too"
    );

    // The bytes really landed.
    let got = std::fs::read(dir.path().join("recv").join("report.pdf")).unwrap();
    assert_eq!(got, payload, "received file byte-exact");

    // And the chat row the FileRef created is the SAME id.
    let hist = call_json(pb_chat_history, &json!({ "peer_id": sender_device_id }));
    assert_eq!(hist["ok"], true, "chat_history: {hist}");
    let msgs = hist["data"]["messages"].as_array().unwrap();
    let row = msgs
        .iter()
        .find(|m| m["id"] == file_ref_id)
        .unwrap_or_else(|| panic!("no chat row under the shared id: {msgs:?}"));
    assert_eq!(row["kind"], "file");
    assert_eq!(row["direction"], "in");
    assert_eq!(row["file"]["name"], "report.pdf");
    assert_eq!(row["file"]["size"], 4096);

    pb_shutdown();
}

/// **The two channels can disagree, and the row must side with the bytes.**
///
/// A chat file share is one id spanning two independent channels: the peer's
/// `FileRef` (CHAT) puts the row in the thread and supplies the name and size
/// the bubble renders; the `TransferMeta` (TRANSFER) decides what is actually
/// written to disk. They are correlated **by id alone** — nothing forces them
/// to agree, and a paired peer can simply make them differ.
///
/// Here the offer says `holiday.jpg · 180 KB` while the stream carries
/// `invoice-2026.pdf.exe`. Both moments are asserted:
///
/// 1. **before the approval** — the row the user is deciding on must already
///    read the name and size that will land, not the advertisement. This is the
///    moment that matters: the bubble renders that name directly above Accept.
/// 2. **while it moves** — the row must read `transferring`, not still be
///    offering Accept / Trust / Decline for a decision already made.
/// 3. **after it lands** — the label, the size and the tap-to-open target must
///    all name the same file, and that file must be the one on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_file_refs_claim_never_outranks_what_the_transfer_actually_lands() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49912;
    init_ffi(port, dir.path());

    // What really travels: a small, differently-named file.
    let payload = vec![3u8; 4096];
    let landed_name = "invoice-2026.pdf.exe";
    let src = dir.path().join(landed_name);
    std::fs::write(&src, &payload).unwrap();

    let sender_device_id = "two-faced-sender";
    let (enc, trust, identity) = peer_identity(dir.path(), sender_device_id);
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    // What the conversation is told: a different name and a wildly different
    // size, under the id the transfer will also use.
    let file_ref = FileRef::new("holiday.jpg", 184_320).unwrap();
    let shared_id = file_ref.id.clone();

    let send_fut = async {
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_and_transfer_caps())
            .with_stream_channel_type(ChannelType::TRANSFER);
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

        send_file_ref(&handle, &file_ref).await;

        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: file_ref.id.clone(),
            name: landed_name.into(),
            path: src.to_string_lossy().into(),
            size: payload.len() as u64,
            chunk_size: 1024,
        };
        send_file_on_session(&handle, &FsStorage::new(), req, &ctrl, &ptx, 3).await
    };

    let expected_id = shared_id.clone();
    let driver = async move {
        let queued = tokio::task::spawn_blocking({
            let id = expected_id.clone();
            move || {
                wait_event(15, |e| {
                    e["type"] == "transfer_queued" && e["transfer_id"] == id
                })
            }
        })
        .await
        .unwrap()
        .expect("expected transfer_queued under the shared id");
        assert_eq!(
            queued["payload"]["file"], landed_name,
            "the prompt shows the transfer's name, as it always has"
        );

        // (1) …and so, now, does the conversation row the user is looking at
        // while deciding. `transfer_queued` is emitted after the reconcile, so
        // by here the write has landed.
        let hist = call_json(pb_chat_history, &json!({ "peer_id": sender_device_id }));
        let msgs = hist["data"]["messages"].as_array().unwrap().clone();
        let row = msgs
            .iter()
            .find(|m| m["id"] == expected_id)
            .unwrap_or_else(|| panic!("no chat row under the shared id: {msgs:?}"));
        assert_eq!(row["status"], "pendingapproval", "still awaiting us");
        assert_eq!(
            row["file"]["name"], landed_name,
            "the row must name the file that will land, not the one advertised"
        );
        assert_eq!(
            row["file"]["size"], 4096,
            "and its size, not the advertised 180 KB"
        );

        let v = call_json(pb_transfer_accept, &json!({ "id": expected_id }));
        assert_eq!(v["ok"], true, "accept by the shared id: {v}");
        expected_id
    };

    let (send_res, recv_id) = tokio::join!(send_fut, driver);
    assert_eq!(send_res.unwrap(), TransferOutcome::Completed);

    // (2) The row reported itself in flight rather than still asking.
    let moving = tokio::task::spawn_blocking({
        let id = recv_id.clone();
        move || {
            wait_event(10, |e| {
                e["type"] == "chat_status" && e["message_id"] == id && e["status"] == "transferring"
            })
        }
    })
    .await
    .unwrap();
    assert!(
        moving.is_some(),
        "an accepted chat file must report `transferring`, not keep offering Accept"
    );

    tokio::task::spawn_blocking({
        let id = recv_id.clone();
        move || {
            wait_event(15, |e| {
                e["type"] == "transfer_completed" && e["transfer_id"] == id
            })
        }
    })
    .await
    .unwrap()
    .expect("expected transfer_completed under the shared id");

    // (3) The bytes are on disk under the transfer's name…
    let saved = dir.path().join("recv").join(landed_name);
    assert_eq!(std::fs::read(&saved).unwrap(), payload, "byte-exact");

    // …and the settled row describes exactly that file.
    let row = wait_chat_status(10, sender_device_id, &recv_id, "received")
        .expect("the chat row was not settled received");
    assert_eq!(row["kind"], "file");
    assert_eq!(row["direction"], "in");
    assert_eq!(
        row["file"]["name"], landed_name,
        "a settled row must never keep the advertised name"
    );
    assert_eq!(row["file"]["size"], 4096);
    assert_eq!(
        row["file"]["local_path"],
        saved.to_string_lossy().into_owned(),
        "the label and the tap-to-open target must be the same file"
    );

    pb_shutdown();
}

/// **The send path, end to end.** `pb_chat_send_file` attaches a real file to a
/// conversation with a real peer that advertises `CHAT_FEAT_FILEREF`: the FFI
/// engine dials, checks the negotiated feature, publishes a `FileRef` on the
/// CHAT channel, and streams the bytes over TRANSFER under *the same id*.
///
/// The three things that make this one feature rather than two are all
/// asserted from the peer's own side of the wire:
/// - the peer's `ChatHandler` gets a `FileRef` whose id is the id
///   `pb_chat_send_file` returned;
/// - the peer's `receive_on_channel` reports that same id as the transfer's
///   `transfer_id`, and the bytes land byte-exact;
/// - the sender's own row ends `sent` with `kind == "file"` and its metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_send_file_shares_a_file_in_the_thread_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49907, dir.path());

    let payload = vec![5u8; 4096];
    let src = dir.path().join("invoice.pdf");
    std::fs::write(&src, &payload).unwrap();
    let src_str = src.to_string_lossy().into_owned();

    // The manual peer: a real QUIC listener, its own identity + ChatStore, and
    // a ChatHandler to capture the FileRef it is offered.
    let peer_device_id = "file-receiver";
    let (enc, trust, identity) = peer_identity(dir.path(), peer_device_id);
    let recv_quic = QuicTransport::new().unwrap();
    let (addr, mut incoming) = recv_quic
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_port = addr.port();
    let peer_store = peer_chat_store(dir.path(), peer_device_id, 44);

    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler, peer_slot) = ChatHandler::new(peer_store.clone(), sink);

    let peer_dest = dir.path().join("peer-recv");
    std::fs::create_dir_all(&peer_dest).unwrap();
    let peer_dest_str = peer_dest.to_string_lossy().into_owned();

    // The peer accepts the session, then receives the transfer stream the FFI
    // engine opens on it — the same session that carried the FileRef.
    let peer_task = tokio::spawn(async move {
        use futures::StreamExt;
        let qc = incoming.next().await.unwrap().unwrap();
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, mut inc_rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_and_transfer_caps())
            .with_stream_channel_type(ChannelType::TRANSFER)
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
        let _ = peer_slot.set(ps.peer().clone());
        let handle = ps.handle();
        tokio::spawn(async move {
            let _ = ps.run().await;
        });
        let stream = inc_rx
            .recv()
            .await
            .expect("the FFI engine must open a transfer stream for the attached file");
        let (ptx, _p) = tokio::sync::mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        receive_on_channel(
            stream,
            &handle,
            &FsStorage::new(),
            &peer_dest_str,
            &ctrl,
            &ptx,
        )
        .await
        .expect("peer receives the attached file")
    });

    // FFI sends: `chat_send_file({peer, path}) -> {id}`.
    let peer_json = json!({
        "id": peer_device_id,
        "name": peer_device_id,
        "addresses": ["127.0.0.1"],
        "port": peer_port,
    });
    let sent = call_json(
        pb_chat_send_file,
        &json!({ "peer": peer_json, "path": src_str }),
    );
    assert_eq!(sent["ok"], true, "chat_send_file: {sent}");
    let id = sent["data"]["id"].as_str().expect("id string").to_string();
    assert!(!id.is_empty());

    // 1) The conversation row reached the peer over CHAT, under that id.
    let offered = wait_record(15, &received).expect("peer never received the FileRef");
    assert_eq!(
        offered.id, id,
        "the peer's chat row must use the id chat_send_file returned"
    );
    assert_eq!(offered.kind, peerbeam_chat::Kind::File);
    assert_eq!(offered.direction, peerbeam_chat::Direction::In);
    assert_eq!(offered.status, peerbeam_chat::Status::PendingApproval);
    let offered_meta = offered.file.clone().expect("file meta on the offered row");
    assert_eq!(offered_meta.name, "invoice.pdf");
    assert_eq!(offered_meta.size, 4096);
    assert!(
        offered_meta.local_path.is_none(),
        "the sender's local path must never travel on the wire"
    );

    // 2) The bytes reached the peer over TRANSFER, tagged with the SAME id.
    let got = tokio::time::timeout(Duration::from_secs(30), peer_task)
        .await
        .expect("peer receive did not finish in time")
        .expect("peer task panicked");
    let ChannelReceived::File(file) = got else {
        panic!("expected a single-file receive");
    };
    assert_eq!(
        file.transfer_id, id,
        "the transfer must carry the FileRef's id — that correlation IS the feature"
    );
    assert_eq!(file.name, "invoice.pdf");
    assert_eq!(file.outcome, TransferOutcome::Completed);
    let bytes = std::fs::read(peer_dest.join("invoice.pdf")).unwrap();
    assert_eq!(bytes, payload, "received file byte-exact");

    // 3) Our own row ends `sent`, as a file row, with its metadata intact.
    let row = wait_chat_status(15, peer_device_id, &id, "sent")
        .expect("the sender's chat row never reached status \"sent\"");
    assert_eq!(row["kind"], "file");
    assert_eq!(row["direction"], "out");
    assert_eq!(row["body"], "", "a file row carries no text body");
    assert_eq!(row["file"]["name"], "invoice.pdf");
    assert_eq!(row["file"]["size"], 4096);
    assert_eq!(
        row["file"]["local_path"], src_str,
        "the sender keeps its own path record-side so the UI can open the file"
    );

    // Exactly one row: the FileRef and its transfer must not produce two.
    let hist = call_json(pb_chat_history, &json!({ "peer_id": peer_device_id }));
    let msgs = hist["data"]["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 1, "one file share is one row: {msgs:?}");

    // And a live `chat_status` fired so a surface updates without re-reading.
    let status_ev = tokio::task::spawn_blocking({
        let id = id.clone();
        move || {
            wait_event(5, |e| {
                e["type"] == "chat_status" && e["message_id"] == id && e["status"] == "sent"
            })
        }
    })
    .await
    .unwrap();
    assert!(
        status_ev.is_some(),
        "expected a chat_status \"sent\" for the delivered file"
    );

    pb_shutdown();
}

/// **The refusal.** A peer whose build predates file-in-chat advertises CHAT
/// with no `CHAT_FEAT_FILEREF`, so the negotiated set clears the bit. Attaching
/// a file to that conversation must fail loudly rather than degrade into a plain
/// transfer: the peer would receive an ordinary file with no row in the thread,
/// while our user was told the attachment had been sent.
///
/// So: the row goes `failed` with a reason naming the problem, and **no transfer
/// is started at all** — no `transfer_queued`, nothing in `pb_transfers_active`,
/// and not even a `FileRef` on the peer's CHAT channel.
///
/// The peer here advertises a full TRANSFER capability, so the only thing that
/// can produce this refusal is the missing feature bit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn chat_send_file_refuses_a_peer_that_cannot_receive_attachments() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49908, dir.path());

    let src = dir.path().join("nope.bin");
    std::fs::write(&src, vec![1u8; 128]).unwrap();
    let src_str = src.to_string_lossy().into_owned();

    let peer_device_id = "legacy-peer";
    let (enc, trust, identity) = peer_identity(dir.path(), peer_device_id);
    let recv_quic = QuicTransport::new().unwrap();
    let (addr, mut incoming) = recv_quic
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let peer_port = addr.port();
    let peer_store = peer_chat_store(dir.path(), peer_device_id, 45);

    // Wired exactly as the accepting peer above: if the engine wrongly sent a
    // FileRef anyway, this handler would record it.
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler, peer_slot) = ChatHandler::new(peer_store.clone(), sink);

    // Records any transfer stream the engine opens — there must be none.
    let streams: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let streams_cl = streams.clone();
    // Flips once the peer's own session loop ends, i.e. once the engine has
    // actually closed the session it refused to use. A `?`-style early return
    // that skipped `session.close()` would leave this false — that exact bug
    // has shipped in this feature's history, so it gets a real assertion rather
    // than trust in the control flow.
    let peer_session_ended: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let ended_cl = peer_session_ended.clone();

    tokio::spawn(async move {
        use futures::StreamExt;
        let qc = incoming.next().await.unwrap().unwrap();
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, mut inc_rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SessionConfig::new(chat_without_fileref_but_transfer_caps())
            .with_stream_channel_type(ChannelType::TRANSFER)
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
        let _ = peer_slot.set(ps.peer().clone());
        tokio::spawn(async move {
            while inc_rx.recv().await.is_some() {
                *streams_cl.lock().unwrap() += 1;
            }
        });
        let _ = ps.run().await;
        *ended_cl.lock().unwrap() = true;
    });

    let peer_json = json!({
        "id": peer_device_id,
        "name": peer_device_id,
        "addresses": ["127.0.0.1"],
        "port": peer_port,
    });
    let sent = call_json(
        pb_chat_send_file,
        &json!({ "peer": peer_json, "path": src_str }),
    );
    assert_eq!(sent["ok"], true, "chat_send_file: {sent}");
    let id = sent["data"]["id"].as_str().expect("id string").to_string();

    // The refusal reaches the caller as a `chat_status` failure that says why —
    // the dial is asynchronous, so this is where the message lands.
    let failure = tokio::task::spawn_blocking({
        let id = id.clone();
        move || {
            wait_event(20, |e| {
                e["type"] == "chat_status" && e["message_id"] == id && e["status"] == "failed"
            })
        }
    })
    .await
    .unwrap()
    .expect("expected a chat_status \"failed\" for the refused attachment");
    let reason = failure["error"]
        .as_str()
        .expect("a refusal must carry a reason");
    assert!(
        reason.contains("cannot receive chat attachments"),
        "the reason must name the actual problem, got: {reason}"
    );
    assert_eq!(failure["peer_id"], peer_device_id);

    // The row says so too, and is still a file row.
    let row = wait_chat_status(10, peer_device_id, &id, "failed")
        .expect("the chat row was not marked failed");
    assert_eq!(row["kind"], "file");
    assert_eq!(row["direction"], "out");

    // And nothing was transferred: no queued/started event, no active transfer,
    // no FileRef on the peer's chat channel, no stream opened.
    let events = events_snapshot();
    let transfer_events: Vec<&Value> = events
        .iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(|t| t.as_str()),
                Some("transfer_queued") | Some("transfer_started")
            )
        })
        .collect();
    assert!(
        transfer_events.is_empty(),
        "a refused attachment must start no transfer; got {transfer_events:?}"
    );
    let active = take(pb_transfers_active());
    let list = active["data"]["transfers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        list.is_empty(),
        "a refused attachment must leave no active transfer; got {list:?}"
    );
    assert!(
        received.lock().unwrap().is_empty(),
        "no FileRef may be offered to a peer that cannot understand it"
    );
    assert_eq!(
        *streams.lock().unwrap(),
        0,
        "no transfer stream may be opened for a refused attachment"
    );

    // The refusal path must close the session it dialed, not leak it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !*peer_session_ended.lock().unwrap() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        *peer_session_ended.lock().unwrap(),
        "the refusal path must close the session it dialed"
    );

    pb_shutdown();
}

/// Fail-soft: a peer whose transfer opens with a frame the peek cannot decode
/// (here a bare `Chunk` — no `Meta`, no manifest) must be handled *exactly* as
/// before the peek existed: a locally minted id and the "(incoming)"
/// placeholder. The peek is an optimisation on the prompt, never a
/// precondition for receiving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn undecodable_first_frame_falls_back_to_a_minted_id_and_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49906;
    init_ffi(port, dir.path());

    let (enc, trust, identity) = peer_identity(dir.path(), "garbage-sender");
    let quic = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", port);

    let send_fut = async {
        let qc =
            dial_channels_retrying(&quic, &route, &session_meta(), Duration::from_secs(5)).await;
        let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
        let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
        let trust: Arc<dyn TrustStore> = Arc::new(trust);
        let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
        let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
        let cfg =
            SessionConfig::new(CapabilitySet::new().with(Capability::new(ChannelType::TRANSFER)))
                .with_stream_channel_type(ChannelType::TRANSFER);
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

        // Open a transfer stream and write something that is not a transfer
        // opening at all, then hold the stream so the receiver's own read is
        // what decides the outcome.
        let (_channel, mut link) = handle
            .open_stream_channel(ChannelType::TRANSFER)
            .await
            .expect("open stream channel");
        link.send_frame(Frame {
            kind: FrameKind::Chunk,
            payload: bytes::Bytes::from_static(b"not a transfer opening"),
        })
        .await
        .expect("write garbage");
        // Keep the link (and so the session) alive while the receiver reacts.
        tokio::time::sleep(Duration::from_secs(3)).await;
    };

    let driver = tokio::task::spawn_blocking(|| {
        wait_event(15, |e| {
            e["type"] == "transfer_queued" && e["payload"]["incoming"] == true
        })
    });

    let (_, queued) = tokio::join!(send_fut, driver);
    let queued = queued.unwrap().expect("expected transfer_queued anyway");

    let id = queued["transfer_id"].as_str().unwrap();
    assert!(
        id.starts_with("tx-"),
        "an undecodable opening must fall back to a locally minted id, got {id:?}"
    );
    assert_eq!(
        queued["payload"]["file"], "(incoming)",
        "and to the placeholder display name"
    );
    assert_eq!(queued["payload"]["size"], 0);
    // The peer identity is still known — it comes from the handshake, not the
    // peeked frame — so routing still works even with nothing learned.
    assert_eq!(queued["payload"]["peer_id"], "garbage-sender");

    pb_shutdown();
}
