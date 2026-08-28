//! A folder sync, end to end, over a real session.
//!
//! # Why this exists
//!
//! `Manager::fetch_by_delta` — the whole receive half of a sync — had no test
//! of any kind. It was rewritten (streaming instead of a 256 MiB in-memory
//! ceiling, staged instead of writing over the user's file) on the strength of
//! unit tests for the part it delegates to, and I recorded the wiring as
//! unverified because "serving shares needs the app engine and there is no
//! two-engine harness".
//!
//! That was wrong. A share does not need an app engine to be served — it needs
//! a `SyncHandler` over a `Shares`, which any listening peer can register, and
//! `chat_ffi` has had exactly that shape of manual peer all along. This is that
//! peer with a different handler on it.
//!
//! What it covers that the unit tests cannot: that `pb_sync_pull` reaches a real
//! peer, that the chunk map and chunk requests actually cross a session, and
//! that the bytes land byte-exact at the destination the caller named.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::mpsc::unbounded_channel;

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, SYNC_FEAT_MANIFEST,
};
use peerbeam_ffi::*;
use peerbeam_transfer::{HandlerRegistry, PeerSession, SessionConfig, SessionRole};
use peerbeam_transfer_quic::QuicTransport;

mod common;

use std::ffi::{c_char, CString};

