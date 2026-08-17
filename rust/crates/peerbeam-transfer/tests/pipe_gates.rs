//! The two consent gates on `peerbeam pipe`, proven over a **real
//! two-PeerSession round trip** rather than against the pure predicate alone —
//! plus the properties that make a pipe a pipe: byte-exactness, EOF, and a
//! stream that is never held whole.
//!
//! The distinction from `pipe/gate.rs`'s unit tests is the whole point of this
//! file. Those prove `may_accept_pipe` returns the right answer; they cannot
//! prove the accept path actually *asks* it. These drive `accept_pipe` — the
//! only code that can put a peer's bytes into this process's `out` — against a
//! live peer really sending them, and assert on what landed. Delete the
//! `listening` leg or the trust leg from `may_accept_pipe` and these fail,
//! because the bytes really do arrive.
//!
//! The session wiring mirrors `peerbeam-presence/tests/gates.rs`, with PIPE
//! substituted and registered as a **stream** capability on both sides.

mod common;

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use common::{MemLink, MemTransport};
use futures::io::AsyncWrite;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::DomainError;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link, TrustStore};
use peerbeam_domain::session::{
    Capability, CapabilitySet, ChannelId, ChannelType, PIPE_FEAT_STREAM,
};
use peerbeam_transfer::{
    accept_pipe, caps_support_stream, receive_pipe, send_pipe, send_pipe_on_session, Control,
    Identity, IncomingStreamChannel, PeerSession, PipeConsent, PipeStats, SessionConfig,
    SessionHandle, SessionRole,
};
use peerbeam_trust_fs::FsTrust;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// Chunk size every test here pipes with, so chunk counts are exact.
const CHUNK: u32 = 64 * 1024;

/// Fresh identity, encryption provider and trust store for one endpoint.
/// Copied from `tests/session.rs::security`.
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

/// What a build with `peerbeam pipe` advertises.
fn pipe_caps() -> CapabilitySet {
    CapabilitySet::new().with(Capability::with_features(
        ChannelType::PIPE,
        PIPE_FEAT_STREAM,
    ))
}

/// What a peer from before `peerbeam pipe` advertises: no PIPE at all.
fn legacy_caps() -> CapabilitySet {
    CapabilitySet::new()
        .with(Capability::new(ChannelType::TRANSFER))
        .with(Capability::new(ChannelType::CHAT))
}

/// A control frame, built the way the wire builds one.
fn control(c: &Control) -> Frame {
    Frame {
        kind: FrameKind::Control,
        payload: Bytes::from(serde_json::to_vec(c).expect("Control is serializable")),
    }
}

fn chunk(bytes: &[u8]) -> Frame {
    Frame {
        kind: FrameKind::Chunk,
        payload: Bytes::copy_from_slice(bytes),
    }
}

/// An `AsyncWrite` that records **every individual write** as well as the bytes.
///
/// The write log is what makes "nothing was buffered whole" assertable: a
/// receiver that accumulated a 4 MiB stream and wrote it at the end would show
/// one enormous write here, whatever the byte total said.
#[derive(Clone, Default)]
struct RecordingSink {
    writes: Arc<Mutex<Vec<usize>>>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl RecordingSink {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().expect("bytes").clone()
    }
    fn writes(&self) -> Vec<usize> {
        self.writes.lock().expect("writes").clone()
    }
}

