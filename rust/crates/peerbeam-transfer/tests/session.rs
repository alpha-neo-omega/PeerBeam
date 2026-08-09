//! Integration tests for the multiplexed PeerSession (M3): two endpoints over an
//! in-memory `ChannelTransport`, exercising establishment/negotiation (M1–M2
//! regression) plus channel open/accept/reject/close, independent ordering,
//! isolation, cleanup, and concurrency.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use common::MemTransport;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::Progress;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelId, ChannelState, ChannelType, MessageFlags, MessageHandler,
    MessageType, SessionError, SessionFrame, Version,
};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file_on_channel, send_file_on_session, ChannelEvent, HandlerRegistry, Identity,
    IncomingStreamChannel, KeepaliveConfig, PeerSession, SendRequest, SessionConfig, SessionEvent,
    SessionHandle, SessionRole, TransferControl, TransferOutcome,
};
use peerbeam_trust_fs::FsTrust;

/// Fresh identity, encryption provider, and (leaked-temp-dir) trust store for a
/// test endpoint. Each side authenticates with its own keypair + empty trust
/// store (TOFU pins the peer on first contact).
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

const T1: ChannelType = ChannelType::TRANSFER; // 0x0100
fn t2() -> ChannelType {
    ChannelType::new(0x0101)
}

/// Records every frame a channel delivers, keyed by channel id, for ordering
/// assertions.
type Log = Arc<Mutex<Vec<(ChannelId, Vec<u8>)>>>;

struct Recorder {
    channel_type: ChannelType,
    log: Log,
}

#[async_trait]
impl MessageHandler for Recorder {
    fn channel_type(&self) -> ChannelType {
        self.channel_type
    }
    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        self.log
            .lock()
            .expect("log")
            .push((frame.channel, frame.payload.to_vec()));
        Ok(())
    }
}

fn caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::new(T1))
        .with(Capability::new(t2()))
}

fn recording_handlers(log: &Log) -> HandlerRegistry {
    HandlerRegistry::new()
        .with(Arc::new(Recorder {
            channel_type: T1,
            log: log.clone(),
        }))
        .with(Arc::new(Recorder {
            channel_type: t2(),
            log: log.clone(),
        }))
}

/// A running, negotiated session pair with handles and event receivers.
struct Pair {
    a: SessionHandle,
    b: SessionHandle,
    a_events: UnboundedReceiver<SessionEvent>,
    b_events: UnboundedReceiver<SessionEvent>,
    a_channels: UnboundedReceiver<ChannelEvent>,
    b_channels: UnboundedReceiver<ChannelEvent>,
    a_incoming: UnboundedReceiver<IncomingStreamChannel>,
    b_incoming: UnboundedReceiver<IncomingStreamChannel>,
}

async fn open(a_cfg: SessionConfig, b_cfg: SessionConfig) -> Pair {
    let (ta, tb) = MemTransport::pair();
    let (a_ev_tx, a_events) = unbounded_channel();
    let (b_ev_tx, b_events) = unbounded_channel();
    let (a_ch_tx, a_channels) = unbounded_channel();
    let (b_ch_tx, b_channels) = unbounded_channel();
    let (a_in_tx, a_incoming) = unbounded_channel();
    let (b_in_tx, b_incoming) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        a_cfg,
        a_ev_tx,
        a_ch_tx,
        a_in_tx,
        None,
        id_a,
        enc_a,
        trust_a,
    );
    let fb = PeerSession::open(
        tb,
        SessionRole::Responder,
        b_cfg,
        b_ev_tx,
        b_ch_tx,
        b_in_tx,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator opens");
    let mut b = rb.expect("responder opens");
    let a_handle = a.handle();
    let b_handle = b.handle();
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    Pair {
        a: a_handle,
        b: b_handle,
        a_events,
        b_events,
        a_channels,
        b_channels,
        a_incoming,
        b_incoming,
    }
}

async fn next_channel_event(
    rx: &mut UnboundedReceiver<ChannelEvent>,
    pred: impl Fn(&ChannelEvent) -> bool,
) -> ChannelEvent {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return ev,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("channel event stream ended"),
            Err(_) => panic!("timed out waiting for a channel event"),
        }
    }
}

async fn wait_until(mut pred: impl FnMut() -> bool) {
    for _ in 0..300 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached in time");
}

