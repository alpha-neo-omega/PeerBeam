//! Interrupted transfers, end to end over the C ABI with a real peer.
//!
//! The properties under test are the two the feature can get catastrophically
//! wrong, plus the plumbing that makes it useful at all:
//!
//! * **A checkpoint binds to its transfer.** Resuming appends to a partial
//!   file, so a checkpoint accepted against the wrong peer, the wrong file or
//!   the wrong size would corrupt it.
//! * **Consent is not laundered.** An inbound transfer the user accepted
//!   resumes without a second prompt; one that was never accepted must not
//!   become resumable into an accepted one. A crash is not an approval.
//! * An interrupted send leaves a checkpoint, a completed one leaves none, the
//!   survivors come back after a restart with their real progress, a resume
//!   continues from the receiver's offset rather than from zero, and a discard
//!   takes the partial bytes with it.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType};
use peerbeam_ffi::*;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_on_channel, send_file_on_session, ChannelReceived, Identity, PeerSession, SendRequest,
    SessionConfig, SessionHandle, SessionRole, TransferControl,
};
use peerbeam_transfer_quic::{direct_route, QuicTransport};
use peerbeam_trust_fs::FsTrust;

// ── harness (mirrors tests/transfer_ffi.rs) ─────────────────────

fn peer_cfg() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(ChannelType::TRANSFER)))
        .with_stream_channel_type(ChannelType::TRANSFER)
}

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
    // 0 lets the OS assign: a fixed discovery port would collide with any
    // other test (or a real daemon) running alongside.
    cfg.discovery.port = 0;
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    cfg.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
    cfg.device.auto_accept_trusted = false;
    std::fs::create_dir_all(dir.join("recv")).unwrap();
    let c = CString::new(serde_json::to_string(&cfg).unwrap()).unwrap();
    let v = take(unsafe { pb_init(c.as_ptr()) });
    assert_eq!(v["ok"], true, "init: {v}");
}

fn checkpoint_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("data").join("checkpoints")
}

fn checkpoint_file(dir: &std::path::Path, id: &str) -> std::path::PathBuf {
    checkpoint_dir(dir).join(format!("{id}.json"))
}

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

/// A second (third, …) connection from an identity that has already been
/// pinned.
///
/// [`peer_identity`] mints a **fresh keypair** every call, so calling it again
/// for the same device id is a TOFU key change — which the engine refuses, as
/// it should. A test that re-offers a file has to come back as the same device
/// it was the first time.
fn same_peer(dir: &std::path::Path, identity: &Identity) -> (AeadCrypto, FsTrust, Identity) {
    let trust = FsTrust::open(dir.join(format!("{}-trust.json", identity.name))).unwrap();
    (AeadCrypto::new(), trust, identity.clone())
}

fn dial_meta() -> TransferSession {
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
        accepted: true,
    }
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// The `{id,name,addresses,port}` object a surface hands `pb_transfer_send` —
/// and, for a resume, `pb_transfer_resume_interrupted`.
fn peer_json(id: &str, port: u16) -> Value {
    json!({
        "id": id,
        "name": id,
        "addresses": ["127.0.0.1"],
        "port": port,
    })
}

// ── a peer that receives (and can walk away mid-transfer) ───────

/// A receiving peer on its own port and identity.
struct RecvPeer {
    port: u16,
    handle: tokio::task::JoinHandle<()>,
}

/// Serve exactly one inbound transfer, writing into `dir`.
///
/// `stop_after` bounds how many bytes the receiver will accept before it drops
/// the link mid-stream, which is how these tests interrupt a transfer: the
/// sender sees a plain connection fault, exactly as it would if the Wi-Fi had
/// gone. `None` receives to completion.
async fn serve_one(
    name: &'static str,
    root: std::path::PathBuf,
    dir: std::path::PathBuf,
    stop_after: Option<u64>,
) -> RecvPeer {
    serve_legs(name, root, dir, vec![stop_after]).await
}

