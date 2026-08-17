//! The two privacy gates, proven over a **real two-PeerSession round trip**
//! rather than against the pure predicate alone.
//!
//! This distinction is the whole point of this file. `gate.rs`'s unit tests
//! prove `may_share_status` returns the right answer; they cannot prove the
//! send path actually *asks* it. These tests drive `PresenceSender::beat` — the
//! only code that puts a `Status` on the wire — against a live peer, and assert
//! on what that peer's registry received. Delete the trust leg or the opt-in
//! leg from `may_share_status` and these fail, because a status really does
//! arrive on the other side.
//!
//! The PeerSession wiring mirrors `peerbeam-chat/tests/roundtrip.rs` verbatim
//! (via the `common` module copied from the transfer crate's harness), with
//! PRESENCE substituted for CHAT.

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::MemTransport;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, PRESENCE_FEAT_STATUS,
};
use peerbeam_presence::{
    Beat, PeerStatus, PresenceHandler, PresenceRegistry, PresenceSender, PresenceSink, Status,
};
use peerbeam_transfer::{HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole};
use peerbeam_trust_fs::FsTrust;
use tokio::sync::mpsc::unbounded_channel;

/// Fresh identity, encryption provider and trust store for one endpoint.
/// Copied from `peerbeam-chat/tests/roundtrip.rs::security`.
fn security(name: &str) -> (Identity, Arc<dyn EncryptionProvider>, Arc<FsTrust>) {
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

/// What a build with presence advertises: the channel plus its feature bit.
fn presence_caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::with_features(
        ChannelType::PRESENCE,
        PRESENCE_FEAT_STATUS,
    ))
}

/// What a 1b/2a-era peer advertises: chat and transfer, no presence at all.
fn legacy_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::new(ChannelType::TRANSFER))
        .with(Capability::new(ChannelType::CHAT))
}

fn a_status() -> Status {
    Status {
        battery_percent: Some(64),
        charging: Some(false),
        storage_free_bytes: Some(123_456),
        network: Some("lan".into()),
        app_version: Some("0.4.1".into()),
        sent_at: "2026-08-17T10:00:00Z".into(),
    }
}

/// Everything one live A↔B pair gives a test to work with.
struct Pair {
    sender: PresenceSender,
    /// What B (the receiving side) has recorded.
    b_registry: PresenceRegistry,
    /// Every status B's sink was handed.
    b_sink: Arc<Mutex<Vec<(DeviceId, PeerStatus)>>>,
    /// A's trust store, so a test can pin or revoke B mid-session.
    a_trust: Arc<FsTrust>,
    /// A's live opt-in setting, so a test can flip it mid-session.
    a_sharing: Arc<Mutex<bool>>,
}