impl AsyncWrite for RecordingSink {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.writes.lock().expect("writes").push(buf.len());
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A payload no line-oriented or UTF-8-assuming code path could carry intact:
/// embedded NULs, lone continuation bytes, bare CR and LF, `0xFF`, `0xFE`.
fn hostile_payload(size: usize) -> Vec<u8> {
    let seed: [u8; 11] = [
        0x00, 0xFF, b'\n', 0x80, b'\r', 0xC3, 0x00, 0xFE, b'\n', 0x1B, 0x7F,
    ];
    (0..size)
        .map(|i| seed[i % seed.len()] ^ (i as u8))
        .collect()
}

/// Two live PeerSessions: A pipes, B accepts (or refuses).
struct Pair {
    a: SessionHandle,
    b: SessionHandle,
    b_incoming: UnboundedReceiver<IncomingStreamChannel>,
    /// B's trust store, so a test can revoke A mid-session.
    b_trust: Arc<FsTrust>,
    /// The id B authenticated A as.
    a_id: DeviceId,
    /// B's **negotiated** (intersected) capability set — what the gate is asked
    /// about, so a legacy A really does AND the capability away here.
    b_negotiated: CapabilitySet,
    /// A's negotiated set, for the sender-side capability check.
    a_negotiated: CapabilitySet,
}

/// Stand up A↔B over the in-memory transport, with PIPE advertised as a stream
/// capability on both sides. `a_advertises` lets a test play a legacy sender.
async fn pair(a_advertises: CapabilitySet) -> Pair {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, _a_ev_rx) = unbounded_channel();
    let (b_ev, _b_ev_rx) = unbounded_channel();
    let (a_ch, _a_ch_rx) = unbounded_channel();
    let (b_ch, _b_ch_rx) = unbounded_channel();
    let (a_in, _a_in_rx) = unbounded_channel();
    let (b_in, b_incoming) = unbounded_channel();

    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let a_id = id_a.device_id.clone();

    let a_cfg = SessionConfig::new(a_advertises).with_stream_channel_type(ChannelType::PIPE);
    let b_cfg = SessionConfig::new(pipe_caps()).with_stream_channel_type(ChannelType::PIPE);

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
        trust_a as Arc<dyn TrustStore>,
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
        trust_b.clone() as Arc<dyn TrustStore>,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator opens");
    let mut b = rb.expect("responder opens");
    let (a_handle, b_handle) = (a.handle(), b.handle());
    let a_negotiated = a.capabilities().clone();
    let b_negotiated = b.capabilities().clone();
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });

    // The handshake has TOFU-pinned A in B's store; the untrusted case below
    // revokes it, which is the shape that actually occurs in production (a user
    // removing a device while a session is live).
    assert!(
        trust_b.is_trusted(&a_id),
        "the handshake must pin A, or the trusted case proves nothing"
    );

    Pair {
        a: a_handle,
        b: b_handle,
        b_incoming,
        b_trust: trust_b,
        a_id,
        a_negotiated,
        b_negotiated,
    }
}

/// What one attempted pipe produced on both ends.
struct Attempt {
    sent: Result<PipeStats, DomainError>,
    received: Result<PipeStats, DomainError>,
    /// Exactly what reached B's `out`, and in how many writes.
    sink: RecordingSink,
}

/// Run one pipe A→B with B's consent as given, and report both ends.
async fn attempt(
    p: &mut Pair,
    listening: bool,
    only_from: Option<DeviceId>,
    payload: &[u8],
) -> Attempt {
    // Destructured so the two halves below borrow disjoint fields.
    let Pair {
        a,
        b,
        b_incoming,
        b_trust,
        a_id,
        b_negotiated,
        ..
    } = p;
    let sink = RecordingSink::default();
    let mut src = futures::io::Cursor::new(payload.to_vec());

    let send = send_pipe_on_session(a, &mut src, CHUNK);
    let recv = async {
        let incoming = b_incoming
            .recv()
            .await
            .ok_or_else(|| DomainError::Connection("no incoming channel".into()))?;
        let consent = PipeConsent {
            listening,
            trust: b_trust.as_ref(),
            only_from: only_from.as_ref(),
            negotiated: b_negotiated,
        };
        let mut out = sink.clone();
        accept_pipe(incoming, b, a_id, &consent, &mut out).await
    };
    let (sent, received) = tokio::join!(send, recv);
    Attempt {
        sent,
        received,
        sink,
    }
}

// ── The baseline ────────────────────────────────────────────────────────────

/// Without this passing, every negative test below is vacuous — they would all
/// "pass" against a build that never delivered anything.
///
/// It is also the byte-exactness test: the payload is deliberately not text.
#[tokio::test]
async fn a_listening_trusted_peer_receives_the_stream_byte_for_byte() {
    let mut p = pair(pipe_caps()).await;
    let payload = hostile_payload(200_000);
    assert!(
        payload.contains(&0),
        "the fixture must actually contain a NUL or it proves nothing"
    );
    assert!(
        String::from_utf8(payload.clone()).is_err(),
        "the fixture must actually be invalid UTF-8 or it proves nothing"
    );

    let a = attempt(&mut p, true, None, &payload).await;
    let sent = a.sent.expect("send");
    let received = a.received.expect("receive");
    assert_eq!(sent.bytes, payload.len() as u64);
    assert_eq!(received.bytes, payload.len() as u64);
    assert_eq!(
        a.sink.bytes(),
        payload,
        "the stream must arrive byte-identical, NULs and invalid UTF-8 included"
    );
}