/// Serve `legs.len()` successive inbound transfers into the same directory,
/// each with its own byte limit.
///
/// One endpoint, one identity, one destination — which is what makes a resume
/// a resume: the second leg finds the first leg's `.part` exactly where it
/// left it, and comes back as the same TOFU-pinned device.
async fn serve_legs(
    name: &'static str,
    root: std::path::PathBuf,
    dir: std::path::PathBuf,
    legs: Vec<Option<u64>>,
) -> RecvPeer {
    let (enc, trust, identity) = peer_identity(&root, name);
    let trust_path = root.join(format!("{name}-trust.json"));
    let quic = QuicTransport::new().unwrap();
    let (addr, mut incoming) = quic
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let port = addr.port();
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.to_string_lossy().into_owned();

    let handle = tokio::spawn(async move {
        use futures::StreamExt;
        for stop_after in legs {
            let Some(Ok(qc)) = incoming.next().await else {
                return;
            };
            let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
            let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
            let trust: Arc<dyn TrustStore> = Arc::new(FsTrust::open(trust_path.clone()).unwrap());
            let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
            let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
            let (inc, mut inc_rx) = tokio::sync::mpsc::unbounded_channel();
            let Ok(mut ps) = PeerSession::open(
                transport,
                SessionRole::Responder,
                peer_cfg(),
                ev,
                ch,
                inc,
                None,
                identity.clone(),
                enc,
                trust,
            )
            .await
            else {
                return;
            };
            let session: SessionHandle = ps.handle();
            let pump = tokio::spawn(async move {
                let _ = ps.run().await;
            });
            let Some(channel) = inc_rx.recv().await else {
                return;
            };
            let (ptx, mut prx) =
                tokio::sync::mpsc::unbounded_channel::<peerbeam_domain::entity::Progress>();
            let ctrl = TransferControl::new();
            // Walking away: cancelling `ctrl` past `stop_after` bytes tears the
            // transfer down from this side, which is what the sender
            // experiences as an interruption.
            if let Some(limit) = stop_after {
                let ctrl = ctrl.clone();
                tokio::spawn(async move {
                    while let Some(p) = prx.recv().await {
                        if p.transferred_bytes >= limit {
                            ctrl.cancel();
                            return;
                        }
                    }
                });
            } else {
                tokio::spawn(async move { while prx.recv().await.is_some() {} });
            }
            let _: Result<ChannelReceived, _> =
                receive_on_channel(channel, &session, &FsStorage::new(), &dest, &ctrl, &ptx).await;
            session.close();
            pump.abort();
        }
    });

    let _ = enc;
    let _ = trust;
    RecvPeer { port, handle }
}

// ── tests ───────────────────────────────────────────────────────

/// The two halves of the same rule, in one run: a transfer that finishes owes
/// nothing to the future, and a transfer that dies owes everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_completed_send_leaves_no_checkpoint_and_an_interrupted_one_does() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49871, dir.path());

    let payload = pattern(1024 * 1024);
    let src = dir.path().join("whole.bin");
    std::fs::write(&src, &payload).unwrap();

    // ── completes ──
    let peer = serve_one(
        "recv-ok",
        dir.path().to_path_buf(),
        dir.path().join("peer-ok"),
        None,
    )
    .await;
    let v = call_json(
        pb_transfer_send,
        &json!({
            "peer": peer_json("recv-ok", peer.port),
            "paths": [src.to_string_lossy()],
            "transfer_id": "done-1",
        }),
    );
    assert_eq!(v["ok"], true, "send: {v}");
    let done = tokio::task::spawn_blocking(|| {
        wait_event(20, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == "done-1"
        })
    })
    .await
    .unwrap();
    assert!(done.is_some(), "the send should have completed");
    let _ = peer.handle.await;
    assert!(
        !checkpoint_file(dir.path(), "done-1").exists(),
        "a completed transfer must not leave a checkpoint behind"
    );

    // ── is interrupted ──
    let peer = serve_one(
        "recv-cut",
        dir.path().to_path_buf(),
        dir.path().join("peer-cut"),
        Some(128 * 1024),
    )
    .await;
    let v = call_json(
        pb_transfer_send,
        &json!({
            "peer": peer_json("recv-cut", peer.port),
            "paths": [src.to_string_lossy()],
            "transfer_id": "cut-1",
        }),
    );
    assert_eq!(v["ok"], true, "send: {v}");
    let ended = tokio::task::spawn_blocking(|| {
        wait_event(30, |e| {
            e["transfer_id"] == "cut-1"
                && (e["type"] == "transfer_failed" || e["type"] == "transfer_cancelled")
        })
    })
    .await
    .unwrap();
    assert!(ended.is_some(), "the send should have ended badly");
    let _ = peer.handle.await;

    let cp = checkpoint_file(dir.path(), "cut-1");
    assert!(
        cp.exists(),
        "an interrupted transfer must leave a checkpoint at {}",
        cp.display()
    );
    let saved: TransferSession = serde_json::from_slice(&std::fs::read(&cp).unwrap()).unwrap();
    assert_eq!(saved.peer, DeviceId::from("recv-cut"));
    assert_eq!(saved.files[0].name, "whole.bin");
    assert_eq!(saved.total_bytes, payload.len() as u64);
    assert!(saved.accepted, "a send is the local user's own action");

    // …and it is what `pb_transfers_interrupted` reports.
    let list = take(pb_transfers_interrupted());
    assert_eq!(list["ok"], true, "{list}");
    let rows = list["data"]["transfers"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["id"] == "cut-1")
        .expect("the interrupted transfer should be listed");
    assert_eq!(row["status"], "interrupted");
    assert_eq!(row["direction"], "sending");
    assert_eq!(row["resumable"], true);
    assert_eq!(row["stats"]["total_bytes"], payload.len() as u64);

    pb_shutdown();
}

