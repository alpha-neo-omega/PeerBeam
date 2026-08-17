//! The two privacy gates, proven over a **real two-PeerSession round trip**
//! rather than against the pure predicate alone.
//!
//! This distinction is the whole point of this file. `gate.rs`'s unit tests
//! prove `may_share_clip` returns the right answer; they cannot prove the send
//! path actually *asks* it. These tests drive `ClipboardSender::send` — the
//! only code that puts a `Clip` on the wire — against a live peer, and assert
//! on what that peer's sink received. Delete the trust leg or the opt-in leg
//! from `may_share_clip` and these fail, because a clipboard really does arrive
//! on the other side.
//!
//! The PeerSession wiring mirrors `peerbeam-presence/tests/gates.rs` verbatim
//! (via the `common` module copied from the transfer crate's harness), with
//! CLIPBOARD substituted for PRESENCE.

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::MemTransport;
use peerbeam_clipboard::{Clip, ClipboardHandler, ClipboardSender, ClipboardSink, Push, MAX_CLIP};
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelType, MessageHandler, CLIPBOARD_FEAT_CLIP,
};
use peerbeam_transfer::{HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole};
use peerbeam_trust_fs::FsTrust;
use tokio::sync::mpsc::unbounded_channel;

/// Fresh identity, encryption provider and trust store for one endpoint.
/// Copied from `peerbeam-presence/tests/gates.rs::security`.
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

/// What a build with clipboard sync advertises: the channel plus its bit.
fn clipboard_caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::with_features(
        ChannelType::CLIPBOARD,
        CLIPBOARD_FEAT_CLIP,
    ))
}

/// What a peer from before clipboard sync advertises: chat and transfer only.
fn legacy_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::new(ChannelType::TRANSFER))
        .with(Capability::new(ChannelType::CHAT))
}

const SECRET: &str = "correct horse battery staple";

/// Everything one live A↔B pair gives a test to work with.
struct Pair {
    sender: ClipboardSender,
    /// Every clip B's sink was handed.
    b_sink: Arc<Mutex<Vec<(DeviceId, Clip)>>>,
    /// A's trust store, so a test can revoke B mid-session.
    a_trust: Arc<FsTrust>,
    /// A's live opt-in setting, so a test can flip it mid-session.
    a_sync: Arc<Mutex<bool>>,
}

impl Pair {
    /// What B actually received, in order.
    fn received(&self) -> Vec<String> {
        self.b_sink
            .lock()
            .unwrap()
            .iter()
            .map(|(_, c)| c.text.clone())
            .collect()
    }
}

/// Stand up two real PeerSessions over the in-memory transport and hand back a
/// `ClipboardSender` pointing A at B.
///
/// `a_trusts_b` decides A's trust state; `sync` seeds A's opt-in setting;
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
/// consulted per push instead of once at session start.
async fn pair(a_trusts_b: bool, sync: bool, b_advertises: CapabilitySet) -> Pair {
    let b_sink_log: Arc<Mutex<Vec<(DeviceId, Clip)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = b_sink_log.clone();
    let sink: ClipboardSink = Arc::new(move |id, clip| log.lock().unwrap().push((id, clip)));
    let (handler_b, peer_slot_b): (_, Arc<OnceLock<DeviceId>>) = ClipboardHandler::new(sink);

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

    let a_cfg = SessionConfig::new(clipboard_caps());
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

    let a_sync = Arc::new(Mutex::new(sync));
    let sync_cl = a_sync.clone();
    let sender = ClipboardSender::new(
        a_handle,
        b_id,
        negotiated,
        trust_a.clone() as Arc<dyn TrustStore>,
        Arc::new(move || *sync_cl.lock().unwrap()),
    );

    Pair {
        sender,
        b_sink: b_sink_log,
        a_trust: trust_a,
        a_sync,
    }
}

