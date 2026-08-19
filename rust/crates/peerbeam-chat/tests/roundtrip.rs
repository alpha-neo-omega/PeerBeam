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

use bytes::Bytes;
use tokio::sync::mpsc::unbounded_channel;

use common::MemTransport;
use peerbeam_appstore_fs::FsAppStore;
use peerbeam_chat::{
    send_message, ChatHandler, ChatMessage, ChatRecord, ChatStore, FileRef, ReceivedSink,
};
use peerbeam_crypto::{derive_subkey, AeadCrypto};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelId, ChannelType, MessageFlags, MessageHandler, MessageType,
};
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

/// One message, then an answer to it: both survive a real session, and the
/// answer still points at what it answered once it is on the other side.
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

    // ── A answers its own first message; B must receive it *as an answer* ──
    //
    // The reply reference travels inside the same `MSG_TEXT` payload, so this
    // is the one test that proves the whole path — `send_reply`, the frame, the
    // receiver's decode, its store, and the render context — rather than any
    // one leg of it.
    let reply = peerbeam_chat::send_reply(
        &a_handle,
        &store_a,
        &b_id,
        "and this answers it",
        Some(&rec.id),
    )
    .await
    .expect("send_reply succeeds");
    assert_eq!(reply.in_reply_to.as_deref(), Some(rec.id.as_str()));

    let mut hist = Vec::new();
    for _ in 0..200 {
        hist = store_b.history(&a_id).expect("store_b history readable");
        if hist.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hist.len(), 2, "B did not receive the reply within 2s");
    assert_eq!(hist[1].in_reply_to.as_deref(), Some(rec.id.as_str()));
    match &peerbeam_chat::resolve_replies(&hist)[1] {
        peerbeam_chat::ReplyContext::Quoting(quoted) => {
            assert_eq!(quoted.id, rec.id);
            assert_eq!(quoted.preview, "hello");
        }
        other => panic!("the reply must resolve against B's own history, got {other:?}"),
    }
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