/// **The offset property.** A resume continues from what the receiver already
/// has; it does not start the file again.
///
/// Asserted on the bytes actually put on the wire, not on "it finished": a
/// resume that silently restarted from zero would also finish, with a
/// byte-identical file, and would be invisible to any weaker check. The first
/// progress report of the resumed leg is the tell — `send_file` seeds its
/// counter with the negotiated offset, so a real resume opens well past the
/// interruption point and a restart opens at one chunk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_resumed_send_continues_from_the_receivers_offset() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49878, dir.path());

    let payload = pattern(2 * 1024 * 1024);
    let src = dir.path().join("resume.bin");
    std::fs::write(&src, &payload).unwrap();
    let cut = 512 * 1024u64;
    let peer_dir = dir.path().join("peer-resume");

    // One receiver, two legs: the first walks away part-way, the second runs
    // to completion over the partial file the first left.
    let peer = serve_legs(
        "recv-resume",
        dir.path().to_path_buf(),
        peer_dir.clone(),
        vec![Some(cut), None],
    )
    .await;
    let peer_obj = peer_json("recv-resume", peer.port);

    let v = call_json(
        pb_transfer_send,
        &json!({
            "peer": peer_obj,
            "paths": [src.to_string_lossy()],
            "transfer_id": "res-1",
        }),
    );
    assert_eq!(v["ok"], true, "send: {v}");
    let ended = tokio::task::spawn_blocking(|| {
        wait_event(30, |e| {
            e["transfer_id"] == "res-1"
                && matches!(
                    e["type"].as_str(),
                    Some("transfer_failed") | Some("transfer_cancelled")
                )
        })
    })
    .await
    .unwrap();
    assert!(
        ended.is_some(),
        "the first leg should have been interrupted"
    );

    let part = peer_dir.join("resume.bin.part");
    let offset = std::fs::metadata(&part)
        .expect("the receiver keeps its partial file")
        .len();
    assert!(
        offset >= cut,
        "the receiver should be holding the bytes it got: {offset}"
    );
    assert!(
        offset < payload.len() as u64,
        "and not the whole file: {offset}"
    );

    // ── resume ──
    EVENTS.lock().unwrap().clear();
    let v = call_json(
        pb_transfer_resume_interrupted,
        &json!({ "id": "res-1", "peer": peer_obj }),
    );
    assert_eq!(v["ok"], true, "resume: {v}");
    assert_eq!(v["data"]["resumed"], true);

    let done = tokio::task::spawn_blocking(|| {
        wait_event(60, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == "res-1"
        })
    })
    .await
    .unwrap();
    assert!(done.is_some(), "the resumed transfer should have completed");
    let _ = peer.handle.await;

    // The bytes that actually moved. The first progress report of the resumed
    // leg starts from the negotiated offset; a restart would open at one
    // chunk (64 KiB), far below it.
    let first = events_snapshot()
        .into_iter()
        .find(|e| e["type"] == "transfer_progress" && e["transfer_id"] == "res-1")
        .expect("the resumed leg should report progress");
    let opened_at = first["payload"]["stats"]["transferred_bytes"]
        .as_u64()
        .expect("transferred_bytes");
    assert!(
        opened_at >= offset,
        "the resumed send opened at {opened_at} bytes but the receiver already          had {offset} — it restarted from zero instead of resuming"
    );

    let got = std::fs::read(peer_dir.join("resume.bin")).unwrap();
    assert_eq!(got, payload, "and the finished file is byte-exact");
    assert!(
        !checkpoint_file(dir.path(), "res-1").exists(),
        "a resume that completed clears its checkpoint"
    );
    pb_shutdown();
}