/// **EOF is the terminator.** An empty stdin is a complete, successful stream of
/// nothing — not an error and not a hang — and the receiver finishes rather than
/// waiting for bytes that will never come.
#[tokio::test]
async fn an_empty_stream_completes_cleanly() {
    let mut p = pair(pipe_caps()).await;
    let a = attempt(&mut p, true, None, &[]).await;
    assert_eq!(
        a.sent.expect("send"),
        PipeStats {
            bytes: 0,
            chunks: 0
        }
    );
    assert_eq!(
        a.received.expect("receive"),
        PipeStats {
            bytes: 0,
            chunks: 0
        }
    );
    assert!(a.sink.bytes().is_empty());
}

// ── The gates ───────────────────────────────────────────────────────────────

/// **The listen gate.** A `receive`/`daemon`-style session — one that advertises
/// the capability, accepts the channel, and simply is not `pipe --listen` —
/// refuses, and **not one byte** reaches its `out`.
///
/// This is the gate that stops a background daemon becoming a remote write to
/// whatever terminal it was started from.
///
/// Mutation target: make the `listening` leg of `may_accept_pipe` unconditional
/// (delete it, or `true &&`) and this test fails — the peer's bytes land in the
/// sink.
#[tokio::test]
async fn a_daemon_style_session_refuses_a_pipe_and_writes_nothing() {
    let mut p = pair(pipe_caps()).await;
    let payload = hostile_payload(100_000);
    let a = attempt(&mut p, false, None, &payload).await;

    let msg = a
        .received
        .expect_err("a non-listening process must refuse")
        .to_string();
    assert!(
        msg.contains("pipe --listen"),
        "the refusal must say why: {msg}"
    );
    assert!(
        a.sink.bytes().is_empty(),
        "a refused pipe must not write a single byte to stdout"
    );
    assert!(
        a.sink.writes().is_empty(),
        "and must not even attempt a write"
    );
    assert!(
        a.sent.is_err(),
        "the sender must learn its pipe went nowhere rather than exiting 0"
    );
}

/// **The trust gate.** A peer whose pin was revoked is refused *even by a real
/// `pipe --listen`*, and writes nothing.
///
/// Mutation target: delete `trust.is_trusted(peer)` from `may_accept_pipe` and
/// this test fails.
#[tokio::test]
async fn an_untrusted_peer_is_refused_even_while_listening() {
    let mut p = pair(pipe_caps()).await;
    assert!(
        p.b_trust.remove(&p.a_id).expect("revoke"),
        "revoking must actually remove a pin that was there"
    );
    assert!(
        !p.b_trust.is_trusted(&p.a_id),
        "A must really be untrusted — a test that cannot tell proves nothing"
    );

    let a = attempt(&mut p, true, None, &hostile_payload(100_000)).await;
    let msg = a
        .received
        .expect_err("untrusted must be refused")
        .to_string();
    assert!(msg.contains("not trusted"), "{msg}");
    assert!(
        a.sink.bytes().is_empty(),
        "an untrusted peer's bytes must never reach stdout"
    );
}

/// `--from` restricts the listener to one device: another peer — *trusted*, and
/// therefore already past the trust leg — is still refused.
#[tokio::test]
async fn from_refuses_a_different_trusted_peer() {
    let mut p = pair(pipe_caps()).await;
    assert!(
        p.b_trust.is_trusted(&p.a_id),
        "A is trusted, so only --from can be what refuses it"
    );
    let someone_else = DeviceId::from("pb-someone-else");
    let a = attempt(&mut p, true, Some(someone_else), &hostile_payload(64_000)).await;
    let msg = a.received.expect_err("--from must refuse").to_string();
    assert!(msg.contains("--from"), "{msg}");
    assert!(a.sink.bytes().is_empty());
}

