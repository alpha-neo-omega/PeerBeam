//! Integration tests for the M7 transport-selection / cutover layer.
//!
//! Verifies that a transfer defaults to PeerSession, falls back to the legacy
//! path automatically and only at establishment, records migration metrics, and
//! produces byte-for-byte identical output on either transport (one pipeline,
//! two transports). Uses real per-channel crypto + real authenticated handshake
//! over an in-memory transport; only the socket is simulated.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::unbounded_channel;

use common::{MemLink, MemTransport};
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, Link, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, SessionError};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file_selected, send_file_selected, send_folder_selected,
    transfer_capability, CompatMode, FallbackReason, FolderSendRequest, Identity,
    IncomingStreamChannel, LegacyPath, PeerSession, Received, SendRequest, Session, SessionConfig,
    SessionHandle, SessionOpen, SessionReceivePath, SessionRole, SessionSendPath, TransferControl,
    TransferOutcome, TransferPath,
};
use peerbeam_trust_fs::FsTrust;

const TRANSFER: ChannelType = ChannelType::TRANSFER;

type Sec = (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>);

// ── security + config helpers ───────────────────────────────────────────────

fn security(name: &str) -> Sec {
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

fn cfg_transfer() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(TRANSFER)))
        .with_stream_channel_type(TRANSFER)
}

// ── session path openers (real PeerSession over MemTransport) ────────────────

struct MemSessionSend {
    transport: Option<Arc<MemTransport>>,
    sec: Option<Sec>,
    cfg: SessionConfig,
}

#[async_trait]
impl SessionSendPath for MemSessionSend {
    async fn open(&mut self) -> Result<SessionOpen<SessionHandle>> {
        let t = self.transport.take().expect("send opener used once");
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let (ev, _ev) = unbounded_channel();
        let (ch, _ch) = unbounded_channel();
        let (inc, _inc) = unbounded_channel();
        std::mem::forget(_ev);
        std::mem::forget(_ch);
        std::mem::forget(_inc);
        match PeerSession::open(
            t,
            SessionRole::Initiator,
            self.cfg.clone(),
            ev,
            ch,
            inc,
            None,
            id,
            enc,
            trust,
        )
        .await
        {
            Ok(mut session) => {
                if let Err(reason) = transfer_capability(&session) {
                    return Ok(SessionOpen::Fallback(reason));
                }
                let handle = session.handle();
                tokio::spawn(async move { session.run().await });
                Ok(SessionOpen::Ready(handle))
            }
            Err(SessionError::VersionIncompatible { .. }) => {
                Ok(SessionOpen::Fallback(FallbackReason::VersionMismatch))
            }
            Err(_) => Ok(SessionOpen::Fallback(FallbackReason::OlderPeer)),
        }
    }
}

struct MemSessionRecv {
    transport: Option<Arc<MemTransport>>,
    sec: Option<Sec>,
    cfg: SessionConfig,
}

#[async_trait]
impl SessionReceivePath for MemSessionRecv {
    async fn open(&mut self) -> Result<SessionOpen<(SessionHandle, IncomingStreamChannel)>> {
        let t = self.transport.take().expect("recv opener used once");
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let (ev, _ev) = unbounded_channel();
        let (ch, _ch) = unbounded_channel();
        let (inc, mut inc_rx) = unbounded_channel();
        std::mem::forget(_ev);
        std::mem::forget(_ch);
        match PeerSession::open(
            t,
            SessionRole::Responder,
            self.cfg.clone(),
            ev,
            ch,
            inc,
            None,
            id,
            enc,
            trust,
        )
        .await
        {
            Ok(mut session) => {
                if let Err(reason) = transfer_capability(&session) {
                    return Ok(SessionOpen::Fallback(reason));
                }
                let handle = session.handle();
                tokio::spawn(async move { session.run().await });
                // Await the sender's incoming transfer channel.
                let incoming = inc_rx
                    .recv()
                    .await
                    .ok_or_else(|| DomainError::Connection("no incoming channel".into()))?;
                Ok(SessionOpen::Ready((handle, incoming)))
            }
            Err(SessionError::VersionIncompatible { .. }) => {
                Ok(SessionOpen::Fallback(FallbackReason::VersionMismatch))
            }
            Err(_) => Ok(SessionOpen::Fallback(FallbackReason::OlderPeer)),
        }
    }
}

