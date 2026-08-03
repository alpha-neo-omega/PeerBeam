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
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelId, ChannelState, ChannelType, MessageFlags, MessageHandler,
    MessageType, SessionError, SessionFrame, Version,
};
use peerbeam_transfer::{
    ChannelEvent, HandlerRegistry, Identity, PeerSession, SessionConfig, SessionEvent,
    SessionHandle, SessionRole,
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
}

async fn open(a_cfg: SessionConfig, b_cfg: SessionConfig) -> Pair {
    let (ta, tb) = MemTransport::pair();
    let (a_ev_tx, a_events) = unbounded_channel();
    let (b_ev_tx, b_events) = unbounded_channel();
    let (a_ch_tx, a_channels) = unbounded_channel();
    let (b_ch_tx, b_channels) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        a_cfg,
        a_ev_tx,
        a_ch_tx,
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
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        SessionConfig::new(caps()),
        a_ev,
        a_ch,
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

#[tokio::test]
async fn incompatible_major_versions_are_rejected() {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _) = unbounded_channel();
    let (b_ev, _) = unbounded_channel();
    let (a_ch, _) = unbounded_channel();
    let (b_ch, _) = unbounded_channel();
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
