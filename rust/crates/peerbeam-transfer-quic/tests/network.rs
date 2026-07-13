//! Real-network integration tests over the QUIC transport.
//!
//! Every test here uses **real QUIC endpoints** (real UDP sockets, TLS 1.3,
//! congestion control) — no in-process mock. They run on whatever interfaces
//! the host has: loopback IPv4/IPv6 are always available, so those run
//! everywhere; scenarios needing extra privilege or hardware (netem latency /
//! loss, network namespaces for subnets, physical Wi-Fi/Ethernet/USB/Tailscale)
//! live in `scripts/nettest.sh`, which this file's doc-matrix mirrors.
//!
//! Covered here (host loopback, no privilege needed):
//! - IPv4 transfer            (`ipv4_loopback_transfer`)
//! - IPv6 transfer            (`ipv6_loopback_transfer`)
//! - Multiple simultaneous    (`multiple_simultaneous_transfers`)
//! - Resume after disconnect  (`resume_after_real_disconnect`)
//! - Large file >10 GB        (`large_file_over_quic`, `#[ignore]`)

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;
use serial_test::serial;
use tokio::sync::mpsc;

use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{Frame, Link, TransferProvider};
use peerbeam_reliability_fs::FsReliability;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file, receive_file_recover, send_file, send_file_recover, LinkFactory, Received,
    SendRequest, TransferControl, TransferOutcome,
};
use peerbeam_transfer_quic::{direct_route, QuicTransport};

