//! Real-QUIC reconnect + resume (M6): two endpoints establish an authenticated
//! `PeerSession` over two real QUIC endpoints, the connection is closed underneath
//! them, and a `RecoveryManager` re-dials a fresh QUIC connection and resumes the
//! session with a single-use token — no repeated handshake, fresh crypto epoch,
//! channels re-attached. This exercises the whole M6 path over the network stack,
//! not an in-memory transport.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, SessionError};
use peerbeam_transfer::{
    ChannelEvent, Identity, PeerSession, RecoveryConfig, RecoveryManager, SessionConfig,
    SessionEvent, SessionHandle, SessionRole, SessionWiring, TransportFactory,
};
use peerbeam_transfer_quic::{direct_route, QuicChannels, QuicTransport};
use peerbeam_trust_fs::FsTrust;
use serial_test::serial;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::watch;

const CHAT: ChannelType = ChannelType::new(0x0101);

fn caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::new(CHAT))
}

fn dial_session() -> TransferSession {
    TransferSession {
        id: TransferId::from("s1"),
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

fn security(name: &str) -> (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>) {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: DeviceId::from(name),
        name: name.to_string(),
        keypair,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let trust = FsTrust::open(dir.path().join("trust.json")).expect("trust");
    std::mem::forget(dir);
    (identity, Arc::new(enc), Arc::new(trust))
}

type Conns = Arc<Mutex<Vec<Arc<dyn ChannelTransport>>>>;

/// Redialling factory: opens a fresh QUIC connection to the server each time and
/// remembers it so a test can close it to sever the link.
struct QuicDial {
    client: QuicTransport,
    route: peerbeam_domain::entity::Route,
    conns: Conns,
}

#[async_trait]
impl peerbeam_transfer::TransportFactory for QuicDial {
    async fn connect(&mut self) -> Result<Arc<dyn ChannelTransport>, SessionError> {
        let qc = self
            .client
            .dial_channels(&self.route, &dial_session())
            .await
            .map_err(|e| SessionError::Link(e.to_string()))?;
        let t: Arc<dyn ChannelTransport> = Arc::new(qc);
        self.conns.lock().expect("conns").push(t.clone());
        Ok(t)
    }
}

/// Accepting factory: yields the next inbound QUIC connection from the listener.
struct QuicAccept {
    incoming: BoxStream<'static, peerbeam_domain::error::Result<QuicChannels>>,
}

#[async_trait]
impl peerbeam_transfer::TransportFactory for QuicAccept {
    async fn connect(&mut self) -> Result<Arc<dyn ChannelTransport>, SessionError> {
        match self.incoming.next().await {
            Some(Ok(qc)) => Ok(Arc::new(qc) as Arc<dyn ChannelTransport>),
            Some(Err(e)) => Err(SessionError::Link(e.to_string())),
            None => Err(SessionError::Link("listener closed".into())),
        }
    }
}

fn wiring() -> (
    SessionWiring,
    UnboundedReceiver<SessionEvent>,
    UnboundedReceiver<ChannelEvent>,
) {
    let (ev, ev_rx) = unbounded_channel();
    let (ch, ch_rx) = unbounded_channel();
    let (in_tx, _in_rx) = unbounded_channel();
    // Leak the incoming-stream receiver: this test drives message channels, not
    // stream channels.
    std::mem::forget(_in_rx);
    (
        SessionWiring {
            events: ev,
            channel_events: ch,
            incoming_streams: in_tx,
            registry: None,
        },
        ev_rx,
        ch_rx,
    )
}

async fn next_event(
    rx: &mut UnboundedReceiver<SessionEvent>,
    pred: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    loop {
        match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return ev,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("event stream ended"),
            Err(_) => panic!("timed out waiting for a session event"),
        }
    }
}

async fn next_channel(
    rx: &mut UnboundedReceiver<ChannelEvent>,
    pred: impl Fn(&ChannelEvent) -> bool,
) -> ChannelEvent {
    loop {
        match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return ev,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("channel event stream ended"),
            Err(_) => panic!("timed out waiting for a channel event"),
        }
    }
}

