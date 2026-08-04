//! Real-QUIC M7 cutover: the transport selector runs a file transfer over two
//! real QUIC endpoints on both the default PeerSession path and the legacy path,
//! proving the selection + fallback work on the network stack (not just in
//! memory) and that both produce byte-for-byte-correct output.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{Direction, Route, TransferSession, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{
    ChannelTransport, EncryptionProvider, Link, TransferProvider, TrustStore,
};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, SessionError};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file_selected, send_file_selected, transfer_capability, CompatMode,
    FallbackReason, Identity, IncomingStreamChannel, LegacyPath, MigrationMetrics, PeerSession,
    SendRequest, Session, SessionConfig, SessionHandle, SessionOpen, SessionReceivePath,
    SessionRole, SessionSendPath, TransferControl, TransferPath,
};
use peerbeam_transfer_quic::{direct_route, QuicChannels, QuicTransport};
use peerbeam_trust_fs::FsTrust;
use serial_test::serial;
use tokio::sync::mpsc::unbounded_channel;

const TRANSFER: ChannelType = ChannelType::TRANSFER;

fn cfg_transfer() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(TRANSFER)))
        .with_stream_channel_type(TRANSFER)
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
    }
}

type Sec = (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>);

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

fn write_src(dir: &std::path::Path, name: &str, data: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p.to_string_lossy().into_owned()
}

fn req(name: &str, path: String, size: u64) -> SendRequest {
    SendRequest {
        transfer_id: name.to_string(),
        name: name.to_string(),
        path,
        size,
        chunk_size: 8192,
    }
}

// ── session-path openers over QUIC ───────────────────────────────────────────

struct QuicSessionSend {
    client: QuicTransport,
    route: Route,
    sec: Option<Sec>,
}
#[async_trait]
impl SessionSendPath for QuicSessionSend {
    async fn open(&mut self) -> Result<SessionOpen<SessionHandle>> {
        let qc = self
            .client
            .dial_channels(&self.route, &dial_session())
            .await
            .map_err(|e| DomainError::Connection(e.to_string()))?;
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let (ev, _e) = unbounded_channel();
        let (ch, _c) = unbounded_channel();
        let (inc, _i) = unbounded_channel();
        std::mem::forget(_e);
        std::mem::forget(_c);
        std::mem::forget(_i);
        let t: Arc<dyn ChannelTransport> = Arc::new(qc);
        match PeerSession::open(
            t,
            SessionRole::Initiator,
            cfg_transfer(),
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
            Ok(mut s) => {
                if let Err(r) = transfer_capability(&s) {
                    return Ok(SessionOpen::Fallback(r));
                }
                let h = s.handle();
                tokio::spawn(async move { s.run().await });
                Ok(SessionOpen::Ready(h))
            }
            Err(SessionError::VersionIncompatible { .. }) => {
                Ok(SessionOpen::Fallback(FallbackReason::VersionMismatch))
            }
            Err(_) => Ok(SessionOpen::Fallback(FallbackReason::OlderPeer)),
        }
    }
}

struct QuicSessionRecv {
    incoming: BoxStream<'static, Result<QuicChannels>>,
    sec: Option<Sec>,
}
#[async_trait]
impl SessionReceivePath for QuicSessionRecv {
    async fn open(&mut self) -> Result<SessionOpen<(SessionHandle, IncomingStreamChannel)>> {
        let qc = self
            .incoming
            .next()
            .await
            .ok_or_else(|| DomainError::Connection("listener closed".into()))??;
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let (ev, _e) = unbounded_channel();
        let (ch, _c) = unbounded_channel();
        let (inc, mut inc_rx) = unbounded_channel();
        std::mem::forget(_e);
        std::mem::forget(_c);
        let t: Arc<dyn ChannelTransport> = Arc::new(qc);
        match PeerSession::open(
            t,
            SessionRole::Responder,
            cfg_transfer(),
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
            Ok(mut s) => {
                if let Err(r) = transfer_capability(&s) {
                    return Ok(SessionOpen::Fallback(r));
                }
                let h = s.handle();
                tokio::spawn(async move { s.run().await });
                let incoming = inc_rx
                    .recv()
                    .await
                    .ok_or_else(|| DomainError::Connection("no incoming channel".into()))?;
                Ok(SessionOpen::Ready((h, incoming)))
            }
            Err(SessionError::VersionIncompatible { .. }) => {
                Ok(SessionOpen::Fallback(FallbackReason::VersionMismatch))
            }
            Err(_) => Ok(SessionOpen::Fallback(FallbackReason::OlderPeer)),
        }
    }
}

// ── legacy-path openers over QUIC ────────────────────────────────────────────