/// Regression: `send_message` must not race the local `open_channel` return
/// (channel merely `Opening`) against the peer's `ChannelAccept`.
///
/// `SessionHandle::open_channel` resolves as soon as *we* have allocated the
/// channel and queued the open request on the wire — it does not wait for the
/// peer's accept (the channel becomes `Open` only once that arrives). On the
/// in-memory transport used elsewhere in this file that round trip is
/// effectively instantaneous, which hid the bug. Here B's run loop — the side
/// that would send back `ChannelAccept` — is deliberately delayed by 50ms
/// before it starts processing anything, reproducing the nonzero round trip
/// any real transport (QUIC over LAN/WiFi/Tailscale/Internet) has. Against the
/// pre-fix code (`open_channel` → immediately `send_on_channel`) this fails
/// with `SendError::Session("... channel not open")`; the fix must wait for
/// the channel to actually reach `Open` first.
#[tokio::test]
async fn send_message_waits_for_channel_open_when_receiver_run_loop_is_delayed() {
    let store_b = chat_store(6);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

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

    let a_cfg = SessionConfig::new(caps());
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

    let _ = peer_slot_b.set(a_id.clone());

    // A's run loop starts immediately so it can observe the ChannelAccept once
    // B (eventually) sends it.
    tokio::spawn(async move { a.run().await });
    // B's run loop is delayed: the ChannelOpen A sends sits unprocessed on the
    // transport for 50ms first, simulating real-transport latency before the
    // peer's accept comes back — the exact scenario that exposed the race.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        b.run().await
    });

    let store_a = chat_store(7);
    let rec = send_message(&a_handle, &store_a, &b_id, "delayed hello")
        .await
        .expect("send_message must wait for the channel to open, not race it");
    assert_eq!(rec.body, "delayed hello");
    // Only-persist-after-success (Finding 2): the send succeeded, so our own
    // history must show it as Sent.
    let sender_hist = store_a.history(&b_id).expect("store_a history readable");
    assert_eq!(sender_hist.len(), 1);
    assert_eq!(sender_hist[0].status, peerbeam_chat::Status::Sent);

    let mut got: Option<ChatRecord> = None;
    for _ in 0..300 {
        let snapshot = received.lock().unwrap().clone();
        if let Some(first) = snapshot.into_iter().next() {
            got = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let received_rec = got.expect("B did not receive the delayed message in time");
    assert_eq!(received_rec.body, "delayed hello");
    let hist = store_b.history(&a_id).expect("store_b history readable");
    assert_eq!(hist.len(), 1, "store_b persisted exactly one record");
}

/// Flush increment (1b): messages enqueued to the offline outbox while no
/// session was open must go out, in FIFO order, over a session opened later —
/// and the sender's own history must flip from `Pending` to `Sent`.
#[tokio::test]
async fn flush_delivers_queued_messages_and_marks_sent() {
    // ── B: receiver, registers a ChatHandler ────────────────────────────────
    let store_b = chat_store(20);
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
    // loops start dispatching frames.
    let _ = peer_slot_b.set(a_id.clone());

    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // ── Enqueue two messages for B while "offline" (before sending anything
    //    over the session). ────────────────────────────────────────────────
    let store_a = chat_store(21);
    let m1 = ChatMessage::new("first").unwrap();
    let m2 = ChatMessage::new("second").unwrap();
    store_a.enqueue(&b_id, &m1).unwrap();
    store_a.enqueue(&b_id, &m2).unwrap();
    assert_eq!(store_a.outbox_for(&b_id).unwrap().len(), 2);

    // ── Flush over the live session. ────────────────────────────────────────
    let flushed = peerbeam_chat::flush_to_session(&a_handle, &store_a, &b_id)
        .await
        .expect("flush succeeds");
    assert_eq!(flushed.len(), 2);
    assert_eq!(flushed, vec![m1.id.clone(), m2.id.clone()]);

    // Sender: outbox drained, records flipped to Sent.
    assert!(store_a.outbox_for(&b_id).unwrap().is_empty());
    let hist_a = store_a.history(&b_id).unwrap();
    assert_eq!(hist_a.len(), 2);
    assert!(hist_a
        .iter()
        .all(|r| r.status == peerbeam_chat::Status::Sent));

    // Receiver: both delivered + persisted, in order (poll briefly rather than
    // a fixed sleep — real async/network timing).
    let mut hist_b = Vec::new();
    for _ in 0..200 {
        hist_b = store_b.history(&a_id).expect("store_b history readable");
        if hist_b.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hist_b.len(), 2, "both queued messages delivered");
    assert_eq!(hist_b[0].body, "first");
    assert_eq!(hist_b[1].body, "second");
}

/// Re-flush idempotency (1b): the sender may legitimately re-send the same
/// message id twice (e.g. re-queued after a partial flush failure) — the
/// receiver's existing dedup-by-id (1a) must still land exactly one record.
#[tokio::test]
async fn reflush_is_idempotent_on_the_receiver() {
    // ── B: receiver, registers a ChatHandler ────────────────────────────────
    let store_b = chat_store(22);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

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

    let a_cfg = SessionConfig::new(caps());
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

    let _ = peer_slot_b.set(a_id.clone());

    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    let store_a = chat_store(23);
    let m = ChatMessage::new("only once").unwrap();
    store_a.enqueue(&b_id, &m).unwrap();

    let flushed = peerbeam_chat::flush_to_session(&a_handle, &store_a, &b_id)
        .await
        .expect("first flush succeeds");
    assert_eq!(flushed, vec![m.id.clone()]);

    // Poll for the first delivery before re-enqueuing, so the re-flush is a
    // genuine duplicate delivery rather than a race with the first.
    let mut first_delivered = false;
    for _ in 0..200 {
        if !store_b
            .history(&a_id)
            .expect("store_b history readable")
            .is_empty()
        {
            first_delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(first_delivered, "first flush did not deliver in time");

    // Re-enqueue the SAME message id (same id/timestamp/body — a genuine
    // re-send, not a new message) and flush again.
    let dup = ChatMessage {
        id: m.id.clone(),
        timestamp: m.timestamp.clone(),
        body: m.body.clone(),
        in_reply_to: None,
    };
    store_a.enqueue(&b_id, &dup).unwrap();
    let reflushed = peerbeam_chat::flush_to_session(&a_handle, &store_a, &b_id)
        .await
        .expect("second flush succeeds");
    assert_eq!(reflushed, vec![m.id.clone()]);

    // Give the duplicate a moment to be processed (it should be dropped by
    // the receiver's dedup, not added as a second record).
    tokio::time::sleep(Duration::from_millis(100)).await;
    let hist_b = store_b.history(&a_id).expect("store_b history readable");
    assert_eq!(
        hist_b.len(),
        1,
        "receiver keeps exactly one record for the re-flushed id"
    );
    assert_eq!(hist_b[0].body, "only once");
}

/// Regression for the review's follow-up finding: when the peer rejects our
/// channel open, `send_message` must fail *fast* (well under the full poll
/// budget), not spin out the whole timeout.
///
/// B is configured with `.with_channel_limit(0)` — it still negotiates the
/// CHAT capability (so A's local `open_channel` succeeds and registers the
/// channel as `Opening`), but its `ChannelManager::permit` rejects every data
/// channel open outright, so it queues and sends back a `ChannelReject`. On
/// A's side that removes the channel from its own registry (`handle_channel_
/// reject` in `channel_manager.rs`) — the exact "present, then gone" signal
/// `wait_for_channel_open`'s fail-fast path is built to detect.
///
/// B's run loop is delayed 30ms before it starts (same technique as the
/// delayed-open test above). This is load-bearing, not incidental: on the
/// undelayed in-memory transport the entire open→reject round trip completes
/// faster than `wait_for_channel_open`'s very first poll, so the channel is
/// already gone before we ever observe it present — indistinguishable, from
/// inside `send_message`, from registration lag (see `decide`'s `None,
/// seen_before: false` arm), so it would (correctly, per that arm's contract)
/// wait out the full budget instead of failing fast. Confirmed by manual
/// tracing: with no delay, `a_handle.channels()` immediately after
/// `open_channel` already returns `[]`. The delay gives us a window where the
/// channel is genuinely observed `Opening` (so `seen` becomes `true`) before
/// B processes the open and rejects it — reproducing the real-transport case
/// where local registration is visible well before a network round trip
/// completes.
#[tokio::test]
async fn send_message_fails_fast_when_the_peer_rejects_the_channel() {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _a_ev_rx) = unbounded_channel();
    let (b_ev, _b_ev_rx) = unbounded_channel();
    let (a_ch, _a_ch_rx) = unbounded_channel();
    let (b_ch, _b_ch_rx) = unbounded_channel();
    let (a_in, _a_in_rx) = unbounded_channel();
    let (b_in, _b_in_rx) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let b_id = id_b.device_id.clone();

    let a_cfg = SessionConfig::new(caps());
    // Negotiates CHAT fine, but accepts zero concurrent data channels: any
    // open A requests is rejected.
    let b_cfg = SessionConfig::new(caps()).with_channel_limit(0);

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

    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        b.run().await
    });

    let store_a = chat_store(8);
    let started = std::time::Instant::now();
    let result = send_message(&a_handle, &store_a, &b_id, "should be rejected").await;
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "send_message must fail when the peer rejects the channel"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "must fail fast on rejection (well under the 5s budget), took {elapsed:?}"
    );
    // No false Sent record: the send never actually happened.
    assert!(
        store_a
            .history(&b_id)
            .expect("store_a history readable")
            .is_empty(),
        "a rejected send must not persist a Sent record"
    );
}