/// **The offset property, on the receiving side.** A resumed receive goes back
/// to the directory its own checkpoint records, not to wherever the save
/// directory points now.
///
/// The partial bytes live at `<that directory>/<name>.part`, and the receive
/// engine looks for them relative to the directory it is handed. Re-deriving
/// that from the current settings would find nothing, restart the transfer from
/// zero, and strand the first half in the old folder — and it would do it
/// silently, because the finished file would still be byte-exact. A user who
/// changed their save folder while a transfer was interrupted is the ordinary
/// case, which is why the save directory is deliberately moved here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_resumed_receive_continues_in_its_own_directory_after_the_save_dir_moves() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49880;
    init_ffi(port, dir.path());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let payload = pattern(1024 * 1024);
    let src = dir.path().join("moved.bin");
    std::fs::write(&src, &payload).unwrap();
    let route = direct_route("127.0.0.1", port);
    let first_dir = dir.path().join("recv");
    let second_dir = dir.path().join("recv-elsewhere");
    std::fs::create_dir_all(&second_dir).unwrap();

    // ── accepted, then cut ──
    let (enc, trust, sender) = peer_identity(dir.path(), "sender");
    let cut = offer_file_cut(
        &route,
        enc,
        trust,
        sender.clone(),
        "move-1",
        src.clone(),
        payload.len() as u64,
        256 * 1024,
    );
    let driver = async {
        let q = tokio::task::spawn_blocking(|| {
            wait_event(10, |e| {
                e["type"] == "transfer_queued" && e["transfer_id"] == "move-1"
            })
        })
        .await
        .unwrap();
        assert!(q.is_some(), "queued");
        let v = call_json(pb_transfer_accept, &json!({ "id": "move-1" }));
        assert_eq!(v["ok"], true, "accept: {v}");
    };
    let (_r, ()) = tokio::join!(cut, driver);

    let ended = tokio::task::spawn_blocking(|| {
        wait_event(20, |e| {
            e["transfer_id"] == "move-1"
                && matches!(
                    e["type"].as_str(),
                    Some("transfer_failed") | Some("transfer_cancelled")
                )
        })
    })
    .await
    .unwrap();
    assert!(ended.is_some(), "the receive should have settled");

    let part = first_dir.join("moved.bin.part");
    let offset = std::fs::metadata(&part)
        .expect("the interrupted receive keeps its partial file")
        .len();
    assert!(offset > 0 && offset < payload.len() as u64, "{offset}");

    // ── the user changes where received files go ──
    let v = call_json(
        pb_settings_set,
        &json!({ "transfer_directory": second_dir.to_string_lossy() }),
    );
    assert_eq!(v["ok"], true, "settings: {v}");

    // ── the sender offers it again ──
    EVENTS.lock().unwrap().clear();
    let (enc, trust, identity) = same_peer(dir.path(), &sender);
    let again = offer_file(
        &route,
        enc,
        trust,
        identity,
        "move-1",
        src.clone(),
        payload.len() as u64,
    );
    let watcher = tokio::task::spawn_blocking(|| {
        wait_event(30, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == "move-1"
        })
    });
    let (send_res, done) = tokio::join!(again, watcher);
    assert!(send_res.is_ok(), "the re-offer should have gone through");
    assert!(done.unwrap().is_some(), "and completed");

    // It finished where it started, over its own partial file.
    let landed = first_dir.join("moved.bin");
    assert!(
        landed.is_file(),
        "a resumed receive must finish in the directory its checkpoint records, \
         not in the one the settings now name"
    );
    assert_eq!(std::fs::read(&landed).unwrap(), payload, "byte-exact");
    assert!(!part.exists(), "and the partial file is promoted, not left");
    assert!(
        !second_dir.join("moved.bin").exists() && !second_dir.join("moved.bin.part").exists(),
        "nothing should have been written to the new save directory: that would \
         mean the transfer restarted from zero and stranded its first half"
    );

    // The bytes that actually moved: the second leg opened past the offset.
    let first = events_snapshot()
        .into_iter()
        .find(|e| e["type"] == "transfer_progress" && e["transfer_id"] == "move-1")
        .expect("the resumed leg should report progress");
    let opened_at = first["payload"]["stats"]["transferred_bytes"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        opened_at >= offset,
        "the resumed receive opened at {opened_at} with {offset} already on \
         disk — it restarted from zero"
    );
    pb_shutdown();
}