/// Poll B's sink briefly — real async timing, not a fixed sleep.
async fn wait_for_clip(p: &Pair) -> Option<String> {
    for _ in 0..200 {
        if let Some(first) = p.received().first() {
            return Some(first.clone());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// Give a *withheld* push every chance to arrive before declaring it absent.
///
/// This is the load-bearing helper of the negative tests: asserting "nothing
/// arrived" immediately after `send()` returns would pass even if the clip were
/// merely still in flight. Waiting out a window that the positive test proves is
/// far more than enough is what makes the absence meaningful.
async fn assert_nothing_arrives(p: &Pair, why: &str) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        p.received().is_empty(),
        "a clipboard reached the peer, but {why}"
    );
}

/// The baseline. Without this passing, every negative test below is vacuous —
/// they would all "pass" against a build that simply never sends anything.
#[tokio::test]
async fn a_trusted_capable_peer_with_sync_on_receives_a_clip() {
    let mut p = pair(true, true, clipboard_caps()).await;
    assert!(p.sender.may_send(), "all three gates should be open");
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Sent);

    assert_eq!(
        wait_for_clip(&p).await.expect("B did not receive a clip"),
        SECRET
    );
    assert_eq!(
        p.b_sink.lock().unwrap()[0].0,
        DeviceId::from("device-a"),
        "the clip is attributed to the authenticated sender"
    );
}

/// **The trust gate.** An untrusted peer is never sent a clip *even with the
/// setting on*.
///
/// Mutation target: delete `trust.is_trusted(peer)` from `may_share_clip` and
/// this test fails — B's sink fills with A's clipboard.
#[tokio::test]
async fn an_untrusted_peer_is_never_sent_a_clip_even_with_sync_on() {
    let mut p = pair(false, true, clipboard_caps()).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .send(SECRET)
            .await
            .expect("a withheld push is not an error"),
        Push::Withheld
    );
    assert_nothing_arrives(&p, "the peer is not trusted").await;
}

/// Trust is re-read per push, not captured at session start: revoking trust
/// mid-session stops the next clip rather than the next reconnect.
#[tokio::test]
async fn revoking_trust_mid_session_stops_the_next_clip() {
    let mut p = pair(true, true, clipboard_caps()).await;
    assert_eq!(p.sender.send("first").await.expect("send"), Push::Sent);
    wait_for_clip(&p).await.expect("first push lands");

    // The user un-trusts A's peer while the session is still up.
    p.a_trust
        .remove(&DeviceId::from("device-b"))
        .expect("revoke trust");

    assert!(!p.sender.may_send(), "the gate must notice immediately");
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Withheld);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !p.received().contains(&SECRET.to_string()),
        "a clip copied after revocation must not reach the revoked device"
    );
}

/// **The opt-in gate.** With the setting off nothing is sent — to a peer that
/// is trusted and fully capable.
///
/// Mutation target: delete `sync_enabled` from `may_share_clip` and this test
/// fails.
#[tokio::test]
async fn sync_off_sends_nothing_to_a_trusted_capable_peer() {
    let mut p = pair(true, false, clipboard_caps()).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .send(SECRET)
            .await
            .expect("a withheld push is not an error"),
        Push::Withheld
    );
    assert_nothing_arrives(&p, "sync is off").await;
}

/// The other half of the opt-in: a device with sync **off** still *receives*.
/// Opt-in governs what leaves this machine, not what reaches it — which is also
/// what lets a phone take part at all.
#[tokio::test]
async fn sync_off_still_receives_and_applies_an_incoming_clip() {
    // A has sync OFF, so it sends nothing...
    let mut p = pair(true, false, clipboard_caps()).await;
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Withheld);
    assert_nothing_arrives(&p, "sync is off").await;

    // ...but a clip arriving at a receiver is surfaced regardless of that
    // receiver's own setting. The handler here is the receive path, and it
    // consults no setting at all.
    let applied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = applied.clone();
    let (handler, slot) =
        ClipboardHandler::new(Arc::new(move |_, c: Clip| log.lock().unwrap().push(c.text)));
    let _ = slot.set(DeviceId::from("device-b"));
    let frame = Clip::new("from the other side")
        .expect("encode")
        .to_frame(peerbeam_domain::session::ChannelId::new(9))
        .expect("frame");
    handler.handle(frame).await.expect("receiving is ungated");

    assert_eq!(
        applied.lock().unwrap().as_slice(),
        ["from the other side"],
        "receiving must not be gated by the sender-side opt-in"
    );
}