/// Regression for the FFI review's Finding 1 (PeerBeam increment 1b, task 3
/// re-review): "flush-on-connect" pushes queued messages from the ACCEPT side
/// (Responder, here B — analogous to the FFI's `handle_incoming`) onto the
/// session toward the peer that DIALED IN (Initiator, here A — analogous to a
/// `session_exec::dial`ed session). The bug: every dial in `peerbeam-ffi`
/// registered no `ChatHandler` on its own side (`chat: None`), so a session it
/// dialed could *send* fine but could not *receive* — a message pushed onto it
/// from the far side was silently dropped by the channel dispatch loop (`if
/// let Some(h) = &handler { h.handle(sf) }`, no `else` — see
/// `peerbeam-transfer`'s channel actor), even though the flushing side had
/// already marked it `Sent` and removed it from its own outbox. Net effect:
/// the message vanished, reported delivered, never actually received, never
/// retried.
///
/// This test reproduces the exact push direction the bug was in and proves
/// the fix's shape: A (the dialer) is given a `ChatHandler`, matching what the
/// FFI's fixed `open_send_retry`/`chat_flush_peer` now always wire up (instead
/// of `chat: None`). B has a message queued for A *before* this session
/// exists (the offline-then-connect scenario), then flushes it over the
/// session it (as Responder) accepted. Against the pre-fix shape — drop
/// `.with_handlers(...)` from `a_cfg` below, reproducing a bare dial with no
/// handler — this test fails: the receive-side poll below times out and the
/// `.expect(...)` panics, because the frame is decoded (dispatch loop runs)
/// but never reaches any handler to persist it.
#[tokio::test]
async fn flush_pushes_from_the_accept_side_to_a_handler_equipped_dialer() {
    // ── A: the "dialer" (Initiator) — must have a ChatHandler registered to
    //    receive a message the far side pushes onto this same session. ─────
    let store_a = chat_store(24);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_a, peer_slot_a) = ChatHandler::new(store_a.clone(), sink);

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

    // Dialer (A, Initiator): advertises CHAT AND registers a handler — this
    // is the fix under test (the FFI's pre-fix dial passed `chat: None`).
    let a_cfg = SessionConfig::new(caps()).with_handlers(HandlerRegistry::new().with(handler_a));
    // Accept side (B, Responder): no handler needed here — B only ever
    // flushes OUT in this test, mirroring `handle_incoming`'s flush-on-connect
    // (it never itself receives a chat frame in that codepath).
    let b_cfg = SessionConfig::new(caps());

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
    let b_handle = b.handle();

    // Bind the dialer's peer slot to the accept side's device id before the
    // run loops start dispatching frames.
    let _ = peer_slot_a.set(b_id.clone());

    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // ── B (the accept-analog) has a message queued for A (the dial-analog)
    //    from before this session existed — the flush-on-connect scenario. ──
    let store_b = chat_store(25);
    let msg = ChatMessage::new("pushed on accept").unwrap();
    store_b.enqueue(&a_id, &msg).unwrap();

    // ── B flushes over the session it just accepted, pushing toward A. ──────
    let flushed = peerbeam_chat::flush_to_session(&b_handle, &store_b, &a_id)
        .await
        .expect("flush succeeds");
    assert_eq!(flushed, vec![msg.id.clone()]);

    // B's own bookkeeping: outbox drained, record flipped to Sent — this part
    // passed even in the pre-fix (buggy) code, which is exactly the danger:
    // B reports success while the message never arrives.
    assert!(store_b.outbox_for(&a_id).unwrap().is_empty());
    let hist_b = store_b.history(&a_id).unwrap();
    assert_eq!(hist_b.len(), 1);
    assert_eq!(hist_b[0].status, peerbeam_chat::Status::Sent);

    // ── A (the dialer) must actually receive + persist it — this is the
    //    assertion that only passes with a handler registered on A. ─────────
    let mut got: Option<ChatRecord> = None;
    for _ in 0..200 {
        let snapshot = received.lock().unwrap().clone();
        if let Some(first) = snapshot.into_iter().next() {
            got = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let received_rec = got.expect(
        "A (the dialer) did not receive the pushed message within 2s — this is exactly the \
         FFI bug: a dialed session with no ChatHandler silently drops a pushed CHAT frame",
    );
    assert_eq!(received_rec.body, "pushed on accept");
    assert_eq!(received_rec.peer_id, b_id.0);

    let hist_a = store_a.history(&b_id).expect("store_a history readable");
    assert_eq!(hist_a.len(), 1, "A persisted exactly one record");
    assert_eq!(hist_a[0].body, "pushed on accept");
    assert_eq!(hist_a[0].id, msg.id, "same message id round-trips");
}

/// Regression (MESSAGE_REGISTRY.md §6): an unknown MessageType flagged OPTIONAL
/// must be ignored WITHOUT killing the channel. Pre-fix, `ChatHandler::handle`
/// returned `Err` for any non-TEXT type and the channel actor treats any handler
/// error as fatal — so the unknown frame tore the chat channel down and the text
/// message that followed it on that same channel never arrived.
///
/// Uses `MessageType::new(999)` — a deliberately unassigned id — rather than
/// `2`, since increment 2a gave `MSG_FILE_REF` (2) its own known-type dispatch
/// arm; using it here would no longer exercise the unknown-type fallback.
#[tokio::test]
async fn unknown_optional_message_does_not_kill_the_chat_channel() {
    let store_b = chat_store(2);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

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

    let a_cfg = SessionConfig::new(caps());
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
    let _ = peer_slot_b.set(a_id.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // Open ONE chat channel and wait for the peer to accept it.
    let channel = a_handle
        .open_channel(ChannelType::CHAT)
        .await
        .expect("open chat channel");
    let mut opened = false;
    for _ in 0..500 {
        let chans = a_handle.channels().await.expect("channels");
        if chans.iter().any(|c| c.id == channel && c.state.is_open()) {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(opened, "chat channel never opened");

    // 1. An additive future message type this build does not implement,
    //    correctly flagged OPTIONAL by its sender.
    a_handle
        .send_on_channel(
            channel,
            MessageType::new(999),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{\"whatever\":true}"),
        )
        .await
        .expect("send unknown optional frame");

    // 2. A perfectly ordinary text message on the SAME channel, after it.
    let msg = ChatMessage::new("still here").expect("mint");
    let frame = msg.to_frame(channel).expect("encode");
    a_handle
        .send_on_channel(
            channel,
            ChatMessage::message_type(),
            frame.flags,
            frame.payload,
        )
        .await
        .expect("send text frame");

    // The text must arrive: the unknown frame was skipped, not fatal.
    let mut got: Option<ChatRecord> = None;
    for _ in 0..300 {
        if let Some(first) = received.lock().unwrap().first().cloned() {
            got = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let rec = got
        .expect("text after an unknown OPTIONAL frame never arrived — the channel was torn down");
    assert_eq!(rec.body, "still here");
    assert_eq!(
        store_b.history(&a_id).expect("history").len(),
        1,
        "exactly one record: the unknown frame persisted nothing"
    );
}

/// Increment 2a: a real two-session `FileRef` delivery. A opens a CHAT
/// channel, waits for it to be accepted, then sends a `FileRef` frame (not a
/// text message) directly on it. B's `ChatHandler` must dispatch `MSG_FILE_REF`
/// as a known type — the increment-0 unknown-OPTIONAL fallback would
/// otherwise silently swallow it, since `FileRef::to_frame` always sets
/// OPTIONAL — and persist an `In`/`File`/`PendingApproval` row keyed by the
/// FileRef's own id (which doubles as the transfer id).
#[tokio::test]
async fn b_persists_a_file_ref_pushed_over_a_real_session() {
    let store_b = chat_store(30);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

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

    let a_cfg = SessionConfig::new(caps());
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
    let _ = peer_slot_b.set(a_id.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // Open ONE chat channel and wait for the peer to accept it.
    let channel = a_handle
        .open_channel(ChannelType::CHAT)
        .await
        .expect("open chat channel");
    let mut opened = false;
    for _ in 0..500 {
        let chans = a_handle.channels().await.expect("channels");
        if chans.iter().any(|c| c.id == channel && c.state.is_open()) {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(opened, "chat channel never opened");

    // Send a FileRef (not a text message) on the open channel.
    let r = FileRef::new("report.pdf", 4096).unwrap();
    let frame = r.to_frame(channel).unwrap();
    a_handle
        .send_on_channel(channel, FileRef::message_type(), frame.flags, frame.payload)
        .await
        .expect("send file ref");

    // B persists it as a PendingApproval File row, keyed by the id that will
    // also be the transfer id.
    let mut hist = Vec::new();
    for _ in 0..200 {
        hist = store_b.history(&a_id).expect("store_b history readable");
        if !hist.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hist.len(), 1, "B persisted exactly one record");
    assert_eq!(hist[0].kind, peerbeam_chat::Kind::File);
    assert_eq!(hist[0].status, peerbeam_chat::Status::PendingApproval);
    assert_eq!(hist[0].id, r.id, "the record key IS the transfer id");
    let meta = hist[0].file.clone().expect("file meta present");
    assert_eq!(meta.name, "report.pdf");
    assert_eq!(meta.size, 4096);
    assert!(meta.local_path.is_none());

    assert_eq!(received.lock().unwrap().len(), 1, "sink fired once");
}

/// **The drain is kind-aware** (increment 2b). Before this, `flush_to_session`
/// rebuilt *every* queued entry as a `ChatMessage` and sent it as `MSG_TEXT`,
/// with no branch on `entry.kind`. That made two things wrong at once:
///
/// * a queued **file** would have gone out as an empty text message while its
///   bytes stayed on disk;
/// * a queued **decline** — the message that makes a refusal terminal for the
///   sender, queued because the sender dropped while our approval prompt was
///   open — would have arrived as an empty text message and settled nothing,
///   so the sender would keep re-offering the file it had already been refused.
///
/// So: A queues a text message, a decline, and a file, all for B, and flushes.
/// The text and the decline go out on CHAT (the decline as a real
/// `FileDecline`, settling B's own outgoing row); the file is left queued for
/// the caller's transfer engine to start.
#[tokio::test]
async fn a_queued_decline_flushes_as_a_decline_and_a_queued_file_is_left_for_the_transfer() {
    // B is the side that offered a file and is waiting to hear about it.
    let store_b = chat_store(31);
    let received: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cl = received.clone();
    let sink: ReceivedSink = Arc::new(move |rec| received_cl.lock().unwrap().push(rec));
    let (handler_b, peer_slot_b) = ChatHandler::new(store_b.clone(), sink);

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

    let a_cfg = SessionConfig::new(caps());
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
    let _ = peer_slot_b.set(a_id.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // B's side of the story: it offered A a file, and its row is in flight.
    let offered = FileRef::new("holiday.mov", 4096).unwrap();
    store_b
        .append(&peerbeam_chat::ChatRecord::file_out(
            &a_id,
            &offered,
            peerbeam_chat::FileMeta::new(&offered.name, offered.size, None),
            peerbeam_chat::Status::Transferring,
        ))
        .unwrap();

    // A's outbox: a text message, the decline for B's offer, and a file of its
    // own that must NOT go out on the chat channel.
    let store_a = chat_store(32);
    let text = ChatMessage::new("no thanks, too big").unwrap();
    store_a.enqueue(&b_id, &text).unwrap();
    let decline = peerbeam_chat::FileDecline::new(&offered.id);
    assert!(store_a.enqueue_decline(&b_id, &decline).unwrap());
    let mine = FileRef::new("mine.bin", 3).unwrap();
    store_a
        .append(&peerbeam_chat::ChatRecord::file_out(
            &b_id,
            &mine,
            peerbeam_chat::FileMeta::new(&mine.name, mine.size, None),
            peerbeam_chat::Status::Staging,
        ))
        .unwrap();
    // Real bytes on disk, in a leaked temp dir like `chat_store`'s: an entry
    // whose staged blob has gone is deliberately skipped by `next_file_for`, so
    // a fixture pointing at a path that never existed would be exercising that
    // skip instead of the hand-over this test is about.
    let blob = tempfile::tempdir().unwrap().keep().join(&mine.id);
    std::fs::write(&blob, b"abc").unwrap();
    assert!(
        store_a
            .enqueue_file(
                &b_id,
                &mine,
                &peerbeam_chat::StagedFile {
                    name: "mine.bin".into(),
                    size: 3,
                    staged_path: blob.to_string_lossy().into_owned(),
                },
            )
            .unwrap(),
        "the row seeded above is there, so it queues"
    );
    assert_eq!(store_a.outbox_for(&b_id).unwrap().len(), 3);

    let flushed = peerbeam_chat::flush_to_session(&a_handle, &store_a, &b_id)
        .await
        .expect("flush succeeds");

    // Only the text is reported delivered. The decline's id names B's file —
    // reporting it would tell A's surface that a file A REFUSED was "sent".
    assert_eq!(
        flushed,
        vec![text.id.clone()],
        "a decline is delivered but never reported as a sent message"
    );

    // The decline really landed as a decline: B's own outgoing row is settled.
    let mut settled = None;
    for _ in 0..300 {
        let rec = store_b.get(&a_id, &offered.id).unwrap().expect("B's row");
        if rec.status != peerbeam_chat::Status::Transferring {
            settled = Some(rec.status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        settled,
        Some(peerbeam_chat::Status::Declined),
        "a queued decline must arrive as a FileDecline and settle the sender's row; \
         sent as text it would have settled nothing"
    );
    // …and it did not arrive as a chat message. The text did, so the sink
    // firing exactly once — for the text — is what says the decline was
    // dispatched as a decline rather than delivered as an empty message.
    let seen = received.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "a decline is a status change, never a new message in the thread: {seen:?}"
    );
    assert_eq!(seen[0].body, "no thanks, too big");
    let hist_b = store_b.history(&a_id).unwrap();
    assert_eq!(
        hist_b.len(),
        2,
        "B's own file row plus the one text message: {hist_b:?}"
    );
    assert_eq!(
        hist_b
            .iter()
            .filter(|r| r.kind == peerbeam_chat::Kind::File)
            .count(),
        1,
        "the decline settled B's existing file row rather than adding one"
    );

    // A's outbox: text and decline drained, the file still queued for the
    // caller's transfer engine — which `next_file_for` is what hands over.
    let left = store_a.outbox_for(&b_id).unwrap();
    assert_eq!(left.len(), 1, "only the file entry remains: {left:?}");
    assert_eq!(left[0].message_id, mine.id);
    assert_eq!(left[0].kind, peerbeam_chat::Kind::File);
    assert_eq!(
        peerbeam_chat::next_file_for(&store_a, &b_id)
            .unwrap()
            .expect("the file is what the caller must send")
            .entry
            .message_id,
        mine.id
    );
}