/// ...and the *named* device is accepted, so `--from` is a filter rather than a
/// refusal of everything.
#[tokio::test]
async fn from_accepts_the_named_device() {
    let mut p = pair(pipe_caps()).await;
    let named = p.a_id.clone();
    let payload = hostile_payload(64_000);
    let a = attempt(&mut p, true, Some(named), &payload).await;
    a.received.expect("the named device is accepted");
    assert_eq!(a.sink.bytes(), payload);
}

/// A peer that predates `peerbeam pipe` never advertises PIPE, so the
/// intersection drops it: the sender's own check refuses **before it reads a
/// byte of stdin**, and the session layer refuses to open the channel at all, so
/// there is no path that silently half-works.
#[tokio::test]
async fn a_peer_without_the_feature_bit_is_refused_rather_than_hanging() {
    let p = pair(legacy_caps()).await;
    assert!(
        !caps_support_stream(&p.a_negotiated),
        "a legacy peer must negotiate the pipe capability away"
    );
    assert!(!p.a_negotiated.supports(ChannelType::PIPE));

    let mut src = futures::io::Cursor::new(vec![1u8, 2, 3]);
    let err = send_pipe_on_session(&p.a, &mut src, CHUNK)
        .await
        .expect_err("an unnegotiated capability cannot open a channel");
    assert!(matches!(err, DomainError::Connection(_)), "{err:?}");
}

/// A stream channel arrives carrying its own claim about what it is. One that is
/// not a pipe is refused rather than written out — a file transfer's opening
/// `Meta` frame must never be dumped to somebody's terminal.
#[tokio::test]
async fn a_channel_that_is_not_a_pipe_is_refused() {
    let p = pair(pipe_caps()).await;
    let sink = RecordingSink::default();
    let (mine, _theirs) = MemLink::pair(1);
    let incoming = IncomingStreamChannel {
        channel: ChannelId::new(9),
        channel_type: ChannelType::TRANSFER,
        link: Box::new(mine) as Box<dyn Link>,
    };
    let consent = PipeConsent {
        listening: true,
        trust: p.b_trust.as_ref(),
        only_from: None,
        negotiated: &p.b_negotiated,
    };
    let mut out = sink.clone();
    let err = accept_pipe(incoming, &p.b, &p.a_id, &consent, &mut out)
        .await
        .expect_err("a mislabelled channel must be refused");
    assert!(err.to_string().contains("0x0100"), "{err}");
    assert!(sink.bytes().is_empty());
}

// ── Streaming, not buffering (I10) ──────────────────────────────────────────

/// **Nothing is held whole, in either direction.** Three independent proofs on
/// one 4 MiB stream:
///
/// 1. **Chunk accounting.** Both ends report exactly `ceil(bytes / CHUNK)`
///    frames. A run that buffered the stream and sent it in one go would report
///    `chunks == 1`.
/// 2. **Write granularity.** The receiver performed one bounded write per chunk
///    and never a single 4 MiB one, so it wrote through rather than accumulating
///    — the property no byte total can show.
/// 3. **Liveness under a bounded transport.** `MemTransport`'s streams hold 32
///    frames; this stream is 64 of them. It can only complete if both ends run
///    concurrently, chunk by chunk — an end that tried to consume or produce the
///    whole stream first would deadlock on that bound rather than fail an
///    assertion.
///
/// Deliberately not an RSS measurement: those are noisy, allocator-dependent and
/// prove nothing about *where* the bytes went.
#[tokio::test]
async fn a_large_stream_moves_in_bounded_chunks_and_is_never_accumulated() {
    let mut p = pair(pipe_caps()).await;
    let size = 4 * 1024 * 1024;
    let payload = hostile_payload(size);
    let expected_chunks = (size as u64).div_ceil(CHUNK as u64);
    assert_eq!(
        expected_chunks, 64,
        "64 frames through a 32-frame transport"
    );

    let a = attempt(&mut p, true, None, &payload).await;
    let sent = a.sent.expect("send");
    let received = a.received.expect("receive");

    assert_eq!(sent.chunks, expected_chunks, "the sender framed in chunks");
    assert_eq!(
        received.chunks, expected_chunks,
        "and the receiver saw them all"
    );
    assert_eq!(sent.bytes, size as u64);
    assert_eq!(a.sink.bytes(), payload, "byte-identical at 4 MiB too");

    let writes = a.sink.writes();
    assert_eq!(writes.len() as u64, expected_chunks, "one write per chunk");
    let biggest = writes.iter().copied().max().unwrap_or(0);
    assert!(
        biggest <= CHUNK as usize,
        "peak single write was {biggest} bytes — the receiver accumulated"
    );
}