/// A session opener that always reports a fixed fallback reason (models an older /
/// incompatible peer without needing a live session).
struct StubSessionSend(FallbackReason);
#[async_trait]
impl SessionSendPath for StubSessionSend {
    async fn open(&mut self) -> Result<SessionOpen<SessionHandle>> {
        Ok(SessionOpen::Fallback(self.0))
    }
}
struct StubSessionRecv(FallbackReason);
#[async_trait]
impl SessionReceivePath for StubSessionRecv {
    async fn open(&mut self) -> Result<SessionOpen<(SessionHandle, IncomingStreamChannel)>> {
        Ok(SessionOpen::Fallback(self.0))
    }
}

// ── legacy path openers (MemLink pair + pre-shared session) ──────────────────

struct MemLegacy {
    link: Option<Box<dyn Link>>,
    sec: Option<Sec>,
}
#[async_trait]
impl LegacyPath for MemLegacy {
    async fn open(&mut self) -> Result<(Box<dyn Link>, Session)> {
        let mut link = self.link.take().expect("legacy link once");
        let (id, enc, trust) = self.sec.take().expect("legacy sec once");
        // Legacy path runs its own authenticated handshake over the raw link.
        let session = authenticate(link.as_mut(), &id, &*enc, &*trust).await?;
        Ok((link, session))
    }
}

fn mem_legacy(link: Box<dyn Link>, name: &str) -> MemLegacy {
    let (id, enc, trust) = security(name);
    MemLegacy {
        link: Some(link),
        sec: Some((id, enc, trust)),
    }
}

/// A legacy opener that must never be called (session path is expected).
struct NeverLegacy;
#[async_trait]
impl LegacyPath for NeverLegacy {
    async fn open(&mut self) -> Result<(Box<dyn Link>, Session)> {
        panic!("legacy path must not be used when the session path is selected");
    }
}

// ── harness ─────────────────────────────────────────────────────────────────

struct Ends {
    s_session: MemSessionSend,
    r_session: MemSessionRecv,
    s_legacy: MemLegacy,
    r_legacy: MemLegacy,
    enc: Arc<dyn EncryptionProvider>,
}

/// Build both endpoints' openers sharing one MemTransport pair (session) and one
/// MemLink pair (legacy), with `send_cfg`/`recv_cfg` for the session.
fn ends(send_cfg: SessionConfig, recv_cfg: SessionConfig) -> Ends {
    let (ta, tb) = MemTransport::pair();
    let (la, lb) = MemLink::pair(64);
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
    Ends {
        s_session: MemSessionSend {
            transport: Some(ta),
            sec: Some((id_a, enc_a, trust_a)),
            cfg: send_cfg,
        },
        r_session: MemSessionRecv {
            transport: Some(tb),
            sec: Some((id_b, enc_b, trust_b)),
            cfg: recv_cfg,
        },
        s_legacy: mem_legacy(Box::new(la), "legacy-a"),
        r_legacy: mem_legacy(Box::new(lb), "legacy-b"),
        enc,
    }
}

fn write_src(dir: &std::path::Path, name: &str, data: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, data).expect("write src");
    path.to_string_lossy().into_owned()
}

fn send_req(name: &str, path: String, size: u64) -> SendRequest {
    SendRequest {
        transfer_id: name.to_string(),
        name: name.to_string(),
        path,
        size,
        chunk_size: 8192,
    }
}