fn session(size: u64) -> TransferSession {
    TransferSession {
        id: TransferId::from("net"),
        peer: DeviceId::from("peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: Vec::new(),
        total_bytes: size,
        transferred_bytes: 0,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
    }
}

fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// One real transfer over `bind` (server) reachable at `dial_host`.
async fn one_transfer(server_bind: SocketAddr, dial_host: &str, payload: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("f.bin");
    let out = dir.path().join("out");
    std::fs::write(&src, payload).unwrap();

    let server = QuicTransport::bound(server_bind).unwrap();
    let (addr, mut incoming) = server.serve_addr_on(server_bind).await.unwrap();
    let client = QuicTransport::bound(server_bind).unwrap();
    let route = direct_route(dial_host, addr.port());

    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let (ptx2, _p2) = mpsc::unbounded_channel();
    let sess = session(payload.len() as u64);

    let send = async {
        let mut link = client.dial(&route, &sess).await.unwrap();
        let req = SendRequest {
            transfer_id: "t".into(),
            name: "f.bin".into(),
            path: src.to_string_lossy().into(),
            size: payload.len() as u64,
            chunk_size: 64 * 1024,
        };
        send_file(&mut *link, &storage_s, req, &cs, &ptx, 3)
            .await
            .unwrap()
    };
    let out_str = out.to_string_lossy().to_string();
    let recv = async {
        let mut link = incoming.next().await.expect("conn").unwrap();
        receive_file(&mut *link, &storage_r, &out_str, &cr, &ptx2)
            .await
            .unwrap()
    };
    let (so, ro) = tokio::join!(send, recv);
    assert_eq!(so, TransferOutcome::Completed);
    assert_eq!(ro.outcome, TransferOutcome::Completed);
    std::fs::read(out.join("f.bin")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn ipv4_loopback_transfer() {
    let payload = pattern(2 * 1024 * 1024);
    let got = one_transfer("127.0.0.1:0".parse().unwrap(), "127.0.0.1", &payload).await;
    assert_eq!(got, payload, "IPv4 transfer must be byte-exact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn ipv6_loopback_transfer() {
    // Skip cleanly if the host has no IPv6 loopback.
    if std::net::UdpSocket::bind("[::1]:0").is_err() {
        eprintln!("skip: no IPv6 loopback on this host");
        return;
    }
    let payload = pattern(2 * 1024 * 1024);
    let got = one_transfer("[::1]:0".parse().unwrap(), "::1", &payload).await;
    assert_eq!(got, payload, "IPv6 transfer must be byte-exact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn multiple_simultaneous_transfers() {
    const N: usize = 8;
    let quic = Arc::new(QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap());
    let (addr, mut incoming) = quic
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    let outdir = tempfile::tempdir().unwrap();
    let recv_dir = outdir.path().to_string_lossy().into_owned();
    let payloads: Vec<Vec<u8>> = (0..N).map(|i| pattern(128 * 1024 + i * 4096)).collect();

    // Sources on disk. Each carries a unique name so results are matched by
    // filename, not by accept order.
    let srcdir = tempfile::tempdir().unwrap();
    let src_paths: Vec<String> = payloads
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let path = srcdir.path().join(format!("s{i}.bin"));
            std::fs::write(&path, p).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();

    // Receiver: accept N links and receive each concurrently into the shared
    // dir. `incoming` stays borrowed in this scope (not a detached spawn) so
    // the server endpoint outlives every in-flight transfer.
    let recv_fut = async {
        let mut tasks = Vec::new();
        for _ in 0..N {
            let mut link = incoming.next().await.expect("conn").expect("ok");
            let dir = recv_dir.clone();
            tasks.push(tokio::spawn(async move {
                let storage = FsStorage::new();
                let (ptx, _p) = mpsc::unbounded_channel();
                let ctrl = TransferControl::new();
                receive_file(&mut *link, &storage, &dir, &ctrl, &ptx).await
            }));
        }
        let mut ok = 0usize;
        for t in tasks {
            if matches!(t.await, Ok(Ok(_))) {
                ok += 1;
            }
        }
        ok
    };

    // Senders: all dial concurrently on the shared client endpoint.
    let send_fut = futures::future::join_all((0..N).map(|i| {
        let quic = quic.clone();
        let route = route.clone();
        let path = src_paths[i].clone();
        let size = payloads[i].len() as u64;
        async move {
            let mut link = quic.dial(&route, &session(size)).await.unwrap();
            let req = SendRequest {
                transfer_id: format!("t{i}"),
                name: format!("s{i}.bin"),
                path,
                size,
                chunk_size: 32 * 1024,
            };
            let storage = FsStorage::new();
            let (ptx, _p) = mpsc::unbounded_channel();
            let ctrl = TransferControl::new();
            send_file(&mut *link, &storage, req, &ctrl, &ptx, 3)
                .await
                .unwrap()
        }
    }));

    let (received_ok, outcomes) = tokio::join!(recv_fut, send_fut);
    assert!(outcomes.iter().all(|o| *o == TransferOutcome::Completed));
    assert_eq!(received_ok, N, "all {N} concurrent transfers must complete");

    for (i, p) in payloads.iter().enumerate() {
        let got = std::fs::read(outdir.path().join(format!("s{i}.bin")))
            .unwrap_or_else(|_| panic!("output {i} missing"));
        assert_eq!(&got, p, "payload {i} must match byte-for-byte");
    }
}

// ── Resume after a real mid-transfer disconnect ─────────────────

/// Wraps a real link and hard-fails after `ok_sends` frames, forcing the
/// recovery driver to reconnect. Dropping it drops the inner QUIC connection,
/// so the peer observes a real disconnect.
struct FaultyLink {
    inner: Box<dyn Link>,
    ok_sends: usize,
}

#[async_trait]
impl Link for FaultyLink {
    async fn send_frame(&mut self, frame: Frame) -> Result<()> {
        if self.ok_sends == 0 {
            return Err(DomainError::Connection("injected disconnect".into()));
        }
        self.ok_sends -= 1;
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> Result<Option<Frame>> {
        self.inner.recv_frame().await
    }
    async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

/// Sender factory: first link is faulty (drops mid-transfer), the rest are
/// healthy, so `send_file_recover` reconnects and resumes.
struct DialFactory {
    quic: Arc<QuicTransport>,
    route: peerbeam_domain::entity::Route,
    size: u64,
    fail_first_after: usize,
    attempt: usize,
}

#[async_trait]
impl LinkFactory for DialFactory {
    async fn connect(&mut self) -> Result<Box<dyn Link>> {
        self.attempt += 1;
        let link = self.quic.dial(&self.route, &session(self.size)).await?;
        if self.attempt == 1 {
            Ok(Box::new(FaultyLink {
                inner: link,
                ok_sends: self.fail_first_after,
            }))
        } else {
            Ok(link)
        }
    }
}

/// Receiver factory: pulls the next accepted inbound link from the serve stream.
struct AcceptFactory {
    incoming: BoxStream<'static, Result<Box<dyn Link>>>,
}

#[async_trait]
impl LinkFactory for AcceptFactory {
    async fn connect(&mut self) -> Result<Box<dyn Link>> {
        match self.incoming.next().await {
            Some(link) => link,
            None => Err(DomainError::Connection("server closed".into())),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn resume_after_real_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let payload = pattern(4 * 1024 * 1024); // 4 MiB, 64 chunks @ 64 KiB
    std::fs::write(&src, &payload).unwrap();

    let quic = Arc::new(QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap());
    let (addr, incoming) = quic
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    // Receiver: recover across reconnects into `out`.
    let out_str = out.to_string_lossy().to_string();
    let recv = tokio::spawn(async move {
        let mut factory = AcceptFactory { incoming };
        let storage = FsStorage::new();
        let ctrl = TransferControl::new();
        let (ptx, _p) = mpsc::unbounded_channel();
        receive_file_recover(&mut factory, &storage, &out_str, &ctrl, &ptx, 5).await
    });

    // Sender: first link dies after ~6 frames (Meta + a few chunks), then
    // reconnects and resumes.
    let reli = FsReliability::new(dir.path().join("checkpoints"));
    let storage = FsStorage::new();
    let ctrl = TransferControl::new();
    let (ptx, _p) = mpsc::unbounded_channel();
    let mut factory = DialFactory {
        quic: quic.clone(),
        route,
        size: payload.len() as u64,
        fail_first_after: 6,
        attempt: 0,
    };
    let req = SendRequest {
        transfer_id: "resume".into(),
        name: "big.bin".into(),
        path: src.to_string_lossy().into(),
        size: payload.len() as u64,
        chunk_size: 64 * 1024,
    };
    let outcome = send_file_recover(
        &mut factory,
        &storage,
        &reli,
        req,
        session(payload.len() as u64),
        &ctrl,
        &ptx,
        5,
        3,
    )
    .await
    .expect("send resumes and completes");
    assert_eq!(outcome, TransferOutcome::Completed);

    let received: Received = recv.await.unwrap().expect("receive resumes and completes");
    assert_eq!(received.outcome, TransferOutcome::Completed);
    assert!(factory.attempt >= 2, "a reconnect must have happened");

    let got = std::fs::read(out.join("big.bin")).unwrap();
    assert_eq!(got, payload, "resumed file must be byte-exact");
}

// ── Large file (>10 GB) over real QUIC ──────────────────────────

mod genstore {
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use async_trait::async_trait;
    use futures::io::{AsyncRead, AsyncWrite};

    use peerbeam_domain::error::{DomainError, Result};
    use peerbeam_domain::port::StorageProvider;

    struct GenReader {
        remaining: u64,
    }
    impl AsyncRead for GenReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.remaining == 0 {
                return Poll::Ready(Ok(0));
            }
            let n = (buf.len() as u64).min(self.remaining) as usize;
            for b in &mut buf[..n] {
                *b = 0;
            }
            self.remaining -= n as u64;
            Poll::Ready(Ok(n))
        }
    }

    struct NullWriter;
    impl AsyncWrite for NullWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Generates source bytes and discards received bytes — lets a >10 GB
    /// transfer run over real QUIC with constant memory and no disk.
    pub struct GenStorage;

    #[async_trait]
    impl StorageProvider for GenStorage {
        async fn open_write(&self, _p: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
            Ok(Box::new(NullWriter))
        }
        async fn open_append(&self, _p: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
            Ok(Box::new(NullWriter))
        }
        async fn open_read(
            &self,
            path: &str,
            offset: u64,
        ) -> Result<Box<dyn AsyncRead + Unpin + Send>> {
            let total: u64 = path
                .strip_prefix("gen:")
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| DomainError::Storage(format!("bad gen path: {path}")))?;
            Ok(Box::new(GenReader {
                remaining: total.saturating_sub(offset),
            }))
        }
        async fn size(&self, _p: &str) -> Result<Option<u64>> {
            Ok(None)
        }
        async fn list_files(&self, _r: &str) -> Result<Vec<(String, u64)>> {
            Ok(Vec::new())
        }
        async fn finalize(&self, _t: &str, dest: &str) -> Result<String> {
            Ok(dest.to_string())
        }
    }
}

/// >10 GB over real QUIC. Ignored by default (moves ~11 GB, hashes ~22 GB).
/// Run with: `cargo test -p peerbeam-transfer-quic --release -- --ignored large_file`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow/huge: >10 GB over real QUIC; run explicitly"]
async fn large_file_over_quic() {
    use genstore::GenStorage;
    const TOTAL: u64 = 11 * 1024 * 1024 * 1024; // > 10 GiB

    let quic = QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap();
    let (addr, mut incoming) = quic
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    let send = async {
        let mut link = client.dial(&route, &session(TOTAL)).await.unwrap();
        let storage = GenStorage;
        let (ptx, _p) = mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: "huge".into(),
            name: "huge.bin".into(),
            path: format!("gen:{TOTAL}"),
            size: TOTAL,
            chunk_size: 1024 * 1024,
        };
        send_file(&mut *link, &storage, req, &ctrl, &ptx, 3)
            .await
            .unwrap()
    };
    let recv = async {
        let mut link = incoming.next().await.expect("conn").unwrap();
        let storage = GenStorage;
        let (ptx, _p) = mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        receive_file(&mut *link, &storage, "/gen", &ctrl, &ptx)
            .await
            .unwrap()
    };
    let (so, ro) = tokio::join!(send, recv);
    assert_eq!(so, TransferOutcome::Completed);
    assert_eq!(ro.bytes, TOTAL, "all >10 GB must be accounted for");
}
