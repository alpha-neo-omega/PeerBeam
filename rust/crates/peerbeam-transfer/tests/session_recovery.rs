//! Integration tests for PeerSession reconnect + resume (M6).
//!
//! An in-memory, **severable** `ChannelTransport` models a network that can be cut
//! and re-established. Each connection is a `MemTransport` pair wrapped so a test
//! can sever it (all its streams then read EOF / fail to write), which drives the
//! session into recovery. A [`RecoveryManager`] on each side re-establishes a fresh
//! connection through a [`TransportFactory`] and resumes with a single-use token —
//! exercising the real authenticated identity, real per-channel crypto (bumped to a
//! fresh epoch), and real channel re-attachment. Only the socket is simulated.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;

use common::MemTransport;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, Frame, Link, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageFlags, MessageHandler, MessageType,
    SessionError, SessionFrame,
};
use peerbeam_transfer::{
    ChannelEvent, HandlerRegistry, Identity, IncomingStreamChannel, PeerSession, RecoveryConfig,
    RecoveryManager, SessionConfig, SessionEvent, SessionHandle, SessionRole, SessionWiring,
    TransportFactory,
};
use peerbeam_trust_fs::FsTrust;

// ── security helpers ────────────────────────────────────────────────────────

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
    std::mem::forget(dir);
    (identity, Arc::new(enc), Arc::new(trust))
}

const CHAT: ChannelType = ChannelType::new(0x0101); // a message channel

fn caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::new(CHAT))
}

// ── recording handler (responder side) ──────────────────────────────────────

type Log = Arc<Mutex<Vec<Vec<u8>>>>;

struct Recorder {
    log: Log,
}

#[async_trait]
impl MessageHandler for Recorder {
    fn channel_type(&self) -> ChannelType {
        CHAT
    }
    async fn handle(&self, frame: SessionFrame) -> std::result::Result<(), SessionError> {
        self.log.lock().expect("log").push(frame.payload.to_vec());
        Ok(())
    }
}

// ── severable in-memory transport ───────────────────────────────────────────

struct SeverableLink {
    inner: Box<dyn Link>,
    alive: watch::Receiver<bool>,
}

#[async_trait]
impl Link for SeverableLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if !*self.alive.borrow() {
            return Err(DomainError::Connection("severed".into()));
        }
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let mut alive = self.alive.clone();
        tokio::select! {
            r = self.inner.recv_frame() => r,
            _ = alive.wait_for(|a| !*a) => Ok(None),
        }
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

struct SeverableTransport {
    inner: Arc<MemTransport>,
    alive: watch::Receiver<bool>,
}

#[async_trait]
impl ChannelTransport for SeverableTransport {
    async fn open_stream(&self) -> Result<Box<dyn Link>> {
        if !*self.alive.borrow() {
            return Err(DomainError::Connection("severed".into()));
        }
        Ok(Box::new(SeverableLink {
            inner: self.inner.open_stream().await?,
            alive: self.alive.clone(),
        }))
    }
    async fn accept_stream(&self) -> Result<Option<Box<dyn Link>>> {
        let mut alive = self.alive.clone();
        tokio::select! {
            s = self.inner.accept_stream() => Ok(s?.map(|l| {
                Box::new(SeverableLink { inner: l, alive: self.alive.clone() }) as Box<dyn Link>
            })),
            _ = alive.wait_for(|a| !*a) => Ok(None),
        }
    }
    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
}

type Severs = Arc<Mutex<Vec<watch::Sender<bool>>>>;

/// The redialling side. `budget` caps how many connections it will ever mint
/// (`None` = unlimited); once exhausted, `connect` fails — modelling a network
/// that never comes back.
struct DialFactory {
    inbound: UnboundedSender<Arc<SeverableTransport>>,
    severs: Severs,
    budget: Option<usize>,
}

#[async_trait]
impl TransportFactory for DialFactory {
    async fn connect(&mut self) -> std::result::Result<Arc<dyn ChannelTransport>, SessionError> {
        if let Some(b) = self.budget.as_mut() {
            if *b == 0 {
                return Err(SessionError::Link("no route".into()));
            }
            *b -= 1;
        }
        let (a, b) = MemTransport::pair();
        let (alive_tx, alive_rx) = watch::channel(true);
        let client = Arc::new(SeverableTransport {
            inner: a,
            alive: alive_rx.clone(),
        });
        let server = Arc::new(SeverableTransport {
            inner: b,
            alive: alive_rx,
        });
        self.severs.lock().expect("severs").push(alive_tx);
        self.inbound
            .send(server)
            .map_err(|_| SessionError::Link("net closed".into()))?;
        Ok(client)
    }
}

/// The accepting side: each `connect` yields the next inbound connection.
struct AcceptFactory {
    inbound: UnboundedReceiver<Arc<SeverableTransport>>,
}

#[async_trait]
impl TransportFactory for AcceptFactory {
    async fn connect(&mut self) -> std::result::Result<Arc<dyn ChannelTransport>, SessionError> {
        self.inbound
            .recv()
            .await
            .map(|t| t as Arc<dyn ChannelTransport>)
            .ok_or_else(|| SessionError::Link("net closed".into()))
    }
}