/// Integrity survives resume. A resumed file whose partial bytes are not the
/// bytes the sender sent fails loudly, and never lands.
///
/// This is the failure mode the binding check is a *first* line of defence
/// against, not the only one: the binding compares peer, name and size, and a
/// corrupted-but-same-size prefix passes all three. The whole-file checksum is
/// what catches the rest, and a resume must not weaken it — the sender seeds
/// its hash with its own 0..offset prefix, so the two only agree if the
/// receiver's prefix is genuinely the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_resumed_transfer_with_a_corrupted_prefix_fails_its_checksum() {
    let dir = tempfile::tempdir().unwrap();
    init_ffi(49879, dir.path());

    let payload = pattern(1024 * 1024);
    let src = dir.path().join("poison.bin");
    std::fs::write(&src, &payload).unwrap();
    let peer_dir = dir.path().join("peer-poison");

    let peer = serve_legs(
        "recv-poison",
        dir.path().to_path_buf(),
        peer_dir.clone(),
        vec![Some(256 * 1024), None],
    )
    .await;
    let peer_obj = peer_json("recv-poison", peer.port);

    let v = call_json(
        pb_transfer_send,
        &json!({
            "peer": peer_obj,
            "paths": [src.to_string_lossy()],
            "transfer_id": "poison-1",
        }),
    );
    assert_eq!(v["ok"], true, "send: {v}");
    let ended = tokio::task::spawn_blocking(|| {
        wait_event(30, |e| {
            e["transfer_id"] == "poison-1"
                && matches!(
                    e["type"].as_str(),
                    Some("transfer_failed") | Some("transfer_cancelled")
                )
        })
    })
    .await
    .unwrap();
    assert!(
        ended.is_some(),
        "the first leg should have been interrupted"
    );

    // Corrupt the partial file: same length, different bytes. Nothing in the
    // binding check can see this, and nothing should have to — the checksum
    // is the backstop.
    let part = peer_dir.join("poison.bin.part");
    let mut bytes = std::fs::read(&part).unwrap();
    let len_before = bytes.len();
    bytes[0] ^= 0xff;
    std::fs::write(&part, &bytes).unwrap();
    assert_eq!(std::fs::read(&part).unwrap().len(), len_before);

    EVENTS.lock().unwrap().clear();
    let v = call_json(
        pb_transfer_resume_interrupted,
        &json!({ "id": "poison-1", "peer": peer_obj }),
    );
    assert_eq!(v["ok"], true, "resume: {v}");

    let failed = tokio::task::spawn_blocking(|| {
        wait_event(60, |e| {
            e["transfer_id"] == "poison-1" && e["type"] == "transfer_failed"
        })
    })
    .await
    .unwrap()
    .expect("a resumed transfer with a corrupt prefix must fail");
    let msg = failed["payload"]["error"].to_string().to_lowercase();
    assert!(
        msg.contains("checksum") || msg.contains("integrity"),
        "and must say why: {failed}"
    );

    let completed = events_snapshot()
        .into_iter()
        .any(|e| e["type"] == "transfer_completed" && e["transfer_id"] == "poison-1");
    assert!(!completed, "it must not also report success");
    assert!(
        !peer_dir.join("poison.bin").exists(),
        "a file that failed verification must never land"
    );
    let _ = peer.handle.await;
    pb_shutdown();
}