async fn wait_channels_len(handle: &SessionHandle, expected: usize) {
    for _ in 0..300 {
        if let Ok(channels) = handle.channels().await {
            if channels.len() == expected {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("channel count did not reach {expected}");
}

#[tokio::test]
async fn regression_establish_and_negotiate() {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, mut a_rx) = unbounded_channel();
    let (b_ev, _b_rx) = unbounded_channel();
    let (a_ch, _a_chr) = unbounded_channel();
    let (b_ch, _b_chr) = unbounded_channel();
    let (a_in, _a_inr) = unbounded_channel();
    let (b_in, _b_inr) = unbounded_channel();
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        SessionConfig::new(caps()),
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
        SessionConfig::new(caps()),
        b_ev,
        b_ch,
        b_in,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let a = ra.expect("a");
    let b = rb.expect("b");
    assert!(a.state().is_active() && b.state().is_active());
    assert_eq!(a.id(), b.id());
    assert!(a.capabilities().supports(ChannelType::CONTROL));
    assert!(a.capabilities().supports(T1) && a.capabilities().supports(t2()));
    assert!(matches!(
        a_rx.try_recv(),
        Ok(SessionEvent::Established { .. })
    ));
}

/// Regression guard for the CLI-facing plumbing: `pairing_code` is computed
/// one layer below (`auth::authenticate`) and copied onto `PeerSession` by
/// hand in two places — `open()`'s post-`assemble` assignment here, and the
/// CLI's `session_transfer::Session` construction (`bins/peerbeam-cli/src/
/// session_transfer.rs`). Neither copy is enforced by the type system, so a
/// future edit dropping either one would compile clean and silently regress to
/// an always-empty code reaching the CLI. Exercises a real `PeerSession::open`
/// dial/accept round trip (mirrors `regression_establish_and_negotiate` above)
/// one layer above the existing `handshake_produces_matching_pairing_codes`
/// test in `tests/secure.rs` (which only covers `authenticate()` directly).
#[tokio::test]
async fn pairing_code_survives_peer_session_handshake() {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _) = unbounded_channel();
    let (b_ev, _) = unbounded_channel();
    let (a_ch, _) = unbounded_channel();
    let (b_ch, _) = unbounded_channel();
    let (a_in, _) = unbounded_channel();
    let (b_in, _) = unbounded_channel();
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        SessionConfig::new(caps()),
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
        SessionConfig::new(caps()),
        b_ev,
        b_ch,
        b_in,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let a = ra.expect("a");
    let b = rb.expect("b");
    assert_eq!(a.pairing_code().len(), 39);
    assert!(!a.pairing_code().is_empty());
    assert_eq!(a.pairing_code(), b.pairing_code());
}

#[tokio::test]
async fn incompatible_major_versions_are_rejected() {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _) = unbounded_channel();
    let (b_ev, _) = unbounded_channel();
    let (a_ch, _) = unbounded_channel();
    let (b_ch, _) = unbounded_channel();
    let (a_in, _) = unbounded_channel();
    let (b_in, _) = unbounded_channel();
    let mut a_cfg = SessionConfig::new(caps());
    a_cfg.version = Version::new(2, 0);
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
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
        SessionConfig::new(caps()),
        b_ev,
        b_ch,
        b_in,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    assert!(matches!(ra, Err(SessionError::VersionIncompatible { .. })));
    assert!(matches!(rb, Err(SessionError::VersionIncompatible { .. })));
}

#[tokio::test]
async fn opens_multiple_independent_channels() {
    let mut p = open(SessionConfig::new(caps()), SessionConfig::new(caps())).await;
    let c1 = p.a.open_channel(T1).await.expect("open t1");
    let c2 = p.a.open_channel(t2()).await.expect("open t2");
    assert_ne!(c1, c2);

    // Both endpoints observe both channels open.
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c2),
    )
    .await;
    next_channel_event(&mut p.b_channels, |e| {
        matches!(e, ChannelEvent::Opened { .. })
    })
    .await;

    let a_snapshot = p.a.channels().await.expect("snapshot");
    assert_eq!(a_snapshot.len(), 2);
    assert!(a_snapshot.iter().all(|c| c.state == ChannelState::Open));
    wait_channels_len(&p.b, 2).await;
}