async fn live_handle(rx: &mut watch::Receiver<Option<SessionHandle>>) -> SessionHandle {
    loop {
        if let Some(h) = rx.borrow_and_update().clone() {
            return h;
        }
        tokio::time::timeout(Duration::from_secs(15), rx.changed())
            .await
            .expect("handle watch timed out")
            .expect("handle watch closed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn real_quic_reconnect_resumes_and_reattaches() {
    let server = QuicTransport::new().unwrap();
    let (addr, incoming) = server
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    let conns: Conns = Arc::new(Mutex::new(Vec::new()));
    let mut dial = QuicDial {
        client,
        route,
        conns: conns.clone(),
    };
    let mut accept = QuicAccept { incoming };

    // First real QUIC connection.
    let (ct_c, ct_s) = tokio::join!(dial.connect(), accept.connect());
    let ct_c = ct_c.expect("dial 1");
    let ct_s = ct_s.expect("accept 1");

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let cfg_a = SessionConfig::new(caps());
    let cfg_b = SessionConfig::new(caps());
    let (wire_a, mut events_a, mut channels_a) = wiring();
    let (wire_b, _events_b, _channels_b) = wiring();

    let fa = PeerSession::open(
        ct_c,
        SessionRole::Initiator,
        cfg_a.clone(),
        wire_a.events.clone(),
        wire_a.channel_events.clone(),
        wire_a.incoming_streams.clone(),
        None,
        id_a,
        enc_a,
        trust_a,
    );
    let fb = PeerSession::open(
        ct_s,
        SessionRole::Responder,
        cfg_b.clone(),
        wire_b.events.clone(),
        wire_b.channel_events.clone(),
        wire_b.incoming_streams.clone(),
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let init = ra.expect("initiator opens over QUIC");
    let resp = rb.expect("responder opens over QUIC");

    let rec = RecoveryConfig {
        max_attempts: 5,
        backoff_base: Duration::from_millis(50),
        attempt_timeout: Duration::from_secs(10),
        token_ttl_ms: 30_000,
    };
    let mut mgr_a = RecoveryManager::new(init, Box::new(dial), rec, cfg_a, wire_a);
    let mut mgr_b = RecoveryManager::new(resp, Box::new(accept), rec, cfg_b, wire_b);
    let mut handle_a = mgr_a.handle_watch();
    tokio::spawn(async move { mgr_a.run().await });
    tokio::spawn(async move { mgr_b.run().await });

    // Open a channel over the live QUIC session.
    let h0 = live_handle(&mut handle_a).await;
    let c1 = h0.open_channel(CHAT).await.expect("open c1");
    next_channel(
        &mut channels_a,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;

    // Sever: close the underlying QUIC connection out from under both sessions.
    let first = conns.lock().expect("conns")[0].clone();
    first.close().await.expect("close first connection");

    // Both sides recover over a fresh QUIC connection and resume at epoch 1.
    next_event(&mut events_a, |e| {
        matches!(e, SessionEvent::Recovering { .. })
    })
    .await;
    let recovered = next_event(&mut events_a, |e| {
        matches!(e, SessionEvent::Recovered { .. })
    })
    .await;
    match recovered {
        SessionEvent::Recovered { epoch, .. } => assert_eq!(epoch, 1),
        _ => unreachable!(),
    }

    // The channel is re-attached over the new connection.
    next_channel(
        &mut channels_a,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;

    // The resumed session is usable: a fresh handle can open another channel.
    let h1 = live_handle(&mut handle_a).await;
    let c2 = h1.open_channel(CHAT).await.expect("open c2 after resume");
    next_channel(
        &mut channels_a,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c2),
    )
    .await;
}