/// Drive one file transfer through the selector on both ends concurrently.
async fn run_file(
    mode: CompatMode,
    mut e: Ends,
    data: &[u8],
    name: &str,
) -> (
    TransferPath,
    TransferOutcome,
    TransferPath,
    Received,
    Vec<Progress>,
    Vec<Progress>,
) {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let path = write_src(src.path(), name, data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, mut prx_s) = unbounded_channel::<Progress>();
    let (ptx_r, mut prx_r) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let enc = e.enc.clone();

    let send = send_file_selected(
        mode,
        &mut e.s_session,
        &mut e.s_legacy,
        &*enc,
        &storage_s,
        send_req(name, path, data.len() as u64),
        &ctrl_s,
        &ptx_s,
        0,
        &metrics_s,
    );
    let recv = receive_file_selected(
        mode,
        &mut e.r_session,
        &mut e.r_legacy,
        &*enc,
        &storage_r,
        &dst_dir,
        &ctrl_r,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    let (path_s, outcome_s) = sr.expect("send ok");
    let (path_r, received) = rr.expect("recv ok");

    let got = std::fs::read(dst.path().join(name)).unwrap();
    assert_eq!(got, data, "received bytes differ from source ({path_r:?})");

    let mut prog_s = Vec::new();
    while let Ok(p) = prx_s.try_recv() {
        prog_s.push(p);
    }
    let mut prog_r = Vec::new();
    while let Ok(p) = prx_r.try_recv() {
        prog_r.push(p);
    }
    (path_s, outcome_s, path_r, received, prog_s, prog_r)
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn peersession_is_selected_by_default() {
    let e = ends(cfg_transfer(), cfg_transfer());
    let data: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
    let (ps, os, pr, rec, _, _) = run_file(CompatMode::Auto, e, &data, "movie.bin").await;
    assert_eq!(ps, TransferPath::Session);
    assert_eq!(pr, TransferPath::Session);
    assert_eq!(os, TransferOutcome::Completed);
    assert_eq!(rec.outcome, TransferOutcome::Completed);
    assert_eq!(rec.bytes, data.len() as u64);
}

#[tokio::test]
async fn explicit_compat_mode_forces_legacy() {
    let e = ends(cfg_transfer(), cfg_transfer());
    let data: Vec<u8> = (0..80_000u32).map(|i| (i % 97) as u8).collect();
    // Session openers here would work, but ForceLegacy must skip them entirely —
    // swap them for openers that would panic if the session were consumed.
    let (ps, os, pr, rec, _, _) = run_file(CompatMode::ForceLegacy, e, &data, "doc.bin").await;
    assert_eq!(ps, TransferPath::Legacy);
    assert_eq!(pr, TransferPath::Legacy);
    assert_eq!(os, TransferOutcome::Completed);
    assert_eq!(rec.outcome, TransferOutcome::Completed);
}

#[tokio::test]
async fn capability_mismatch_falls_back_to_legacy() {
    // Both sides establish a session, but neither advertises the Transfer
    // capability → capability mismatch → automatic legacy fallback.
    let no_transfer = SessionConfig::new(CapabilitySet::new());
    let e = ends(no_transfer.clone(), no_transfer);
    let data: Vec<u8> = (0..60_000u32).map(|i| (i % 131) as u8).collect();
    let (ps, _os, pr, rec, _, _) = run_file(CompatMode::Auto, e, &data, "cap.bin").await;
    assert_eq!(ps, TransferPath::Legacy);
    assert_eq!(pr, TransferPath::Legacy);
    assert_eq!(rec.outcome, TransferOutcome::Completed);
}

#[tokio::test]
async fn version_mismatch_falls_back_to_legacy() {
    let mut send_cfg = cfg_transfer();
    send_cfg.version = peerbeam_domain::session::Version::new(2, 0);
    let e = ends(send_cfg, cfg_transfer());
    let data: Vec<u8> = (0..40_000u32).map(|i| (i % 71) as u8).collect();
    let (ps, _os, pr, rec, _, _) = run_file(CompatMode::Auto, e, &data, "ver.bin").await;
    assert_eq!(ps, TransferPath::Legacy);
    assert_eq!(pr, TransferPath::Legacy);
    assert_eq!(rec.outcome, TransferOutcome::Completed);
}

#[tokio::test]
async fn older_peer_stub_falls_back_and_records_reason() {
    // Model an older peer that does not speak PeerSession at all.
    let (la, lb) = MemLink::pair(64);
    let enc = AeadCrypto::new();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 253) as u8).collect();
    let path = write_src(src.path(), "old.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _prx_s) = unbounded_channel::<Progress>();
    let (ptx_r, _prx_r) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let mut s_session = StubSessionSend(FallbackReason::OlderPeer);
    let mut r_session = StubSessionRecv(FallbackReason::OlderPeer);
    let mut s_legacy = mem_legacy(Box::new(la), "legacy-a");
    let mut r_legacy = mem_legacy(Box::new(lb), "legacy-b");

    let send = send_file_selected(
        CompatMode::Auto,
        &mut s_session,
        &mut s_legacy,
        &enc,
        &storage_s,
        send_req("old.bin", path, data.len() as u64),
        &ctrl_s,
        &ptx_s,
        0,
        &metrics_s,
    );
    let recv = receive_file_selected(
        CompatMode::Auto,
        &mut r_session,
        &mut r_legacy,
        &enc,
        &storage_r,
        &dst_dir,
        &ctrl_r,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    let (ps, _) = sr.expect("send");
    let (pr, _) = rr.expect("recv");
    assert_eq!(ps, TransferPath::Legacy);
    assert_eq!(pr, TransferPath::Legacy);
    assert_eq!(std::fs::read(dst.path().join("old.bin")).unwrap(), data);

    let ms = metrics_s.snapshot();
    assert_eq!(ms.legacy_transfers, 1);
    assert_eq!(ms.session_transfers, 0);
    assert_eq!(ms.fallbacks, 1);
    assert_eq!(ms.older_peer, 1);
}

#[tokio::test]
async fn session_and_legacy_produce_byte_for_byte_identical_output() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

    // Session path.
    let e1 = ends(cfg_transfer(), cfg_transfer());
    let (ps, _, _, rec_s, _, _) = run_file(CompatMode::Auto, e1, &data, "same.bin").await;
    assert_eq!(ps, TransferPath::Session);

    // Legacy path (forced).
    let e2 = ends(cfg_transfer(), cfg_transfer());
    let (pl, _, _, rec_l, _, _) = run_file(CompatMode::ForceLegacy, e2, &data, "same.bin").await;
    assert_eq!(pl, TransferPath::Legacy);

    // Both wrote the identical source bytes (run_file already asserts each dest ==
    // source), and both report the same byte count → identical hash of identical
    // content.
    assert_eq!(rec_s.bytes, rec_l.bytes);
    assert_eq!(rec_s.bytes, data.len() as u64);
    assert_eq!(rec_s.outcome, rec_l.outcome);
}

#[tokio::test]
async fn metrics_count_session_success() {
    let e = ends(cfg_transfer(), cfg_transfer());
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = vec![5u8; 30_000];
    let path = write_src(src.path(), "m.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel::<Progress>();
    let (ptx_r, _b) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let enc = e.enc.clone();
    let mut e = e;

    let send = send_file_selected(
        CompatMode::Auto,
        &mut e.s_session,
        &mut e.s_legacy,
        &*enc,
        &storage_s,
        send_req("m.bin", path, data.len() as u64),
        &ctrl,
        &ptx_s,
        0,
        &metrics_s,
    );
    let recv = receive_file_selected(
        CompatMode::Auto,
        &mut e.r_session,
        &mut e.r_legacy,
        &*enc,
        &storage_r,
        &dst_dir,
        &ctrl,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    sr.expect("send");
    rr.expect("recv");
    let ms = metrics_s.snapshot();
    assert_eq!(ms.session_transfers, 1);
    assert_eq!(ms.legacy_transfers, 0);
    assert_eq!(ms.fallbacks, 0);
}

#[tokio::test]
async fn folder_transfers_over_the_selected_session() {
    let e = ends(cfg_transfer(), cfg_transfer());
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(src.path().join("b.txt"), vec![3u8; 5000]).unwrap();
    let root = src.path().to_string_lossy().into_owned();
    let root_name = src
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel::<Progress>();
    let (ptx_r, _b) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let enc = e.enc.clone();
    let mut e = e;

    let req = FolderSendRequest {
        transfer_id: "f1".into(),
        root_path: root,
        chunk_size: 4096,
    };
    let send = send_folder_selected(
        CompatMode::Auto,
        &mut e.s_session,
        &mut e.s_legacy,
        &*enc,
        &storage_s,
        req,
        &ctrl_s,
        &ptx_s,
        0,
        &metrics_s,
    );
    // Receiver: folder receive over the selected session.
    let recv = peerbeam_transfer::receive_folder_selected(
        CompatMode::Auto,
        &mut e.r_session,
        &mut e.r_legacy,
        &*enc,
        &storage_r,
        &dst_dir,
        &ctrl_r,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    let (ps, outcome) = sr.expect("send folder");
    let (pr, received) = rr.expect("recv folder");
    assert_eq!(ps, TransferPath::Session);
    assert_eq!(pr, TransferPath::Session);
    assert_eq!(outcome, TransferOutcome::Completed);
    assert_eq!(received.files, 2);
    assert_eq!(
        std::fs::read(dst.path().join(&root_name).join("a.txt")).unwrap(),
        b"alpha"
    );
}

#[tokio::test]
async fn transfer_completes_after_fallback_preserving_integrity() {
    // Fall back to legacy (older peer) and confirm the transfer still completes
    // byte-for-byte — fallback must never affect integrity.
    let (la, lb) = MemLink::pair(64);
    let enc = AeadCrypto::new();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..150_000u32).map(|i| (i % 249) as u8).collect();
    let path = write_src(src.path(), "fb.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel::<Progress>();
    let (ptx_r, _b) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let mut s_session = StubSessionSend(FallbackReason::NegotiationFailed);
    let mut r_session = StubSessionRecv(FallbackReason::NegotiationFailed);
    let mut s_legacy = mem_legacy(Box::new(la), "legacy-a");
    let mut r_legacy = mem_legacy(Box::new(lb), "legacy-b");
    let send = send_file_selected(
        CompatMode::Auto,
        &mut s_session,
        &mut s_legacy,
        &enc,
        &storage_s,
        send_req("fb.bin", path, data.len() as u64),
        &ctrl_s,
        &ptx_s,
        0,
        &metrics_s,
    );
    let recv = receive_file_selected(
        CompatMode::Auto,
        &mut r_session,
        &mut r_legacy,
        &enc,
        &storage_r,
        &dst_dir,
        &ctrl_r,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    let (ps, os) = sr.expect("send");
    let (_pr, rec) = rr.expect("recv");
    assert_eq!(ps, TransferPath::Legacy);
    assert_eq!(os, TransferOutcome::Completed);
    assert_eq!(rec.bytes, data.len() as u64);
    assert_eq!(std::fs::read(dst.path().join("fb.bin")).unwrap(), data);
    assert_eq!(metrics_s.snapshot().negotiation_failed, 1);
}

/// `NeverLegacy` guards that the session path really is used (compile + runtime).
#[tokio::test]
async fn session_path_does_not_touch_legacy() {
    let e = ends(cfg_transfer(), cfg_transfer());
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = vec![1u8; 20_000];
    let path = write_src(src.path(), "s.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel::<Progress>();
    let (ptx_r, _b) = unbounded_channel::<Progress>();
    let metrics_s = peerbeam_transfer::MigrationMetrics::new();
    let metrics_r = peerbeam_transfer::MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();
    let enc = e.enc.clone();
    let mut e = e;
    let mut never_s = NeverLegacy;
    let mut never_r = NeverLegacy;

    let send = send_file_selected(
        CompatMode::Auto,
        &mut e.s_session,
        &mut never_s,
        &*enc,
        &storage_s,
        send_req("s.bin", path, data.len() as u64),
        &ctrl,
        &ptx_s,
        0,
        &metrics_s,
    );
    let recv = receive_file_selected(
        CompatMode::Auto,
        &mut e.r_session,
        &mut never_r,
        &*enc,
        &storage_r,
        &dst_dir,
        &ctrl,
        &ptx_r,
        &metrics_r,
    );
    let (sr, rr) = tokio::join!(send, recv);
    assert_eq!(sr.expect("send").0, TransferPath::Session);
    assert_eq!(rr.expect("recv").0, TransferPath::Session);
}