#[tokio::test]
async fn frames_are_ordered_per_channel_and_isolated() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut p = open(
        SessionConfig::new(caps()),
        SessionConfig::new(caps()).with_handlers(recording_handlers(&log)),
    )
    .await;
    let c1 = p.a.open_channel(T1).await.expect("t1");
    let c2 = p.a.open_channel(t2()).await.expect("t2");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c2),
    )
    .await;

    for i in 0..5u8 {
        p.a.send_on_channel(
            c1,
            MessageType::new(1),
            MessageFlags::NONE,
            Bytes::from(vec![i]),
        )
        .await
        .expect("send c1");
        p.a.send_on_channel(
            c2,
            MessageType::new(1),
            MessageFlags::NONE,
            Bytes::from(vec![100 + i]),
        )
        .await
        .expect("send c2");
    }

    wait_until({
        let log = log.clone();
        move || log.lock().expect("log").len() == 10
    })
    .await;

    let recorded = log.lock().expect("log").clone();
    let c1_bytes: Vec<u8> = recorded
        .iter()
        .filter(|(ch, _)| *ch == c1)
        .flat_map(|(_, p)| p.clone())
        .collect();
    let c2_bytes: Vec<u8> = recorded
        .iter()
        .filter(|(ch, _)| *ch == c2)
        .flat_map(|(_, p)| p.clone())
        .collect();
    assert_eq!(c1_bytes, vec![0, 1, 2, 3, 4], "channel 1 preserves order");
    assert_eq!(
        c2_bytes,
        vec![100, 101, 102, 103, 104],
        "channel 2 preserves order, isolated from channel 1"
    );
}

#[tokio::test]
async fn closing_one_channel_leaves_others_open() {
    let mut p = open(SessionConfig::new(caps()), SessionConfig::new(caps())).await;
    let c1 = p.a.open_channel(T1).await.expect("t1");
    let c2 = p.a.open_channel(t2()).await.expect("t2");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c2),
    )
    .await;

    p.a.close_channel(c1);
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Closed { channel } if *channel == c1),
    )
    .await;

    wait_channels_len(&p.a, 1).await;
    let snap = p.a.channels().await.expect("snap");
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, c2, "the other channel stays open");
}

#[tokio::test]
async fn channel_beyond_limit_is_rejected() {
    // The responder allows only one channel; the initiator's second open is
    // rejected by the peer.
    let mut p = open(
        SessionConfig::new(caps()),
        SessionConfig::new(caps()).with_channel_limit(1),
    )
    .await;
    let c1 = p.a.open_channel(T1).await.expect("first");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;

    let c2 = p.a.open_channel(t2()).await.expect("second requested");
    let ev = next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Rejected { channel, .. } if *channel == c2),
    )
    .await;
    assert!(matches!(ev, ChannelEvent::Rejected { .. }));
}

#[tokio::test]
async fn reject_does_not_desync_later_channel_pairing() {
    // RM4 (#6/#7): a rejected ChannelOpen must still consume the stream the peer
    // opened for it. Otherwise the orphaned stream desyncs FIFO open<->stream
    // pairing and a later channel binds to the wrong stream (wrong per-channel
    // key → spurious error), corrupting an unrelated channel.
    let mut p = open(
        SessionConfig::new(caps()),
        SessionConfig::new(caps()).with_channel_limit(1),
    )
    .await;

    // c1 opens and fills the responder's single slot.
    let c1 = p.a.open_channel(T1).await.expect("c1");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;

    // c2 is rejected on the limit; the peer already opened a stream for it — the
    // orphan that (pre-fix) would offset every later pairing.
    let c2 = p.a.open_channel(t2()).await.expect("c2 requested");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Rejected { channel, .. } if *channel == c2),
    )
    .await;

    // Free the slot (control stream is ordered: the close is processed before the
    // next open), then open c3. With the fix c3 pairs with its own stream and
    // opens; without it, c3 inherits c2's orphan stream and errors.
    p.a.close_channel(c1);
    wait_channels_len(&p.a, 0).await;

    let c3 = p.a.open_channel(T1).await.expect("c3 requested");
    let ev = next_channel_event(&mut p.a_channels, |e| {
        matches!(
            e,
            ChannelEvent::Opened { channel, .. }
                | ChannelEvent::Rejected { channel, .. }
                | ChannelEvent::Error { channel, .. }
            if *channel == c3
        )
    })
    .await;
    assert!(
        matches!(ev, ChannelEvent::Opened { .. }),
        "c3 must open cleanly, not inherit the rejected channel's stream: {ev:?}"
    );
    wait_channels_len(&p.a, 1).await;
}