/// A checkpoint written by a run that is gone comes back as an interrupted
/// transfer with the progress it actually reached — not as a fresh row at
/// zero, and not silently dropped.
#[test]
#[serial_test::serial]
fn a_checkpoint_survives_a_restart_with_its_partial_progress() {
    let dir = tempfile::tempdir().unwrap();
    // Written *before* init: from this process's point of view it is
    // indistinguishable from one a previous run left behind.
    std::fs::create_dir_all(checkpoint_dir(dir.path())).unwrap();
    let cp = TransferSession {
        id: TransferId::from("prev-run"),
        peer: DeviceId::from("some-peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: vec![peerbeam_domain::entity::FileEntry {
            path: dir.path().join("big.bin"),
            name: "big.bin".into(),
            size: 4_000,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 4_000,
        transferred_bytes: 2_500,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted: true,
    };
    std::fs::write(
        checkpoint_file(dir.path(), "prev-run"),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();

    init_ffi(49872, dir.path());

    let list = take(pb_transfers_interrupted());
    let rows = list["data"]["transfers"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["id"] == "prev-run")
        .expect("a surviving checkpoint must be visible after a restart");
    assert_eq!(row["status"], "interrupted");
    assert_eq!(row["file"], "big.bin");
    assert_eq!(row["stats"]["transferred_bytes"], 2_500);
    assert_eq!(row["stats"]["total_bytes"], 4_000);

    // And it is announced, so a surface that only listens for events still
    // learns about it.
    let announced = wait_event(5, |e| {
        e["type"] == "transfer_interrupted" && e["transfer_id"] == "prev-run"
    });
    assert!(
        announced.is_some(),
        "startup should announce the checkpoints it found"
    );
    pb_shutdown();
}

/// The binding check, from the outside: a resume aimed at the wrong device, or
/// at a source file that is no longer the one the receiver has a prefix of, is
/// refused before a byte moves.
#[test]
#[serial_test::serial]
fn a_resume_is_refused_against_a_mismatched_peer_or_a_changed_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("bound.bin");
    std::fs::write(&src, pattern(4_000)).unwrap();
    std::fs::create_dir_all(checkpoint_dir(dir.path())).unwrap();
    let cp = TransferSession {
        id: TransferId::from("bound-1"),
        peer: DeviceId::from("peer-a"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: vec![peerbeam_domain::entity::FileEntry {
            path: src.clone(),
            name: "bound.bin".into(),
            size: 4_000,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 4_000,
        transferred_bytes: 1_000,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted: true,
    };
    std::fs::write(
        checkpoint_file(dir.path(), "bound-1"),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();
    init_ffi(49873, dir.path());

    // A peer object naming a *different* device: the caller may say how to
    // reach a device, never which device.
    let v = call_json(
        pb_transfer_resume_interrupted,
        &json!({ "id": "bound-1", "peer": peer_json("peer-b", 49999) }),
    );
    assert_eq!(v["ok"], false, "a different peer must be refused: {v}");

    // The source file is no longer the file the receiver holds a prefix of.
    // Appending its bytes to that prefix would build a file that never
    // existed.
    std::fs::write(&src, pattern(9_000)).unwrap();
    let v = call_json(
        pb_transfer_resume_interrupted,
        &json!({ "id": "bound-1", "peer": peer_json("peer-a", 49999) }),
    );
    assert_eq!(v["ok"], false, "a resized source must be refused: {v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("size"),
        "the refusal should say what changed: {v}"
    );

    // A checkpoint that does not exist is not a resume either.
    let v = call_json(pb_transfer_resume_interrupted, &json!({ "id": "nope" }));
    assert_eq!(v["ok"], false, "{v}");
    pb_shutdown();
}

/// An inbound transfer cannot be pulled — the protocol is sender-driven — and
/// says so instead of offering an action that would do nothing.
#[test]
#[serial_test::serial]
fn an_interrupted_receive_reports_that_its_sender_must_re_offer_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(checkpoint_dir(dir.path())).unwrap();
    let cp = TransferSession {
        id: TransferId::from("in-1"),
        peer: DeviceId::from("peer-a"),
        direction: Direction::Receiving,
        status: TransferStatus::Transferring,
        files: vec![peerbeam_domain::entity::FileEntry {
            path: dir.path().join("recv").join("in.bin"),
            name: "in.bin".into(),
            size: 100,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 100,
        transferred_bytes: 40,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted: true,
    };
    std::fs::write(
        checkpoint_file(dir.path(), "in-1"),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();
    init_ffi(49874, dir.path());

    let list = take(pb_transfers_interrupted());
    let row = list["data"]["transfers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "in-1")
        .cloned()
        .expect("listed");
    assert_eq!(
        row["resumable"], false,
        "a receive cannot be restarted from this side"
    );

    let v = call_json(pb_transfer_resume_interrupted, &json!({ "id": "in-1" }));
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "unsupported", "{v}");
    pb_shutdown();
}

/// Discarding is the way an interrupted transfer stops being clutter — and it
/// must take the partial bytes with it, or a discarded transfer would silently
/// seed the next one of the same name.
#[test]
#[serial_test::serial]
fn discarding_removes_the_checkpoint_and_the_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let recv = dir.path().join("recv");
    std::fs::create_dir_all(&recv).unwrap();
    let part = recv.join("half.bin.part");
    std::fs::write(&part, pattern(500)).unwrap();
    std::fs::create_dir_all(checkpoint_dir(dir.path())).unwrap();
    let cp = TransferSession {
        id: TransferId::from("junk-1"),
        peer: DeviceId::from("peer-a"),
        direction: Direction::Receiving,
        status: TransferStatus::Transferring,
        files: vec![peerbeam_domain::entity::FileEntry {
            path: recv.join("half.bin"),
            name: "half.bin".into(),
            size: 1_000,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 1_000,
        transferred_bytes: 500,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted: true,
    };
    std::fs::write(
        checkpoint_file(dir.path(), "junk-1"),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();
    init_ffi(49875, dir.path());

    let v = call_json(pb_transfer_discard_interrupted, &json!({ "id": "junk-1" }));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["data"]["discarded"], true);
    assert_eq!(v["data"]["partial_removed"], true);
    assert!(!part.exists(), "the partial file must go with the record");
    assert!(!checkpoint_file(dir.path(), "junk-1").exists());

    let list = take(pb_transfers_interrupted());
    assert!(list["data"]["transfers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["id"] != "junk-1"));

    // Discarding again is a clear "there is nothing there", not a silent ok.
    let v = call_json(pb_transfer_discard_interrupted, &json!({ "id": "junk-1" }));
    assert_eq!(v["ok"], false, "{v}");
    pb_shutdown();
}

/// A checkpoint nobody came back to is eventually reclaimed, along with the
/// bytes it was pinning — otherwise `.checkpoints` and every abandoned `.part`
/// grow without bound.
#[test]
#[serial_test::serial]
fn startup_reclaims_checkpoints_that_aged_out_and_keeps_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let recv = dir.path().join("recv");
    std::fs::create_dir_all(&recv).unwrap();
    let stale_part = recv.join("stale.bin.part");
    std::fs::write(&stale_part, pattern(50)).unwrap();
    std::fs::create_dir_all(checkpoint_dir(dir.path())).unwrap();

    let mut stale = TransferSession {
        id: TransferId::from("stale-1"),
        peer: DeviceId::from("peer-a"),
        direction: Direction::Receiving,
        status: TransferStatus::Transferring,
        files: vec![peerbeam_domain::entity::FileEntry {
            path: recv.join("stale.bin"),
            name: "stale.bin".into(),
            size: 100,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 100,
        transferred_bytes: 50,
        started_at: Utc::now() - chrono::Duration::days(30),
        completed_at: None,
        is_resume: false,
        accepted: true,
    };
    std::fs::write(
        checkpoint_file(dir.path(), "stale-1"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();
    stale.id = TransferId::from("fresh-1");
    stale.started_at = Utc::now();
    std::fs::write(
        checkpoint_file(dir.path(), "fresh-1"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();

    init_ffi(49876, dir.path());

    assert!(
        !checkpoint_file(dir.path(), "stale-1").exists(),
        "a checkpoint past its age must be reclaimed at startup"
    );
    assert!(
        !stale_part.exists(),
        "and so must the partial file it was holding open"
    );
    assert!(
        checkpoint_file(dir.path(), "fresh-1").exists(),
        "a recent checkpoint must survive the sweep"
    );
    pb_shutdown();
}

/// **The consent property, both halves.**
///
/// A transfer the user rejected leaves nothing that could make a later offer
/// of the same id auto-accept; a transfer the user accepted and that was then
/// interrupted resumes without asking again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_rejected_transfer_is_never_resumable_but_an_accepted_one_resumes_unprompted() {
    let dir = tempfile::tempdir().unwrap();
    let port = 49877;
    init_ffi(port, dir.path());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let payload = pattern(512 * 1024);
    let src = dir.path().join("offer.bin");
    std::fs::write(&src, &payload).unwrap();
    let route = direct_route("127.0.0.1", port);

    // ── 1. offered and REJECTED ──
    let (enc, trust, sender) = peer_identity(dir.path(), "sender");
    let identity = sender.clone();
    let offer = offer_file(
        &route,
        enc,
        trust,
        identity,
        "reject-me",
        src.clone(),
        payload.len() as u64,
    );
    let driver = async {
        let q = tokio::task::spawn_blocking(|| {
            wait_event(10, |e| {
                e["type"] == "transfer_queued" && e["transfer_id"] == "reject-me"
            })
        })
        .await
        .unwrap();
        assert!(q.is_some(), "the offer should have raised a prompt");
        let v = call_json(pb_transfer_reject, &json!({ "id": "reject-me" }));
        assert_eq!(v["ok"], true, "reject: {v}");
    };
    let (_send, ()) = tokio::join!(offer, driver);

    assert!(
        !checkpoint_file(dir.path(), "reject-me").exists(),
        "a transfer the user turned down must leave nothing behind that could \
         later be read as consent"
    );

    // Offering it again must prompt again. If the rejection had somehow left a
    // resumable record, this second offer would sail past the gate.
    EVENTS.lock().unwrap().clear();
    let (enc, trust, identity) = same_peer(dir.path(), &sender);
    let offer = offer_file(
        &route,
        enc,
        trust,
        identity,
        "reject-me",
        src.clone(),
        payload.len() as u64,
    );
    let driver = async {
        let q = tokio::task::spawn_blocking(|| {
            wait_event(10, |e| {
                e["type"] == "transfer_queued" && e["transfer_id"] == "reject-me"
            })
        })
        .await
        .unwrap();
        assert!(q.is_some(), "queued again");
        // It is genuinely waiting on a person: a transfer that had been
        // auto-accepted would have no pending decision to reject.
        let v = call_json(pb_transfer_reject, &json!({ "id": "reject-me" }));
        assert_eq!(
            v["ok"], true,
            "a re-offer of a rejected transfer must still be waiting on the \
             user, not already accepted: {v}"
        );
    };
    let (_send, ()) = tokio::join!(offer, driver);

    // ── 2. offered, ACCEPTED, interrupted, offered again ──
    EVENTS.lock().unwrap().clear();
    let (enc, trust, identity) = same_peer(dir.path(), &sender);
    let cut = offer_file_cut(
        &route,
        enc,
        trust,
        identity,
        "keep-me",
        src.clone(),
        payload.len() as u64,
        128 * 1024,
    );
    let driver = async {
        let q = tokio::task::spawn_blocking(|| {
            wait_event(10, |e| {
                e["type"] == "transfer_queued" && e["transfer_id"] == "keep-me"
            })
        })
        .await
        .unwrap();
        assert!(q.is_some(), "queued");
        let v = call_json(pb_transfer_accept, &json!({ "id": "keep-me" }));
        assert_eq!(v["ok"], true, "accept: {v}");
    };
    let (_send, ()) = tokio::join!(cut, driver);

    // Let the interrupted leg finish unwinding before offering again: while it
    // is still registered, its id is taken, the re-offer would be registered
    // under a fresh one, and it would (correctly, if uselessly for this test)
    // be treated as a new transfer rather than a resume.
    let ended = tokio::task::spawn_blocking(|| {
        wait_event(20, |e| {
            e["transfer_id"] == "keep-me"
                && matches!(
                    e["type"].as_str(),
                    Some("transfer_failed") | Some("transfer_cancelled")
                )
        })
    })
    .await
    .unwrap();
    assert!(
        ended.is_some(),
        "the interrupted receive should have settled"
    );

    // The accepted-then-interrupted receive left a checkpoint, and it records
    // the consent.
    let cp_path = checkpoint_file(dir.path(), "keep-me");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cp_path.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        cp_path.exists(),
        "an accepted receive that was interrupted must keep a checkpoint"
    );
    let saved: TransferSession = serde_json::from_slice(&std::fs::read(&cp_path).unwrap()).unwrap();
    assert!(saved.accepted, "the consent must be what is persisted");
    assert_eq!(saved.direction, Direction::Receiving);

    // Offer it again: no prompt this time, and it completes on its own.
    EVENTS.lock().unwrap().clear();
    let (enc, trust, identity) = same_peer(dir.path(), &sender);
    let again = offer_file(
        &route,
        enc,
        trust,
        identity,
        "keep-me",
        src.clone(),
        payload.len() as u64,
    );
    let watcher = tokio::task::spawn_blocking(|| {
        wait_event(30, |e| {
            e["type"] == "transfer_completed" && e["transfer_id"] == "keep-me"
        })
    });
    let (send_res, done) = tokio::join!(again, watcher);
    assert!(send_res.is_ok(), "the re-offer should have gone through");
    assert!(
        done.unwrap().is_some(),
        "an interrupted transfer the user already accepted must resume without \
         a second prompt — nothing here ever called pb_transfer_accept"
    );

    let got = std::fs::read(dir.path().join("recv").join("offer.bin")).unwrap();
    assert_eq!(got, payload, "and it lands byte-exact");
    assert!(
        !cp_path.exists(),
        "a completed transfer clears its checkpoint"
    );
    pb_shutdown();
}

// ── offering a file into the FFI engine ─────────────────────────

/// Open a session to the FFI engine and offer one file over a transfer
/// channel, to completion.
async fn offer_file(
    route: &peerbeam_domain::entity::Route,
    enc: AeadCrypto,
    trust: FsTrust,
    identity: Identity,
    transfer_id: &str,
    src: std::path::PathBuf,
    size: u64,
) -> Result<(), String> {
    offer(route, enc, trust, identity, transfer_id, src, size, None).await
}

/// The same, but the sender walks away after `cut` bytes.
#[allow(clippy::too_many_arguments)]
async fn offer_file_cut(
    route: &peerbeam_domain::entity::Route,
    enc: AeadCrypto,
    trust: FsTrust,
    identity: Identity,
    transfer_id: &str,
    src: std::path::PathBuf,
    size: u64,
    cut: u64,
) -> Result<(), String> {
    offer(
        route,
        enc,
        trust,
        identity,
        transfer_id,
        src,
        size,
        Some(cut),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn offer(
    route: &peerbeam_domain::entity::Route,
    enc: AeadCrypto,
    trust: FsTrust,
    identity: Identity,
    transfer_id: &str,
    src: std::path::PathBuf,
    size: u64,
    cut: Option<u64>,
) -> Result<(), String> {
    let quic = QuicTransport::new().unwrap();
    let qc = quic
        .dial_channels(route, &dial_meta())
        .await
        .map_err(|e| e.to_string())?;
    let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
    let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
    let trust: Arc<dyn TrustStore> = Arc::new(trust);
    let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
    let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
    let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
    let mut ps = PeerSession::open(
        transport,
        SessionRole::Initiator,
        peer_cfg(),
        ev,
        ch,
        inc,
        None,
        identity,
        enc,
        trust,
    )
    .await
    .map_err(|e| e.to_string())?;
    let handle = ps.handle();
    let pump = tokio::spawn(async move {
        let _ = ps.run().await;
    });
    let (ptx, mut prx) =
        tokio::sync::mpsc::unbounded_channel::<peerbeam_domain::entity::Progress>();
    let ctrl = TransferControl::new();
    if let Some(limit) = cut {
        let ctrl = ctrl.clone();
        tokio::spawn(async move {
            while let Some(p) = prx.recv().await {
                if p.transferred_bytes >= limit {
                    ctrl.cancel();
                    return;
                }
            }
        });
    } else {
        tokio::spawn(async move { while prx.recv().await.is_some() {} });
    }
    let req = SendRequest {
        transfer_id: transfer_id.into(),
        name: src.file_name().unwrap().to_string_lossy().into_owned(),
        path: src.to_string_lossy().into_owned(),
        size,
        chunk_size: 64 * 1024,
    };
    let r = send_file_on_session(&handle, &FsStorage::new(), req, &ctrl, &ptx, 3).await;
    handle.close();
    pump.abort();
    r.map(|_| ()).map_err(|e| e.to_string())
}
