//! A real two-PeerSession round trip: one side sends a chat message, the other
//! side's ChatHandler persists it and fires the sink.
//!
//! The PeerSession wiring (identities, encryption, trust, `MemTransport`,
//! `PeerSession::open`, spawning `run()`) mirrors
//! `peerbeam-transfer/tests/session.rs::open`/`security` verbatim (via the
//! `common` module copied from that crate's test harness), with the CHAT
//! capability added to both configs and `with_handlers` on the responder's.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::unbounded_channel;

use common::MemTransport;
use peerbeam_appstore_fs::FsAppStore;
use peerbeam_chat::{send_message, ChatHandler, ChatMessage, ChatRecord, ChatStore, ReceivedSink};
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelId, ChannelType, MessageHandler};
use peerbeam_transfer::{HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole};
use peerbeam_trust_fs::FsTrust;

fn chat_store(seed: u8) -> ChatStore {
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir for the test's lifetime so the path stays valid.
    let path = dir.keep().join("appstore");
    let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
    let key = derive_subkey(&[seed; 32], b"peerbeam-appstore-v1");
    ChatStore::new(Arc::new(FsAppStore::open(path, key, enc)))
}

/// Fresh identity, encryption provider, and (leaked-temp-dir) trust store for a
/// test endpoint. Copied verbatim from
/// `peerbeam-transfer/tests/session.rs::security`: each side authenticates
/// with its own keypair + empty trust store (TOFU pins the peer on first
/// contact).
fn security(name: &str) -> (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>) {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: DeviceId::from(name),
        name: name.to_string(),
        keypair,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let trust = FsTrust::open(dir.path().join("trust.json")).expect("trust store");
    // Keep the temp dir alive for the whole test; leaking is fine in tests.
    std::mem::forget(dir);
    (identity, Arc::new(enc), Arc::new(trust))
}

/// Capabilities advertised by both sides: just CHAT (control is implicit).
fn caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::new(ChannelType::CHAT))
}

#[tokio::test]
async fn a_sends_b_receives_and_persists() {
    // ── B: receiver, registers a ChatHandler ────────────────────────────────
    let store_b = chat_store(2);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

    // ── Build two real PeerSessions over an in-memory transport, exactly as
    //    peerbeam-transfer/tests/session.rs's `open()` helper does. ─────────
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _a_ev_rx) = unbounded_channel();
    let (b_ev, _b_ev_rx) = unbounded_channel();
    let (a_ch, _a_ch_rx) = unbounded_channel();
    let (b_ch, _b_ch_rx) = unbounded_channel();
    let (a_in, _a_in_rx) = unbounded_channel();
    let (b_in, _b_in_rx) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let a_id = id_a.device_id.clone();
    let b_id = id_b.device_id.clone();

    // Sender (A, Initiator): advertises CHAT, no chat handler.
    let a_cfg = SessionConfig::new(caps());
    // Receiver (B, Responder): advertises CHAT + registers the ChatHandler.
    let b_cfg = SessionConfig::new(caps()).with_handlers(HandlerRegistry::new().with(handler_b));

    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        a_cfg,
        a_ev,
        a_ch,
        a_in,
        None,
        id_a,
        enc_a,
        trust_a,
    );
    let fb = PeerSession::open(
        tb,
        SessionRole::Responder,
        b_cfg,
        b_ev,
        b_ch,
        b_in,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator opens");
    let mut b = rb.expect("responder opens");
    let a_handle = a.handle();

    // Bind the receiver's peer slot to the sender's device id BEFORE the run
    // loops start dispatching frames (the sender only sends after both
    // sessions are established, so setting it immediately after `open` is
    // safe).
    let _ = peer_slot_b.set(a_id.clone());

    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // ── A sends ──────────────────────────────────────────────────────────
    let store_a = chat_store(1);
    let rec = send_message(&a_handle, &store_a, &b_id, "hello")
        .await
        .expect("send_message succeeds");
    assert_eq!(rec.body, "hello");

    // ── B receives + persists + fires sink (poll briefly rather than a fixed
    //    sleep — real async/network timing). ───────────────────────────────
    let mut got: Option<ChatRecord> = None;
    for _ in 0..200 {
        let snapshot = received.lock().unwrap().clone();
        if let Some(first) = snapshot.into_iter().next() {
            got = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let received_rec = got.expect("B's sink did not fire within 2s");
    assert_eq!(received_rec.body, "hello");
    assert_eq!(received_rec.peer_id, a_id.0);

    let hist = store_b.history(&a_id).expect("store_b history readable");
    assert_eq!(hist.len(), 1, "store_b persisted exactly one record");
    assert_eq!(hist[0].body, "hello");
    assert_eq!(hist[0].id, rec.id, "same message id round-trips");
}

/// Dedup at the handler level: delivering the same frame (same message id)
/// twice must persist and notify only once. Exercised directly against
/// `ChatHandler::handle` rather than over the network, per the task brief.
#[tokio::test]
async fn handler_dedups_same_message_id_without_a_second_network_send() {
    let store_b = chat_store(3);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

    let a_id = DeviceId::from("device-a");
    let _ = peer_slot_b.set(a_id.clone());

    let msg = ChatMessage::new("dup me").expect("valid body");
    let frame1 = msg.to_frame(ChannelId::new(7)).expect("encode frame");
    // Same message id delivered twice, simulating a retried/duplicate
    // delivery of the same logical message.
    let frame2 = frame1.clone();

    handler_b.handle(frame1).await.expect("first delivery ok");
    handler_b.handle(frame2).await.expect("second delivery ok");

    assert_eq!(received.lock().unwrap().len(), 1, "sink fires exactly once");
    let hist = store_b.history(&a_id).expect("history readable");
    assert_eq!(hist.len(), 1, "exactly one record persisted");
    assert_eq!(hist[0].body, "dup me");
}