#[tokio::test]
async fn idle_session_sends_automatic_keepalive_pings() {
    // The pump drives the keepalive scheduler: after `interval` of no activity it
    // auto-sends a Ping (previously the scheduler was never consulted, so the
    // idle-timeout/keepalive config was inert and a stalled peer hung forever).
    let mut fast = SessionConfig::new(caps());
    fast.keepalive = KeepaliveConfig {
        interval: Duration::from_millis(50),
        idle_timeout: Duration::from_secs(30),
    };
    let mut p = open(fast, SessionConfig::new(caps())).await;

    // With A idle, its pump auto-pings within a few intervals; B reports it.
    loop {
        match tokio::time::timeout(Duration::from_secs(3), p.b_events.recv()).await {
            Ok(Some(SessionEvent::PingReceived { .. })) => break,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("event stream ended before a keepalive ping"),
            Err(_) => panic!("no automatic keepalive ping arrived within the timeout"),
        }
    }
    p.a.close();
}

#[tokio::test]
async fn ping_is_answered_and_reported() {
    let mut p = open(SessionConfig::new(caps()), SessionConfig::new(caps())).await;
    p.a.ping();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), p.b_events.recv()).await {
            Ok(Some(SessionEvent::PingReceived { .. })) => break,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("event stream ended"),
            Err(_) => panic!("timed out waiting for PingReceived"),
        }
    }
}

#[tokio::test]
async fn stress_many_channels_open_and_clean_up() {
    let mut p = open(SessionConfig::new(caps()), SessionConfig::new(caps())).await;
    let mut ids = Vec::new();
    for _ in 0..24 {
        ids.push(p.a.open_channel(T1).await.expect("open"));
    }
    // Wait for all to be acknowledged open on the initiator.
    let mut opened = 0;
    while opened < ids.len() {
        next_channel_event(&mut p.a_channels, |e| {
            matches!(e, ChannelEvent::Opened { .. })
        })
        .await;
        opened += 1;
    }
    let snap = p.a.channels().await.expect("snap");
    assert_eq!(snap.len(), 24);

    // Closing the session tears everything down.
    p.a.close();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), p.a_events.recv()).await {
            Ok(Some(SessionEvent::Closed { .. })) => break,
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => panic!("timed out waiting for session close"),
        }
    }
    // After close the handle can no longer query channels.
    assert!(
        p.a.channels().await.is_err() || p.a.channels().await.map(|c| c.is_empty()).unwrap_or(true)
    );
}

// ── M5: file transfer as a PeerSession channel ──────────────────────────────
//
// These exercise the full PeerSession stack — real authenticated handshake
// (AeadCrypto + FsTrust), real per-channel key derivation, and the real,
// unmodified transfer engine (`send_file`/`receive_file`) — over an in-memory
// `ChannelTransport`. Only the socket is simulated; the crypto and transfer
// logic are the production code paths.

/// A config that advertises TRANSFER as a stream capability (opt-in to
/// session transfer) plus the message caps used elsewhere in this file.
fn transfer_cfg() -> SessionConfig {
    SessionConfig::new(caps()).with_stream_channel_type(T1)
}

/// Write `data` to a fresh file under `dir` and return its absolute path.
fn write_src(dir: &std::path::Path, name: &str, data: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, data).expect("write source");
    path.to_string_lossy().into_owned()
}

fn send_req(id: &str, name: &str, path: String, size: u64, chunk: u32) -> SendRequest {
    SendRequest {
        transfer_id: id.to_string(),
        name: name.to_string(),
        path,
        size,
        chunk_size: chunk,
    }
}

/// One file sent over its own transfer channel arrives byte-for-byte.
#[tokio::test]
async fn transfer_over_session_single_file_is_byte_for_byte() {
    let mut p = open(transfer_cfg(), transfer_cfg()).await;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..250_000u32).map(|i| (i % 251) as u8).collect();
    let path = write_src(src.path(), "movie.bin", &data);

    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = unbounded_channel::<Progress>();
    let (ptx2, _prx2) = unbounded_channel::<Progress>();

    let dst_dir = dst.path().to_string_lossy().into_owned();
    let ha = &p.a;
    let hb = &p.b;
    let bin = &mut p.b_incoming;

    let send_fut = send_file_on_session(
        ha,
        &storage_s,
        send_req("t1", "movie.bin", path, data.len() as u64, 8192),
        &ctrl_s,
        &ptx,
        0,
    );
    let recv_fut = async {
        let inc = bin.recv().await.expect("incoming transfer channel");
        assert_eq!(inc.channel_type, T1);
        receive_file_on_channel(inc, hb, &storage_r, &dst_dir, &ctrl_r, &ptx2).await
    };
    let (send_res, recv_res) = tokio::join!(send_fut, recv_fut);

    assert_eq!(send_res.expect("send ok"), TransferOutcome::Completed);
    let received = recv_res.expect("receive ok");
    assert_eq!(received.outcome, TransferOutcome::Completed);
    assert_eq!(received.bytes, data.len() as u64);
    let got = std::fs::read(dst.path().join("movie.bin")).unwrap();
    assert_eq!(got, data, "received bytes differ from source");

    // The opener never receives an incoming-stream notification for a channel
    // it opened itself — that path is the accepter's only.
    assert!(p.a_incoming.try_recv().is_err());
}