// ── Broken pipes ────────────────────────────────────────────────────────────

/// A sender that dies mid-stream must fail the receiver, **not** look like a
/// clean end. Anything else means `peerbeam pipe --listen > project.tgz` exits
/// `0` on a truncated archive.
///
/// The bytes that did arrive are already out — a pipe cannot un-write stdout —
/// so the error is the whole report, and the exit code is how a script learns.
#[tokio::test]
async fn a_sender_that_dies_mid_stream_fails_the_receiver_rather_than_truncating_silently() {
    let (mut a, mut b) = MemLink::pair(4);
    let half = vec![7u8; 1024];
    a.send_frame(chunk(&half)).await.expect("first chunk");
    drop(a); // the sending process dies before `Complete`

    let sink = RecordingSink::default();
    let mut out = sink.clone();
    let err = receive_pipe(&mut b, &mut out)
        .await
        .expect_err("a truncated stream must be an error, never a clean end");
    assert!(matches!(err, DomainError::Transfer(_)), "{err:?}");
    assert!(err.to_string().contains("incomplete"), "{err}");
    assert_eq!(
        sink.bytes(),
        half,
        "what did arrive was still written through — the error reports the rest"
    );
}

/// The other direction: a receiver that goes away mid-stream fails the sender
/// cleanly — an error, no panic, and no wedged task.
#[tokio::test]
async fn a_receiver_that_goes_away_fails_the_sender_without_panicking() {
    let (mut a, b) = MemLink::pair(1);
    drop(b);
    let mut src = futures::io::Cursor::new(hostile_payload(256 * 1024));
    let err = send_pipe(&mut a, &mut src, CHUNK)
        .await
        .expect_err("writing into a dead pipe must fail");
    assert!(matches!(err, DomainError::Connection(_)), "{err:?}");
}

/// A corrupted stream is reported rather than passed off as complete. The bytes
/// are already out by then, so the *error* is the report — which is why the CLI
/// maps it to a non-zero exit code.
#[tokio::test]
async fn a_checksum_mismatch_is_reported_rather_than_accepted() {
    let (mut a, mut b) = MemLink::pair(8);
    a.send_frame(chunk(b"the bytes that really arrived"))
        .await
        .expect("chunk");
    a.send_frame(control(&Control::Complete {
        checksum: "0".repeat(64),
    }))
    .await
    .expect("bogus complete");

    let sink = RecordingSink::default();
    let mut out = sink.clone();
    let err = receive_pipe(&mut b, &mut out)
        .await
        .expect_err("a mismatched checksum must fail");
    assert!(matches!(err, DomainError::Integrity(_)), "{err:?}");
}

/// A pipe negotiates no resume and signals no pause, so those control frames are
/// skipped rather than treated as data or as an error (§6's fail-safe rule) —
/// and, critically, they are **never written to stdout**.
#[tokio::test]
async fn unexpected_control_frames_are_skipped_not_written_out() {
    let (mut a, mut b) = MemLink::pair(8);
    a.send_frame(control(&Control::Pause)).await.expect("pause");
    a.send_frame(chunk(b"data")).await.expect("chunk");
    a.send_frame(control(&Control::ResumeAck { offset: 99 }))
        .await
        .expect("resume ack");
    let digest = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"data");
        h.finalize().iter().fold(String::new(), |mut s, byte| {
            use std::fmt::Write;
            let _ = write!(s, "{byte:02x}");
            s
        })
    };
    a.send_frame(control(&Control::Complete { checksum: digest }))
        .await
        .expect("complete");

    let sink = RecordingSink::default();
    let mut out = sink.clone();
    let stats = receive_pipe(&mut b, &mut out).await.expect("receive");
    assert_eq!(stats.bytes, 4);
    assert_eq!(sink.bytes(), b"data", "only chunk payloads reach stdout");
}