/// Stand up two real PeerSessions over the in-memory transport and hand back a
/// `PresenceSender` pointing A at B.
///
/// `a_trusts_b` decides A's trust state; `sharing` seeds A's opt-in setting;
/// `b_advertises` is what B puts on the wire, so a test can play a legacy peer.
///
/// **How "untrusted" is produced matters.** PeerBeam's handshake is TOFU: the
/// authenticated peer is *pinned* as it connects, so by the time a session
/// exists it is in the trust store. Faking an untrusted peer by pre-seeding a
/// bogus record does not work and should not — the handshake correctly refuses
/// it as a possible MITM ("key changed since it was trusted"). So the untrusted
/// case here is the one that actually occurs in production: a genuine
/// handshake, a genuine pin, and then the pin **revoked** — the user removing a
/// device under Settings → Trusted devices while a session is still live. That
/// is the real shape of the trust gate's job, and it is why the gate is
/// consulted per beat instead of once at session start.
async fn pair(a_trusts_b: bool, sharing: bool, b_advertises: CapabilitySet) -> Pair {
    let b_registry = PresenceRegistry::new();
    let b_sink_log: Arc<Mutex<Vec<(DeviceId, PeerStatus)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = b_sink_log.clone();
    let sink: PresenceSink = Arc::new(move |id, st| log.lock().unwrap().push((id, st)));
    let (handler_b, peer_slot_b): (_, Arc<OnceLock<DeviceId>>) =
        PresenceHandler::new(b_registry.clone(), sink);

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

    let a_cfg = SessionConfig::new(presence_caps());
    let b_cfg = SessionConfig::new(b_advertises)
        .with_handlers(HandlerRegistry::new().with(handler_b as Arc<dyn MessageHandler>));

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
        trust_a.clone() as Arc<dyn TrustStore>,
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
        trust_b as Arc<dyn TrustStore>,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator opens");
    let mut b = rb.expect("responder opens");
    let a_handle = a.handle();
    // The NEGOTIATED set — this is what the gate is asked about, so a legacy B
    // really does AND the capability away here rather than in a fixture.
    let negotiated = a.capabilities().clone();

    let _ = peer_slot_b.set(a_id.clone());
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // The handshake has TOFU-pinned B in A's store. Both branches assert the
    // state they claim to set up, so neither can silently become the other.
    assert!(
        trust_a.is_trusted(&b_id),
        "the handshake must pin B, or the trusted case proves nothing"
    );
    // ...but a pin is not approval. The handshake records every never-seen
    // peer with `approved: false` so a later key change is detectable, and the
    // gate deliberately asks `is_approved` rather than `is_trusted` — otherwise
    // any stranger that completed a handshake would be sent our status. So the
    // trusted case has to do what the user does: accept the device.
    assert!(
        !trust_a.is_approved(&b_id),
        "a fresh handshake must not approve anyone by itself"
    );
    if a_trusts_b {
        trust_a.approve(&b_id).expect("the user accepts B");
        assert!(trust_a.is_approved(&b_id), "approval must stick");
    }
    if !a_trusts_b {
        assert!(
            trust_a.remove(&b_id).expect("revoke"),
            "revoking must actually remove a pin that was there"
        );
        assert!(
            !trust_a.is_trusted(&b_id),
            "B must really be untrusted — a test that cannot tell the \
             difference proves nothing"
        );
    }

    let a_sharing = Arc::new(Mutex::new(sharing));
    let sharing_cl = a_sharing.clone();
    let sender = PresenceSender::new(
        a_handle,
        b_id,
        negotiated,
        trust_a.clone() as Arc<dyn TrustStore>,
        Arc::new(move || *sharing_cl.lock().unwrap()),
        Arc::new(a_status),
    );

    Pair {
        sender,
        b_registry,
        b_sink: b_sink_log,
        a_trust: trust_a,
        a_sharing,
    }
}

/// Poll B's registry briefly — real async timing, not a fixed sleep.
async fn wait_for_status(reg: &PresenceRegistry) -> Option<PeerStatus> {
    for _ in 0..200 {
        if let Some(s) = reg.get(&DeviceId::from("device-a")) {
            return Some(s);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// Give a *withheld* beat every chance to arrive before declaring it absent.
///
/// This is the load-bearing helper of the negative tests: asserting "nothing
/// arrived" immediately after `beat()` returns would pass even if the status
/// were merely still in flight. Waiting out a window that the positive test
/// proves is far more than enough is what makes the absence meaningful.
async fn assert_nothing_arrives(reg: &PresenceRegistry, why: &str) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        reg.get(&DeviceId::from("device-a")).is_none(),
        "a status reached the peer, but {why}"
    );
    assert!(reg.is_empty(), "registry must be untouched, but {why}");
}

/// The baseline. Without this passing, every negative test below is vacuous —
/// they would all "pass" against a build that simply never sends anything.
#[tokio::test]
async fn a_trusted_capable_peer_with_sharing_on_receives_a_status() {
    let mut p = pair(true, true, presence_caps()).await;
    assert!(p.sender.may_send(), "all three gates should be open");
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Sent);

    let got = wait_for_status(&p.b_registry)
        .await
        .expect("B did not receive a status within 2s");
    assert_eq!(got.status, a_status());
    assert_eq!(p.b_sink.lock().unwrap().len(), 1, "B's sink fired once");
    assert_eq!(p.b_sink.lock().unwrap()[0].0, DeviceId::from("device-a"));
}

/// **The trust gate.** An untrusted peer is never sent a status *even with the
/// setting on*.
///
/// Mutation target: delete `trust.is_trusted(peer)` from `may_share_status` and
/// this test fails — B's registry fills with A's battery level.
#[tokio::test]
async fn an_untrusted_peer_is_never_sent_a_status_even_with_sharing_on() {
    let mut p = pair(false, true, presence_caps()).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .beat()
            .await
            .expect("a withheld beat is not an error"),
        Beat::Withheld
    );
    assert_nothing_arrives(&p.b_registry, "the peer is not trusted").await;
    assert!(
        p.b_sink.lock().unwrap().is_empty(),
        "and nothing was surfaced"
    );
}