/// Two transfers run concurrently on independent channels; both complete and
/// each arrives byte-for-byte — the channels do not cross-contaminate.
#[tokio::test]
async fn concurrent_transfers_use_independent_channels() {
    let mut p = open(transfer_cfg(), transfer_cfg()).await;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data_a: Vec<u8> = (0..180_000u32).map(|i| (i % 97) as u8).collect();
    let data_b: Vec<u8> = (0..90_000u32).map(|i| (i % 131 + 3) as u8).collect();
    let path_a = write_src(src.path(), "a.bin", &data_a);
    let path_b = write_src(src.path(), "b.bin", &data_b);

    let ss = FsStorage::new();
    let sr = FsStorage::new();
    let (cs1, cs2, cr1, cr2) = (
        TransferControl::new(),
        TransferControl::new(),
        TransferControl::new(),
        TransferControl::new(),
    );
    let (pt, _pr) = unbounded_channel::<Progress>();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let ha = &p.a;
    let hb = &p.b;
    let bin = &mut p.b_incoming;

    let senders = async {
        tokio::join!(
            send_file_on_session(
                ha,
                &ss,
                send_req("ta", "a.bin", path_a, data_a.len() as u64, 4096),
                &cs1,
                &pt,
                0,
            ),
            send_file_on_session(
                ha,
                &ss,
                send_req("tb", "b.bin", path_b, data_b.len() as u64, 4096),
                &cs2,
                &pt,
                0,
            ),
        )
    };
    let receivers = async {
        // Both channels pair before either transfer body runs.
        let i1 = bin.recv().await.expect("incoming 1");
        let i2 = bin.recv().await.expect("incoming 2");
        tokio::join!(
            receive_file_on_channel(i1, hb, &sr, &dst_dir, &cr1, &pt),
            receive_file_on_channel(i2, hb, &sr, &dst_dir, &cr2, &pt),
        )
    };
    let (send_res, recv_res) = tokio::join!(senders, receivers);

    assert_eq!(send_res.0.expect("send a"), TransferOutcome::Completed);
    assert_eq!(send_res.1.expect("send b"), TransferOutcome::Completed);
    assert_eq!(
        recv_res.0.expect("recv 1").outcome,
        TransferOutcome::Completed
    );
    assert_eq!(
        recv_res.1.expect("recv 2").outcome,
        TransferOutcome::Completed
    );
    assert_eq!(std::fs::read(dst.path().join("a.bin")).unwrap(), data_a);
    assert_eq!(std::fs::read(dst.path().join("b.bin")).unwrap(), data_b);
}

/// Cancelling a transfer ends it as `Cancelled` and leaves the PeerSession
/// fully usable — a transfer failure is isolated to its own channel.
#[tokio::test]
async fn cancelled_transfer_does_not_terminate_session() {
    let mut p = open(transfer_cfg(), transfer_cfg()).await;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = vec![9u8; 2_000_000];
    let path = write_src(src.path(), "big.bin", &data);

    let ss = FsStorage::new();
    let sr = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (pt, _pr) = unbounded_channel::<Progress>();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let ha = &p.a;
    let hb = &p.b;
    let bin = &mut p.b_incoming;

    let send_fut = send_file_on_session(
        ha,
        &ss,
        send_req("tc", "big.bin", path, data.len() as u64, 4096),
        &ctrl_s,
        &pt,
        0,
    );
    let recv_fut = async {
        let inc = bin.recv().await.expect("incoming channel");
        // Cancel before the receiver acks, so the transfer aborts early.
        ctrl_s.cancel();
        receive_file_on_channel(inc, hb, &sr, &dst_dir, &ctrl_r, &pt).await
    };
    let (send_res, recv_res) = tokio::join!(send_fut, recv_fut);

    assert_eq!(send_res.expect("send resolves"), TransferOutcome::Cancelled);
    assert_eq!(
        recv_res.expect("receive resolves").outcome,
        TransferOutcome::Cancelled
    );

    // The session survived the cancelled transfer: control still works and a
    // brand-new channel can be opened.
    p.a.ping();
    let _ = p.a.channels().await.expect("session still serves control");
    let c = p.a.open_channel(t2()).await.expect("session still usable");
    next_channel_event(
        &mut p.a_channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c),
    )
    .await;
}