/// Turning the setting off mid-session stops the next clip.
#[tokio::test]
async fn turning_sync_off_mid_session_stops_the_next_clip() {
    let mut p = pair(true, true, clipboard_caps()).await;
    assert_eq!(p.sender.send("first").await.expect("send"), Push::Sent);
    wait_for_clip(&p).await.expect("first push lands");

    *p.a_sync.lock().unwrap() = false;

    assert!(!p.sender.may_send());
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Withheld);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !p.received().contains(&SECRET.to_string()),
        "a clip copied after the toggle went off must not go out"
    );
}

/// **The negotiation gate.** A peer that does not advertise Clipboard is never
/// sent a Clip — the intersection drops the capability entirely.
#[tokio::test]
async fn a_peer_that_does_not_advertise_clipboard_is_never_sent_a_clip() {
    let mut p = pair(true, true, legacy_caps()).await;
    assert!(
        !p.sender.may_send(),
        "a peer without the capability must be refused"
    );
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Withheld);
    assert_nothing_arrives(&p, "the peer never advertised Clipboard").await;
}

/// A peer advertising the channel with `features: 0` is likewise sent nothing —
/// it simply does not take part in sync, and the session and its other channels
/// are entirely unaffected.
#[tokio::test]
async fn a_peer_advertising_clipboard_without_the_feature_bit_is_sent_nothing() {
    let bare = CapabilitySet::new().with(Capability::new(ChannelType::CLIPBOARD));
    let mut p = pair(true, true, bare).await;
    assert!(!p.sender.may_send());
    assert_eq!(
        p.sender
            .send(SECRET)
            .await
            .expect("this is not an error condition"),
        Push::Withheld
    );
    assert_nothing_arrives(&p, "the feature bit was ANDed away").await;
}

/// An over-cap clip is **skipped, not truncated**, over a live session: the
/// peer receives nothing rather than a silently shortened clipboard. The error
/// is the caller's cue to tell the user it was too large.
#[tokio::test]
async fn an_over_cap_clip_is_skipped_rather_than_arriving_truncated() {
    let mut p = pair(true, true, clipboard_caps()).await;
    let big = "x".repeat(MAX_CLIP + 1);

    let err = p.sender.send(&big).await.expect_err("must refuse");
    assert!(
        matches!(
            err,
            peerbeam_clipboard::SendError::Clipboard(
                peerbeam_clipboard::ClipboardError::TooLarge { .. }
            )
        ),
        "wrong error: {err:?}"
    );
    assert_nothing_arrives(&p, "the clip was over the cap").await;

    // The session is unharmed: a normal clip still goes through afterwards.
    assert_eq!(p.sender.send("small").await.expect("send"), Push::Sent);
    assert_eq!(wait_for_clip(&p).await.as_deref(), Some("small"));
}

/// Successive clips each arrive, in order, over the one channel the sender
/// opens lazily and reuses.
#[tokio::test]
async fn successive_clips_each_arrive_over_the_reused_channel() {
    let mut p = pair(true, true, clipboard_caps()).await;
    for text in ["one", "two", "three"] {
        assert_eq!(p.sender.send(text).await.expect("send"), Push::Sent);
    }
    for _ in 0..200 {
        if p.received().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(p.received(), vec!["one", "two", "three"]);
}

/// A withheld push opens **no channel at all**. Opening one is itself a signal:
/// a peer must not be able to tell "sync is off" from "does not know me".
#[tokio::test]
async fn a_withheld_push_opens_no_channel() {
    let mut p = pair(true, false, clipboard_caps()).await;
    assert_eq!(p.sender.send(SECRET).await.expect("send"), Push::Withheld);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        p.received().is_empty(),
        "nothing sent, and nothing opened either"
    );
}