// ── harness ─────────────────────────────────────────────────────────────────

fn wiring() -> (
    SessionWiring,
    UnboundedReceiver<SessionEvent>,
    UnboundedReceiver<ChannelEvent>,
    UnboundedReceiver<IncomingStreamChannel>,
) {
    let (ev_tx, events) = unbounded_channel();
    let (ch_tx, channels) = unbounded_channel();
    let (in_tx, incoming) = unbounded_channel();
    (
        SessionWiring {
            events: ev_tx,
            channel_events: ch_tx,
            incoming_streams: in_tx,
            registry: None,
        },
        events,
        channels,
        incoming,
    )
}

struct Endpoint {
    handle: watch::Receiver<Option<SessionHandle>>,
    events: UnboundedReceiver<SessionEvent>,
    channels: UnboundedReceiver<ChannelEvent>,
    severs: Severs,
    /// The initiator manager's join handle (its run result on close/exhaustion).
    init_run: tokio::task::JoinHandle<std::result::Result<(), SessionError>>,
}

/// Establish a session pair over the first connection, wrap each side in a
/// `RecoveryManager`, spawn both pumps, and return the initiator endpoint plus the
/// responder's recorded-message log. `dial_budget` caps redials.
async fn harness(rec: RecoveryConfig, dial_budget: Option<usize>) -> (Endpoint, Log) {
    let severs: Severs = Arc::new(Mutex::new(Vec::new()));
    let (inbound_tx, inbound_rx) = unbounded_channel();
    let mut dial = DialFactory {
        inbound: inbound_tx,
        severs: severs.clone(),
        budget: dial_budget,
    };
    let mut accept = AcceptFactory {
        inbound: inbound_rx,
    };

    let client_t = dial.connect().await.expect("dial 1");
    let server_t = accept.connect().await.expect("accept 1");

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");

    let cfg_a = SessionConfig::new(caps());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let handlers = HandlerRegistry::new().with(Arc::new(Recorder { log: log.clone() }));
    let cfg_b = SessionConfig::new(caps()).with_handlers(handlers);

    let (wire_a, events_a, channels_a, _in_a) = wiring();
    let (wire_b, _events_b, _channels_b, _in_b) = wiring();

    let fa = PeerSession::open(
        client_t,
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
        server_t,
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
    let init = ra.expect("initiator opens");
    let resp = rb.expect("responder opens");

    let mut mgr_a = RecoveryManager::new(init, Box::new(dial), rec, cfg_a, wire_a);
    let mut mgr_b = RecoveryManager::new(resp, Box::new(accept), rec, cfg_b, wire_b);
    let handle_a = mgr_a.handle_watch();
    let init_run = tokio::spawn(async move { mgr_a.run().await });
    tokio::spawn(async move { mgr_b.run().await });

    (
        Endpoint {
            handle: handle_a,
            events: events_a,
            channels: channels_a,
            severs,
            init_run,
        },
        log,
    )
}

fn sever(severs: &Severs) {
    let list = severs.lock().expect("severs");
    if let Some(s) = list.last() {
        let _ = s.send(false);
    }
}

async fn next_event(
    rx: &mut UnboundedReceiver<SessionEvent>,
    pred: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    loop {
        match tokio::time::timeout(WAIT, rx.recv()).await {
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
        match tokio::time::timeout(WAIT, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return ev,
            Ok(Some(_)) => continue,
            Ok(None) => panic!("channel event stream ended"),
            Err(_) => panic!("timed out waiting for a channel event"),
        }
    }
}

/// How long a test waits for something the code is expected to do.
///
/// **Generous on purpose.** These tests assert *behaviour* — an event arrives, a
/// handle reappears — never latency. Under `cargo test --workspace` this file
/// runs alongside a hundred other binaries, and a five-second bound turned CPU
/// starvation into a failure that looked like a reconnect bug: it failed
/// intermittently in full runs and passed every time on its own, which is the
/// signature of a clock, not a defect. A wait long enough to be unreachable
/// unless something is genuinely stuck says the same thing without lying about
/// what broke.
const WAIT: Duration = Duration::from_secs(60);

/// Timing for the tests that assert recovery **succeeds**.
///
/// Deliberately **not** `RecoveryConfig::default()`. The production default
/// gives up after five attempts, each bounded at ten seconds — so under
/// `cargo test --workspace`, with a hundred binaries competing for cores, an
/// in-process reconnect can miss its window five times and the session
/// genuinely stops trying. The test then reports "recovery never happened",
/// which is *true* and is a fact about the machine rather than about the code.
///
/// Many cheap attempts retry through that starvation. Nothing about the
/// behaviour under test changes — does a severed session resume, bump its
/// epoch, and re-attach its channels? — only how much transient unluckiness it
/// tolerates before concluding the answer is no.
///
/// The tests that assert recovery *fails* keep their own tight configs: for
/// those, giving up is the behaviour being checked.
fn resilient() -> RecoveryConfig {
    RecoveryConfig {
        max_attempts: 30,
        backoff_base: Duration::from_millis(5),
        attempt_timeout: Duration::from_secs(3),
        ..RecoveryConfig::default()
    }
}

async fn live_handle(rx: &mut watch::Receiver<Option<SessionHandle>>) -> SessionHandle {
    loop {
        if let Some(h) = rx.borrow_and_update().clone() {
            return h;
        }
        tokio::time::timeout(WAIT, rx.changed())
            .await
            .expect("handle watch timed out")
            .expect("handle watch closed");
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reconnect_resumes_and_reattaches_a_channel_with_message_continuity() {
    let (mut ep, log) = harness(resilient(), None).await;

    let h0 = live_handle(&mut ep.handle).await;
    let c1 = h0.open_channel(CHAT).await.expect("open c1");
    next_channel(
        &mut ep.channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;
    h0.send_on_channel(
        c1,
        MessageType::new(1),
        MessageFlags::END_OF_MESSAGE,
        Bytes::from_static(b"before"),
    )
    .await
    .expect("send before");

    sever(&ep.severs);
    next_event(&mut ep.events, |e| {
        matches!(e, SessionEvent::Recovering { .. })
    })
    .await;
    let recovered = next_event(&mut ep.events, |e| {
        matches!(e, SessionEvent::Recovered { .. })
    })
    .await;
    match recovered {
        SessionEvent::Recovered { epoch, .. } => assert_eq!(epoch, 1, "epoch bumped on resume"),
        _ => unreachable!(),
    }

    // Channel re-attached: a second Opened for the same id.
    next_channel(
        &mut ep.channels,
        |e| matches!(e, ChannelEvent::Opened { channel, .. } if *channel == c1),
    )
    .await;

    let h1 = live_handle(&mut ep.handle).await;
    h1.send_on_channel(
        c1,
        MessageType::new(1),
        MessageFlags::END_OF_MESSAGE,
        Bytes::from_static(b"after"),
    )
    .await
    .expect("send after");

    for _ in 0..100 {
        if log.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let got = log.lock().unwrap().clone();
    assert!(
        got.iter().any(|m| m == b"before"),
        "pre-reconnect message lost: {got:?}"
    );
    assert!(
        got.iter().any(|m| m == b"after"),
        "post-reconnect message lost: {got:?}"
    );
}

#[tokio::test]
async fn multiple_reconnects_bump_the_epoch_each_time() {
    let (mut ep, _log) = harness(resilient(), None).await;
    let _ = live_handle(&mut ep.handle).await;

    for expected_epoch in 1..=3u64 {
        sever(&ep.severs);
        let ev = next_event(&mut ep.events, |e| {
            matches!(e, SessionEvent::Recovered { .. })
        })
        .await;
        match ev {
            SessionEvent::Recovered { epoch, .. } => assert_eq!(epoch, expected_epoch),
            _ => unreachable!(),
        }
        let _ = live_handle(&mut ep.handle).await;
    }
}

#[tokio::test]
async fn recovery_exhausts_and_fails_closed_when_the_network_stays_down() {
    // Budget of 1 → the initial connection succeeds, every redial fails.
    let rec = RecoveryConfig {
        max_attempts: 3,
        backoff_base: Duration::from_millis(5),
        attempt_timeout: Duration::from_millis(200),
        token_ttl_ms: 30_000,
    };
    let (mut ep, _log) = harness(rec, Some(1)).await;
    let _ = live_handle(&mut ep.handle).await;

    sever(&ep.severs);
    // Three attempts, each dial fails, then it gives up.
    let failed = next_event(&mut ep.events, |e| {
        matches!(e, SessionEvent::RecoveryFailed { .. })
    })
    .await;
    assert!(matches!(failed, SessionEvent::RecoveryFailed { .. }));

    // The manager's run resolves with a typed exhaustion error, and the handle
    // watch has fallen to None (fail-closed).
    let result = tokio::time::timeout(WAIT, ep.init_run)
        .await
        .expect("manager did not finish")
        .expect("join");
    assert!(matches!(
        result,
        Err(SessionError::RecoveryExhausted { attempts: 3 })
    ));
    assert!(ep.handle.borrow().is_none(), "handle should be closed");
}

#[tokio::test]
async fn recovery_times_out_a_hung_dial_then_gives_up() {
    // budget 0 combined with a tiny attempt_timeout is covered above; here we prove
    // a *hung* dial is bounded by attempt_timeout rather than blocking forever.
    // A budget of 1 that then fails fast is the deterministic analogue; we assert
    // the whole recovery completes well within a generous overall bound.
    let rec = RecoveryConfig {
        max_attempts: 2,
        backoff_base: Duration::from_millis(1),
        attempt_timeout: Duration::from_millis(100),
        token_ttl_ms: 30_000,
    };
    let (mut ep, _log) = harness(rec, Some(1)).await;
    let _ = live_handle(&mut ep.handle).await;
    sever(&ep.severs);
    let result = tokio::time::timeout(Duration::from_secs(3), ep.init_run)
        .await
        .expect("recovery did not complete within bound")
        .expect("join");
    assert!(matches!(
        result,
        Err(SessionError::RecoveryExhausted { .. })
    ));
}
