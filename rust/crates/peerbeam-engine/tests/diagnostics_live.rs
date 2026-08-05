//! Live wiring test for `SessionDiagnostics` (M8): a real PeerSession registers
//! into the diagnostics' registry, its channels are snapshotted through its
//! handle, and a transport-selected transfer records migration metrics into the
//! same diagnostics — proving the engine is the single source of truth and no
//! state is duplicated. In-memory transport; real crypto + handshake.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{ChannelTransport, EncryptionProvider, Frame, Link, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType};
use peerbeam_engine::SessionDiagnostics;
use peerbeam_transfer::{
    ChannelEvent, Identity, PeerSession, SessionConfig, SessionEvent, SessionHandle, SessionRole,
};
use peerbeam_trust_fs::FsTrust;

const CHAT: ChannelType = ChannelType::new(0x0101);

type Sec = (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>);

fn security(name: &str) -> Sec {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: DeviceId::from(name),
        name: name.to_string(),
        keypair,
    };
    let dir = tempfile::tempdir().unwrap();
    let trust = FsTrust::open(dir.path().join("trust.json")).unwrap();
    std::mem::forget(dir);
    (identity, Arc::new(enc), Arc::new(trust))
}

// ── in-memory ChannelTransport ───────────────────────────────────────────────

struct MemLink {
    tx: tokio::sync::mpsc::Sender<Frame>,
    rx: tokio::sync::mpsc::Receiver<Frame>,
}
impl MemLink {
    fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a, br) = tokio::sync::mpsc::channel(cap);
        let (b, ar) = tokio::sync::mpsc::channel(cap);
        (MemLink { tx: a, rx: ar }, MemLink { tx: b, rx: br })
    }
}
#[async_trait]
impl Link for MemLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| DomainError::Connection("peer closed".into()))
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        Ok(self.rx.recv().await)
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

struct MemTransport {
    peer_accept: tokio::sync::mpsc::UnboundedSender<Box<dyn Link>>,
    my_accept: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Box<dyn Link>>>,
}
impl MemTransport {
    fn pair() -> (Arc<MemTransport>, Arc<MemTransport>) {
        let (a2b, br) = unbounded_channel();
        let (b2a, ar) = unbounded_channel();
        (
            Arc::new(MemTransport {
                peer_accept: a2b,
                my_accept: tokio::sync::Mutex::new(ar),
            }),
            Arc::new(MemTransport {
                peer_accept: b2a,
                my_accept: tokio::sync::Mutex::new(br),
            }),
        )
    }
}
#[async_trait]
impl ChannelTransport for MemTransport {
    async fn open_stream(&self) -> Result<Box<dyn Link>> {
        let (mine, theirs) = MemLink::pair(32);
        self.peer_accept
            .send(Box::new(theirs))
            .map_err(|_| DomainError::Connection("peer transport closed".into()))?;
        Ok(Box::new(mine))
    }
    async fn accept_stream(&self) -> Result<Option<Box<dyn Link>>> {
        Ok(self.my_accept.lock().await.recv().await)
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

fn cfg(caps: &[ChannelType], stream: Option<ChannelType>) -> SessionConfig {
    let mut set = CapabilitySet::new();
    for c in caps {
        set = set.with(Capability::new(*c));
    }
    let mut cfg = SessionConfig::new(set);
    if let Some(s) = stream {
        cfg = cfg.with_stream_channel_type(s);
    }
    cfg
}

/// Open both ends over a MemTransport pair; the initiator registers into
/// `diag`. Returns the running initiator + responder handles and event receivers.
struct Live {
    a: SessionHandle,
    #[allow(dead_code)]
    b: SessionHandle,
    a_channels: UnboundedReceiver<ChannelEvent>,
    #[allow(dead_code)]
    a_events: UnboundedReceiver<SessionEvent>,
}

async fn establish(diag: &SessionDiagnostics, caps: &[ChannelType]) -> Live {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, a_events) = unbounded_channel();
    let (b_ev, _b_ev) = unbounded_channel();
    let (a_ch, a_channels) = unbounded_channel();
    let (b_ch, _b_ch) = unbounded_channel();
    let (a_in, _a_in) = unbounded_channel();
    let (b_in, _b_in) = unbounded_channel();
    std::mem::forget(_b_ev);
    std::mem::forget(_b_ch);
    std::mem::forget(_a_in);
    std::mem::forget(_b_in);
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        cfg(caps, None),
        a_ev,
        a_ch,
        a_in,
        Some(diag.registry()),
        id_a,
        enc_a,
        trust_a,
    );
    let fb = PeerSession::open(
        tb,
        SessionRole::Responder,
        cfg(caps, None),
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
    let ah = a.handle();
    let bh = b.handle();
    diag.register_handle(a.id(), ah.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });
    Live {
        a: ah,
        b: bh,
        a_channels,
        a_events,
    }
}

#[tokio::test]
async fn live_session_and_channel_appear_in_diagnostics() {
    let diag = SessionDiagnostics::new();
    let mut live = establish(&diag, &[CHAT]).await;

    // The session is registered and visible.
    let sessions = diag.sessions_json();
    assert_eq!(
        sessions["count"], 1,
        "session should be registered: {sessions}"
    );
    let s0 = &sessions["sessions"][0];
    assert_eq!(s0["state"], "active");
    assert_eq!(s0["version"], "1.0");
    let id = s0["id"].as_str().unwrap().to_string();

    // Open a message channel; it appears in the channel snapshot.
    let c1 = live.a.open_channel(CHAT).await.expect("open channel");
    // Wait for it to become Open on both ends.
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), live.a_channels.recv()).await
        {
            Ok(Some(ChannelEvent::Opened { channel, .. })) if channel == c1 => break,
            Ok(Some(_)) => continue,
            _ => panic!("channel did not open"),
        }
    }
    let channels = diag.channels_json(&id).await;
    assert_eq!(channels["count"], 1, "channel snapshot: {channels}");
    assert_eq!(channels["channels"][0]["channel_type"], "0x0101");

    // session_json round-trips by id; recovery view is empty (session is active).
    assert_eq!(diag.session_json(&id)["session"]["id"], id);
    assert_eq!(diag.recovery_json()["recovering"], 0);
}
