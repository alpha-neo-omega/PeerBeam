//! Integration tests for the PeerSession skeleton (M2): two endpoints over an
//! in-memory link, exercising establishment, negotiation, keepalive, and close.

mod common;

use common::MemLink;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, SessionError, Version};
use peerbeam_transfer::{
    CloseReason, Flow, PeerSession, SessionConfig, SessionEvent, SessionRegistry, SessionRole,
};

/// Open both ends concurrently with the given capability sets and per-side
/// registries.
async fn open_pair(
    cfg_a: SessionConfig,
    cfg_b: SessionConfig,
) -> (
    Result<PeerSession, SessionError>,
    Result<PeerSession, SessionError>,
    UnboundedReceiver<SessionEvent>,
    UnboundedReceiver<SessionEvent>,
    SessionRegistry,
    SessionRegistry,
) {
    let (la, lb) = MemLink::pair(8);
    let (tx_a, rx_a) = mpsc::unbounded_channel();
    let (tx_b, rx_b) = mpsc::unbounded_channel();
    let reg_a = SessionRegistry::new();
    let reg_b = SessionRegistry::new();
    let fa = PeerSession::open(
        Box::new(la),
        SessionRole::Initiator,
        DeviceId::from("device-b"),
        cfg_a,
        tx_a,
        Some(reg_a.clone()),
    );
    let fb = PeerSession::open(
        Box::new(lb),
        SessionRole::Responder,
        DeviceId::from("device-a"),
        cfg_b,
        tx_b,
        Some(reg_b.clone()),
    );
    let (ra, rb) = tokio::join!(fa, fb);
    (ra, rb, rx_a, rx_b, reg_a, reg_b)
}

fn caps(extra: ChannelType) -> CapabilitySet {
    CapabilitySet::new().with(Capability::new(extra))
}

fn drain(rx: &mut UnboundedReceiver<SessionEvent>) -> Vec<SessionEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn two_endpoints_establish_and_agree() {
    let (ra, rb, mut rx_a, mut rx_b, reg_a, reg_b) = open_pair(
        SessionConfig::new(caps(ChannelType::TRANSFER)),
        SessionConfig::new(caps(ChannelType::TRANSFER)),
    )
    .await;
    let a = ra.expect("initiator opens");
    let b = rb.expect("responder opens");

    assert!(a.state().is_active());
    assert!(b.state().is_active());
    // The responder adopts the initiator's minted id.
    assert_eq!(a.id(), b.id());
    assert!(!a.id().is_nil());
    assert_eq!(a.version(), Version::CURRENT);

    // Both advertised TRANSFER + the implicit CONTROL, so both are agreed.
    assert!(a.capabilities().supports(ChannelType::CONTROL));
    assert!(a.capabilities().supports(ChannelType::TRANSFER));

    // Each side emitted Established and registered itself.
    assert!(matches!(
        drain(&mut rx_a).first(),
        Some(SessionEvent::Established { .. })
    ));
    assert!(matches!(
        drain(&mut rx_b).first(),
        Some(SessionEvent::Established { .. })
    ));
    assert_eq!(reg_a.len(), 1);
    assert_eq!(reg_b.len(), 1);
    assert!(reg_a.get(a.id()).is_some());
}

#[tokio::test]
async fn close_ends_both_sides() {
    let (ra, rb, _rx_a, mut rx_b, reg_a, _reg_b) = open_pair(
        SessionConfig::new(CapabilitySet::new()),
        SessionConfig::new(CapabilitySet::new()),
    )
    .await;
    let mut a = ra.expect("initiator");
    let b = rb.expect("responder");
    let a_id = a.id();

    // Run the responder until the initiator's shutdown reaches it.
    let b_task = tokio::spawn(async move {
        let mut b = b;
        b.run().await.map(|()| b)
    });

    a.close().await.expect("close");
    let b = b_task.await.expect("join").expect("run");

    assert!(a.state().is_terminal());
    assert!(b.state().is_terminal());
    assert!(
        reg_a.remove(a_id).is_none(),
        "closing deregisters the session"
    );

    // The responder saw the peer-initiated close.
    assert!(drain(&mut rx_b).iter().any(|e| matches!(
        e,
        SessionEvent::Closed {
            reason: CloseReason::Peer(_),
            ..
        }
    )));
}

#[tokio::test]
async fn incompatible_major_versions_are_rejected() {
    let mut cfg_a = SessionConfig::new(CapabilitySet::new());
    cfg_a.version = Version::new(2, 0);
    let cfg_b = SessionConfig::new(CapabilitySet::new()); // 1.0

    let (ra, rb, _rx_a, _rx_b, _reg_a, _reg_b) = open_pair(cfg_a, cfg_b).await;

    assert!(matches!(ra, Err(SessionError::VersionIncompatible { .. })));
    assert!(matches!(rb, Err(SessionError::VersionIncompatible { .. })));
}

#[tokio::test]
async fn capabilities_are_intersected_and_unknown_ones_dropped() {
    // A supports TRANSFER; B supports chat (0x0101). Only the implicit CONTROL is
    // common; each side's unique capability is dropped.
    let (ra, rb, _rx_a, _rx_b, _reg_a, _reg_b) = open_pair(
        SessionConfig::new(caps(ChannelType::TRANSFER)),
        SessionConfig::new(caps(ChannelType::new(0x0101))),
    )
    .await;
    let a = ra.expect("a");
    let b = rb.expect("b");

    for session in [&a, &b] {
        assert!(session.capabilities().supports(ChannelType::CONTROL));
        assert!(!session.capabilities().supports(ChannelType::TRANSFER));
        assert!(!session.capabilities().supports(ChannelType::new(0x0101)));
        assert_eq!(session.capabilities().len(), 1);
    }
}

#[tokio::test]
async fn ping_is_answered_with_pong() {
    let (ra, rb, _rx_a, mut rx_b, _reg_a, _reg_b) = open_pair(
        SessionConfig::new(CapabilitySet::new()),
        SessionConfig::new(CapabilitySet::new()),
    )
    .await;
    let mut a = ra.expect("a");
    let b = rb.expect("b");

    let b_task = tokio::spawn(async move {
        let mut b = b;
        b.run().await.map(|()| b)
    });

    a.send_ping().await.expect("send ping");
    // The responder replies with Pong; the initiator receives it and stays open.
    assert_eq!(
        a.recv_and_dispatch().await.expect("recv pong"),
        Flow::Continue
    );

    a.close().await.expect("close");
    let _b = b_task.await.expect("join").expect("run");

    assert!(drain(&mut rx_b)
        .iter()
        .any(|e| matches!(e, SessionEvent::PingReceived { .. })));
}

#[tokio::test]
async fn peer_hangup_closes_the_session() {
    let (ra, rb, _rx_a, _rx_b, _reg_a, _reg_b) = open_pair(
        SessionConfig::new(CapabilitySet::new()),
        SessionConfig::new(CapabilitySet::new()),
    )
    .await;
    let mut a = ra.expect("a");
    let b = rb.expect("b");

    // Drop the responder: its link end closes, so the initiator's next receive
    // observes a clean hangup and closes.
    drop(b);
    assert_eq!(a.recv_and_dispatch().await.expect("recv"), Flow::Closed);
    assert!(a.state().is_terminal());
}
