//! Mutual authentication, replay/tamper protection, and end-to-end secure
//! transfer over an in-memory link.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link, TrustStore};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file, send_file, Identity, SecureLink, SendRequest, TransferControl,
    TransferOutcome,
};
use peerbeam_trust_fs::FsTrust;

// ── In-memory + adversarial links ───────────────────────────────

struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl MemLink {
    fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a_tx, b_rx) = mpsc::channel(cap);
        let (b_tx, a_rx) = mpsc::channel(cap);
        (
            MemLink { tx: a_tx, rx: a_rx },
            MemLink { tx: b_tx, rx: b_rx },
        )
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

/// Replays the previously-received frame once on the following `recv`.
struct DuplicatingLink {
    inner: MemLink,
    pending: Option<Frame>,
    replayed: bool,
}

#[async_trait]
impl Link for DuplicatingLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        if !self.replayed {
            if let Some(f) = self.pending.clone() {
                self.replayed = true;
                return Ok(Some(f)); // replay the last frame
            }
        }
        let f = self.inner.recv_frame().await?;
        self.pending = f.clone();
        Ok(f)
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

/// Flips the last byte of every frame it delivers.
struct TamperingLink {
    inner: MemLink,
}

#[async_trait]
impl Link for TamperingLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let mut f = self.inner.recv_frame().await?;
        if let Some(frame) = &mut f {
            if !frame.payload.is_empty() {
                let mut d = frame.payload.to_vec();
                *d.last_mut().unwrap() ^= 0x01;
                frame.payload = Bytes::from(d);
            }
        }
        Ok(f)
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

/// On-path attacker: rewrites the cleartext `device_id`/`name` fields of the
/// first `Hello` handshake frame it relays, leaving `pubkey`/`nonce`
/// untouched — simulating an in-flight rebind attempt to an arbitrary
/// identity (see auth.rs's transcript doc comment).
struct RewriteHelloIdentityLink {
    inner: MemLink,
    rewritten: bool,
}