/// Trust is re-read per beat, not captured at session start: revoking trust
/// mid-session stops the next heartbeat rather than the next reconnect.
#[tokio::test]
async fn revoking_trust_mid_session_stops_the_next_heartbeat() {
    let mut p = pair(true, true, presence_caps()).await;
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Sent);
    wait_for_status(&p.b_registry)
        .await
        .expect("first beat lands");

    // The user un-trusts A's peer while the session is still up.
    p.a_trust
        .remove(&DeviceId::from("device-b"))
        .expect("revoke trust");

    assert!(!p.sender.may_send(), "the gate must notice immediately");
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Withheld);
}

/// **The opt-in gate.** With the setting off nothing is sent — to a peer that
/// is trusted and fully capable.
///
/// Mutation target: delete `sharing_enabled` from `may_share_status` and this
/// test fails.
#[tokio::test]
async fn sharing_off_sends_nothing_to_a_trusted_capable_peer() {
    let mut p = pair(true, false, presence_caps()).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .beat()
            .await
            .expect("a withheld beat is not an error"),
        Beat::Withheld
    );
    assert_nothing_arrives(&p.b_registry, "sharing is off").await;
}

/// The other half of the opt-in: *"it may still receive and display others'"*.
/// A device with sharing off is a full participant in everyone else's
/// dashboard, contributing nothing to it.
#[tokio::test]
async fn sharing_off_still_receives_and_displays_an_incoming_status() {
    // A has sharing OFF, so it sends nothing...
    let mut p = pair(true, false, presence_caps()).await;
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Withheld);
    assert_nothing_arrives(&p.b_registry, "sharing is off").await;

    // ...but a status arriving at a receiver is recorded and surfaced
    // regardless of that receiver's own setting. B's handler here is the
    // receive path, and it consults no setting at all.
    let inbound = Status {
        battery_percent: Some(12),
        ..a_status()
    };
    let frame = inbound
        .to_frame(peerbeam_domain::session::ChannelId::new(9))
        .expect("encode");
    let (handler, slot) = PresenceHandler::new(p.b_registry.clone(), Arc::new(|_, _| {}));
    let _ = slot.set(DeviceId::from("device-a"));
    handler.handle(frame).await.expect("receiving is ungated");

    assert_eq!(
        p.b_registry
            .get(&DeviceId::from("device-a"))
            .expect("recorded")
            .status
            .battery_percent,
        Some(12),
        "receiving must not be gated by the sender-side opt-in"
    );
}

/// Turning the setting off mid-session stops the heartbeat at the next tick.
#[tokio::test]
async fn turning_sharing_off_mid_session_stops_the_next_heartbeat() {
    let mut p = pair(true, true, presence_caps()).await;
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Sent);
    wait_for_status(&p.b_registry)
        .await
        .expect("first beat lands");

    *p.a_sharing.lock().unwrap() = false;

    assert!(!p.sender.may_send());
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Withheld);
}

/// **The negotiation gate.** A 1b/2a-era peer that does not advertise Presence
/// is never sent a Status — the intersection drops the capability entirely.
#[tokio::test]
async fn a_peer_that_does_not_advertise_presence_is_never_sent_a_status() {
    let mut p = pair(true, true, legacy_caps()).await;
    assert!(
        !p.sender.may_send(),
        "a peer without the capability must be refused"
    );
    assert_eq!(p.sender.beat().await.expect("beat"), Beat::Withheld);
    assert_nothing_arrives(&p.b_registry, "the peer never advertised Presence").await;
}

/// A peer advertising the channel with `features: 0` is likewise sent nothing —
/// it shows as "status not shared", never as an error, and the session and its
/// other channels are entirely unaffected.
#[tokio::test]
async fn a_peer_advertising_presence_without_the_feature_bit_is_sent_nothing() {
    let bare = CapabilitySet::new().with(Capability::new(ChannelType::PRESENCE));
    let mut p = pair(true, true, bare).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .beat()
            .await
            .expect("this is not an error condition"),
        Beat::Withheld
    );
    assert_nothing_arrives(&p.b_registry, "the feature bit was ANDed away").await;
}

/// Heartbeats replace rather than accumulate, and each one is a fresh
/// collection — the point of a cadence is that the numbers actually move.
#[tokio::test]
async fn repeated_beats_replace_the_peers_view_rather_than_accumulating() {
    let mut p = pair(true, true, presence_caps()).await;
    p.sender.beat().await.expect("first");
    wait_for_status(&p.b_registry).await.expect("first lands");
    p.sender.beat().await.expect("second");
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(p.b_registry.len(), 1, "one entry per peer, not a log");
    assert!(
        p.b_sink.lock().unwrap().len() >= 2,
        "each heartbeat surfaces, so a dashboard can re-render"
    );
}