struct QuicLegacyDial {
    client: QuicTransport,
    route: Route,
    sec: Option<Sec>,
}
#[async_trait]
impl LegacyPath for QuicLegacyDial {
    async fn open(&mut self) -> Result<(Box<dyn Link>, Session)> {
        let mut link = TransferProvider::dial(&self.client, &self.route, &dial_session()).await?;
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let session = authenticate(link.as_mut(), &id, &*enc, &*trust).await?;
        Ok((link, session))
    }
}

struct QuicLegacyAccept {
    incoming: BoxStream<'static, Result<Box<dyn Link>>>,
    sec: Option<Sec>,
}
#[async_trait]
impl LegacyPath for QuicLegacyAccept {
    async fn open(&mut self) -> Result<(Box<dyn Link>, Session)> {
        let mut link = self
            .incoming
            .next()
            .await
            .ok_or_else(|| DomainError::Connection("listener closed".into()))??;
        let (id, enc, trust) = self.sec.take().expect("sec once");
        let session = authenticate(link.as_mut(), &id, &*enc, &*trust).await?;
        Ok((link, session))
    }
}

/// Session opener that forces a fallback (models an old peer that only does legacy).
struct StubSend(FallbackReason);
#[async_trait]
impl SessionSendPath for StubSend {
    async fn open(&mut self) -> Result<SessionOpen<SessionHandle>> {
        Ok(SessionOpen::Fallback(self.0))
    }
}
struct StubRecv(FallbackReason);
#[async_trait]
impl SessionReceivePath for StubRecv {
    async fn open(&mut self) -> Result<SessionOpen<(SessionHandle, IncomingStreamChannel)>> {
        Ok(SessionOpen::Fallback(self.0))
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn real_quic_selector_uses_peersession() {
    let server = QuicTransport::new().unwrap();
    let (addr, incoming) = server
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    let mut s_session = QuicSessionSend {
        client,
        route,
        sec: Some(security("device-a")),
    };
    let mut r_session = QuicSessionRecv {
        incoming,
        sec: Some(security("device-b")),
    };
    // Legacy openers must not be reached on the session path.
    let mut s_legacy = QuicLegacyDial {
        client: QuicTransport::new().unwrap(),
        route: direct_route("127.0.0.1", 1),
        sec: Some(security("x")),
    };
    let mut r_legacy = QuicLegacyDial {
        client: QuicTransport::new().unwrap(),
        route: direct_route("127.0.0.1", 1),
        sec: Some(security("y")),
    };

    let enc = AeadCrypto::new();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let path = write_src(src.path(), "q.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel();
    let (ptx_r, _b) = unbounded_channel();
    let m_s = MigrationMetrics::new();
    let m_r = MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let send = send_file_selected(
        CompatMode::Auto,
        &mut s_session,
        &mut s_legacy,
        &enc,
        &storage_s,
        req("q.bin", path, data.len() as u64),
        &ctrl_s,
        &ptx_s,
        0,
        &m_s,
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
        &m_r,
    );
    let (sr, rr) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(send, recv) })
            .await
            .expect("transfer timed out");
    assert_eq!(sr.expect("send").0, TransferPath::Session);
    assert_eq!(rr.expect("recv").0, TransferPath::Session);
    assert_eq!(std::fs::read(dst.path().join("q.bin")).unwrap(), data);
    assert_eq!(m_s.snapshot().session_transfers, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn real_quic_selector_falls_back_to_legacy() {
    let server = QuicTransport::new().unwrap();
    let (addr, incoming) = server
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    // Session openers force a fallback (old peer); legacy runs over real QUIC.
    let mut s_session = StubSend(FallbackReason::OlderPeer);
    let mut r_session = StubRecv(FallbackReason::OlderPeer);
    let mut s_legacy = QuicLegacyDial {
        client,
        route,
        sec: Some(security("device-a")),
    };
    let mut r_legacy = QuicLegacyAccept {
        incoming,
        sec: Some(security("device-b")),
    };

    let enc = AeadCrypto::new();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..250_000u32).map(|i| (i % 249) as u8).collect();
    let path = write_src(src.path(), "leg.bin", &data);
    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel();
    let (ptx_r, _b) = unbounded_channel();
    let m_s = MigrationMetrics::new();
    let m_r = MigrationMetrics::new();
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let send = send_file_selected(
        CompatMode::Auto,
        &mut s_session,
        &mut s_legacy,
        &enc,
        &storage_s,
        req("leg.bin", path, data.len() as u64),
        &ctrl_s,
        &ptx_s,
        0,
        &m_s,
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
        &m_r,
    );
    let (sr, rr) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(send, recv) })
            .await
            .expect("transfer timed out");
    assert_eq!(sr.expect("send").0, TransferPath::Legacy);
    assert_eq!(rr.expect("recv").0, TransferPath::Legacy);
    assert_eq!(std::fs::read(dst.path().join("leg.bin")).unwrap(), data);
    assert_eq!(m_s.snapshot().legacy_transfers, 1);
    assert_eq!(m_s.snapshot().older_peer, 1);
}