#[async_trait]
impl Link for RewriteHelloIdentityLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        let frame = self.inner.recv_frame().await?;
        let Some(mut frame) = frame else {
            return Ok(None);
        };
        if !self.rewritten && frame.kind == FrameKind::Handshake {
            let mut msg: serde_json::Value = serde_json::from_slice(&frame.payload)
                .expect("handshake frame is valid JSON");
            if let Some(hello) = msg.get_mut("Hello") {
                // Leave pubkey/nonce untouched; rebind the presented identity.
                hello["device_id"] = serde_json::json!("mallory");
                hello["name"] = serde_json::json!("Trusted Laptop");
                self.rewritten = true;
                frame.payload = Bytes::from(serde_json::to_vec(&msg).unwrap());
            }
        }
        Ok(Some(frame))
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn identity(enc: &AeadCrypto, id: &str, name: &str) -> Identity {
    Identity {
        device_id: DeviceId::from(id),
        name: name.to_string(),
        keypair: enc.generate_keypair(),
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn mutual_authentication_pins_trust_and_transfers() {
    let dir = tempfile::tempdir().unwrap();
    let enc = AeadCrypto::new();
    let id_a = identity(&enc, "A", "Alice");
    let id_b = identity(&enc, "B", "Bob");
    let ta = FsTrust::open(dir.path().join("a-trust.json")).unwrap();
    let tb = FsTrust::open(dir.path().join("b-trust.json")).unwrap();

    let (mut la, mut lb) = MemLink::pair(16);
    let (sa, sb) = tokio::join!(
        authenticate(&mut la, &id_a, &enc, &ta),
        authenticate(&mut lb, &id_b, &enc, &tb),
    );
    let sa = sa.expect("A authenticates");
    let sb = sb.expect("B authenticates");

    assert_eq!(sa.peer_id, DeviceId::from("B"));
    assert_eq!(sb.peer_id, DeviceId::from("A"));
    // The session must carry the peer's human name (not just the id), so the
    // receiver can display "from Bob" instead of the raw device id.
    assert_eq!(sa.peer_name, "Bob");
    assert_eq!(sb.peer_name, "Alice");
    assert!(sa.newly_trusted && sb.newly_trusted);
    // Each side pinned the other.
    assert!(ta.lookup(&DeviceId::from("B")).unwrap().is_some());
    assert!(tb.lookup(&DeviceId::from("A")).unwrap().is_some());

    // Now run a real file transfer over the authenticated secure links.
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    let bytes: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let out_str = out.to_string_lossy().to_string();

    let storage = FsStorage::new();
    let mut seca = SecureLink::new(&mut la, &enc, sa);
    let mut secb = SecureLink::new(&mut lb, &enc, sb);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "sec-1".into(),
        name: "f.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes.len() as u64,
        chunk_size: 64 * 1024,
    };
    let send = send_file(&mut seca, &storage, req, &cs, &ptx, 3);
    let recv = receive_file(&mut secb, &storage, &out_str, &cr, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    assert_eq!(rr.unwrap().outcome, TransferOutcome::Completed);
    assert_eq!(std::fs::read(out.join("f.bin")).unwrap(), bytes);
}

#[tokio::test]
async fn tofu_pins_then_trusts_then_rejects_key_change() {
    let dir = tempfile::tempdir().unwrap();
    let enc = AeadCrypto::new();
    let id_a = identity(&enc, "A", "Alice");
    let ta = FsTrust::open(dir.path().join("a-trust.json")).unwrap();

    // One handshake from A's perspective against a given B identity.
    async fn once(
        enc: &AeadCrypto,
        id_a: &Identity,
        ta: &FsTrust,
        id_b: &Identity,
    ) -> Result<peerbeam_transfer::Session> {
        let (mut la, mut lb) = MemLink::pair(16);
        let tb_dir = tempfile::tempdir().unwrap();
        let tb = FsTrust::open(tb_dir.path().join("t.json")).unwrap();
        let (ra, _rb) = tokio::join!(
            authenticate(&mut la, id_a, enc, ta),
            authenticate(&mut lb, id_b, enc, &tb),
        );
        // keep tb_dir alive until both finish
        drop(tb_dir);
        ra
    }

    let b_key = enc.generate_keypair();
    let id_b1 = Identity {
        device_id: DeviceId::from("B"),
        name: "Bob".into(),
        keypair: b_key.clone(),
    };

    // First contact → pinned.
    let s1 = once(&enc, &id_a, &ta, &id_b1).await.unwrap();
    assert!(s1.newly_trusted);

    // Same key again → already trusted.
    let s2 = once(&enc, &id_a, &ta, &id_b1).await.unwrap();
    assert!(!s2.newly_trusted);

    // Same device id, DIFFERENT key → rejected (possible MITM).
    let id_b2 = Identity {
        device_id: DeviceId::from("B"),
        name: "Bob".into(),
        keypair: enc.generate_keypair(),
    };
    let s3 = once(&enc, &id_a, &ta, &id_b2).await;
    assert!(
        matches!(s3, Err(DomainError::Encryption(_))),
        "key change must be rejected"
    );
}

/// An on-path attacker who rewrites A's cleartext `device_id`/`name` in its
/// Hello frame — but leaves `pubkey`/`nonce` untouched — must not be able to
/// rebind A's genuine key to an attacker-chosen identity ("mallory" /
/// "Trusted Laptop"). Before the transcript bound identity, the Confirm MAC
/// verified anyway (it only covered pubkey+nonce) and B would silently pin
/// A's real key under the bogus device_id. Now the transcripts diverge, so
/// key confirmation must fail on both sides and nothing gets pinned.
#[tokio::test]
async fn mitm_rebind_of_identity_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let enc = AeadCrypto::new();
    let id_a = identity(&enc, "A", "Alice");
    let id_b = identity(&enc, "B", "Bob");
    let ta = FsTrust::open(dir.path().join("a-trust.json")).unwrap();
    let tb = FsTrust::open(dir.path().join("b-trust.json")).unwrap();

    let (mut la, lb) = MemLink::pair(16);
    let mut lb = RewriteHelloIdentityLink {
        inner: lb,
        rewritten: false,
    };

    let (ra, rb) = tokio::join!(
        authenticate(&mut la, &id_a, &enc, &ta),
        authenticate(&mut lb, &id_b, &enc, &tb),
    );

    assert!(
        matches!(ra, Err(DomainError::Encryption(_))),
        "A must detect key-confirmation mismatch"
    );
    assert!(
        matches!(rb, Err(DomainError::Encryption(_))),
        "B must detect key-confirmation mismatch"
    );
    // Neither the real nor the attacker-chosen identity got pinned.
    assert!(ta.lookup(&DeviceId::from("B")).unwrap().is_none());
    assert!(tb.lookup(&DeviceId::from("A")).unwrap().is_none());
    assert!(tb.lookup(&DeviceId::from("mallory")).unwrap().is_none());
}

async fn established(
    enc: &AeadCrypto,
) -> (
    peerbeam_transfer::Session,
    peerbeam_transfer::Session,
    MemLink,
    MemLink,
) {
    let dir = tempfile::tempdir().unwrap();
    let ta = FsTrust::open(dir.path().join("a.json")).unwrap();
    let tb = FsTrust::open(dir.path().join("b.json")).unwrap();
    let id_a = identity(enc, "A", "Alice");
    let id_b = identity(enc, "B", "Bob");
    let (mut la, mut lb) = MemLink::pair(16);
    let (sa, sb) = tokio::join!(
        authenticate(&mut la, &id_a, enc, &ta),
        authenticate(&mut lb, &id_b, enc, &tb),
    );
    drop(dir);
    (sa.unwrap(), sb.unwrap(), la, lb)
}

#[tokio::test]
async fn secure_link_rejects_replayed_frame() {
    let enc = AeadCrypto::new();
    let (sa, sb, mut la, lb) = established(&enc).await;

    let mut sender = SecureLink::new(&mut la, &enc, sa);
    let mut dup = DuplicatingLink {
        inner: lb,
        pending: None,
        replayed: false,
    };
    let mut receiver = SecureLink::new(&mut dup, &enc, sb);

    sender
        .send_frame(Frame {
            kind: FrameKind::Meta,
            payload: Bytes::from_static(b"hello"),
        })
        .await
        .unwrap();

    // First delivery opens fine.
    let first = receiver.recv_frame().await.unwrap().unwrap();
    assert_eq!(first.kind, FrameKind::Meta);
    assert_eq!(&first.payload[..], b"hello");

    // The duplicated (replayed) frame carries a stale counter → rejected.
    let replayed = receiver.recv_frame().await;
    assert!(
        matches!(replayed, Err(DomainError::Integrity(_))),
        "replay must be rejected, got {replayed:?}"
    );
}

#[tokio::test]
async fn secure_link_rejects_tampered_frame() {
    let enc = AeadCrypto::new();
    let (sa, sb, mut la, lb) = established(&enc).await;

    let mut sender = SecureLink::new(&mut la, &enc, sa);
    let mut tamper = TamperingLink { inner: lb };
    let mut receiver = SecureLink::new(&mut tamper, &enc, sb);

    sender
        .send_frame(Frame {
            kind: FrameKind::Chunk,
            payload: Bytes::from_static(b"important data"),
        })
        .await
        .unwrap();

    let got = receiver.recv_frame().await;
    assert!(got.is_err(), "tampered frame must be rejected, got {got:?}");
}