/// Read a `pb_*` answer and free it, exactly as a caller must.
fn take(ptr: *mut c_char) -> Value {
    if ptr.is_null() {
        return Value::Null;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(ptr).to_str().unwrap().to_string() };
    unsafe { pb_free_string(ptr) };
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn call_json(f: unsafe extern "C" fn(*const c_char) -> *mut c_char, v: &Value) -> Value {
    let c = CString::new(v.to_string()).unwrap();
    take(unsafe { f(c.as_ptr()) })
}

/// Stand up the engine this test drives.
fn init_ffi(port: u16, dir: &std::path::Path) {
    let mut cfg = peerbeam_config::EngineConfig::default();
    cfg.transfer.port = port;
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    cfg.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
    std::fs::create_dir_all(dir.join("recv")).unwrap();
    let v = call_json(pb_init, &serde_json::to_value(&cfg).unwrap());
    assert_eq!(v["ok"], true, "init: {v}");
}

/// A trust store that grants everything.
///
/// The sharer's side only: the gates it stands in for — browse and files — have
/// their own tests, and reproducing an approval handshake here would test those
/// instead of the sync path this file exists for. The *engine's* side is not
/// faked: it approves the sharer through `pb_trust_approve`, exactly as a user
/// would, which is also what lets its own answer arms accept the replies.
struct Permissive;

impl TrustStore for Permissive {
    fn record(
        &self,
        _r: peerbeam_domain::entity::TrustRecord,
    ) -> peerbeam_domain::error::Result<()> {
        Ok(())
    }
    fn lookup(
        &self,
        _d: &peerbeam_domain::id::DeviceId,
    ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
        Ok(None)
    }
    fn is_trusted(&self, _d: &peerbeam_domain::id::DeviceId) -> bool {
        true
    }
    fn is_approved(&self, _d: &peerbeam_domain::id::DeviceId) -> bool {
        true
    }
    fn may(
        &self,
        _d: &peerbeam_domain::id::DeviceId,
        _p: peerbeam_domain::entity::Permission,
    ) -> bool {
        true
    }
}

/// A peer that listens and serves one shared folder.
///
/// The answer sinks are wired back onto the session, which is the part a
/// handler alone does not do: `SyncHandler` decides *what* to answer and hands
/// it to a callback, and something has to put that on the wire.
fn spawn_sharing_peer(share: &std::path::Path, port: u16) {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = peerbeam_transfer::Identity {
        device_id: peerbeam_domain::id::DeviceId::from("pb-sharer"),
        name: "sharer".into(),
        keypair,
    };
    let enc: Arc<dyn EncryptionProvider> = Arc::new(enc);
    let trust: Arc<dyn TrustStore> = Arc::new(Permissive);
    let shares = peerbeam_browse::Shares::new([share.to_path_buf()]);

    tokio::spawn(async move {
        use futures::StreamExt;
        let quic = QuicTransport::new().expect("peer quic");
        let (_addr, mut incoming) = quic
            .serve_channels_on(format!("127.0.0.1:{port}").parse().expect("addr"))
            .await
            .expect("peer listen");

        while let Some(Ok(qc)) = incoming.next().await {
            let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
            let identity = identity.clone();
            let enc = enc.clone();
            let trust = trust.clone();
            let shares = shares.clone();

            tokio::spawn(async move {
                // Answers this peer produces, routed to the task below that
                // writes them onto the session.
                let (mtx, mut manifests) = unbounded_channel();
                let (cmtx, mut chunkmaps) = unbounded_channel();
                let (cdtx, mut chunks) = unbounded_channel();

                let (handler, peer_slot) = peerbeam_sync::SyncHandler::with_chunks(
                    shares,
                    trust.clone(),
                    Arc::new(move |m| {
                        let _ = mtx.send(m);
                    }),
                    // The whole-file fallback: no build serves it, and nothing
                    // asks any more since delta streams.
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(move |r| {
                        let _ = cmtx.send(r);
                    }),
                    Arc::new(move |d| {
                        let _ = cdtx.send(d);
                    }),
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                );

                let (ev, _e) = unbounded_channel();
                let (ch, _c) = unbounded_channel();
                let (inc, inc_rx) = unbounded_channel();
                let caps = CapabilitySet::new()
                    .with(Capability::new(ChannelType::CONTROL))
                    .with(Capability::with_features(
                        ChannelType::SYNC,
                        SYNC_FEAT_MANIFEST,
                    ));
                let cfg = SessionConfig::new(caps)
                    .with_handlers(HandlerRegistry::new().with(handler as Arc<dyn MessageHandler>));
                let Ok(mut ps) = PeerSession::open(
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
                else {
                    return;
                };
                let _ = peer_slot.set(ps.peer().clone());
                let handle = ps.handle();
                tokio::spawn(async move {
                    let _held = inc_rx;
                    let _ = ps.run().await;
                });

                // One SYNC channel for every answer. The asker dispatches on
                // message type, not channel, so a single lane back is enough.
                let Ok(channel) = handle.open_channel(ChannelType::SYNC).await else {
                    return;
                };
                loop {
                    tokio::select! {
                        Some(m) = manifests.recv() => {
                            if let Ok(f) = m.to_frame(channel) {
                                let _ = handle
                                    .send_on_channel(channel, f.message_type, f.flags, f.payload)
                                    .await;
                            }
                        }
                        Some(r) = chunkmaps.recv() => {
                            if let Ok(f) = r.to_frame(channel) {
                                let _ = handle
                                    .send_on_channel(channel, f.message_type, f.flags, f.payload)
                                    .await;
                            }
                        }
                        Some(d) = chunks.recv() => {
                            if let Ok(f) = d.to_frame(channel) {
                                let _ = handle
                                    .send_on_channel(channel, f.message_type, f.flags, f.payload)
                                    .await;
                            }
                        }
                        else => break,
                    }
                }
            });
        }
    });
}

/// **The sync path, over a wire.** A file the asker does not have is fetched
/// whole, through the chunk protocol, and written byte-exact where asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_shared_file_is_fetched_and_lands_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let port = common::free_port();
    init_ffi(common::free_port(), dir.path());

    // What the peer shares, and what it holds.
    let share = dir.path().join("shared");
    std::fs::create_dir_all(&share).unwrap();
    let body: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 251) as u8).collect();
    std::fs::write(share.join("report.bin"), &body).unwrap();

    spawn_sharing_peer(&share, port);
    // The listener binds asynchronously; the pull retries below rather than
    // racing it.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let into = dir.path().join("into");
    std::fs::create_dir_all(&into).unwrap();

    let req = json!({
        "peer": {
            "id": "pb-sharer",
            "name": "sharer",
            "addresses": ["127.0.0.1"],
            "port": port,
        },
        "path": "shared",
        "into": into.to_string_lossy(),
    });

    // First contact pins the sharer; then the user approves it, which is what a
    // real sync needs. Without the approval this engine drops the sharer's
    // answers on the floor — an unapproved peer's unsolicited manifests and
    // chunks are refused, deliberately, and that guard is what this sequence
    // demonstrates as well as satisfies.
    let _ = call_json(pb_sync_pull, &req);
    let approved = call_json(pb_trust_approve, &json!({ "id": "pb-sharer" }));
    assert_eq!(approved["ok"], true, "approve: {approved}");

    // Retried: the peer's bind and this engine's dial race, and a first attempt
    // that finds nobody home is a scheduling artefact rather than a result.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let landed = into.join("report.bin");
    let mut last = Value::Null;
    while std::time::Instant::now() < deadline {
        last = call_json(pb_sync_pull, &req);
        if landed.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    assert!(
        landed.exists(),
        "the shared file must arrive; last answer was {last}"
    );
    assert_eq!(
        std::fs::read(&landed).unwrap(),
        body,
        "and it must be byte-exact — the whole point of verifying every chunk"
    );
    assert!(
        !into.join("report.bin.pbsync").exists(),
        "the staging file is renamed away, never left behind"
    );
}
