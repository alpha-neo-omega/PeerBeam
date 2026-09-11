//! Integration tests over **two real QUIC endpoints** on localhost (not an
//! in-process mock): a server `serve`s and a client `dial`s, and the existing
//! transfer engine runs over the resulting `Link`s unchanged.

use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use tokio::sync::mpsc;

use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{Bind, TransferProvider};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl, TransferOutcome};
use peerbeam_transfer_quic::{direct_route, QuicTransport};

fn session() -> TransferSession {
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
        accepted: true,
    }
}

fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfers_a_file_over_real_quic() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let out = dir.path().join("out");
    let bytes = pattern(2 * 1024 * 1024); // 2 MiB → many chunks + both directions
    std::fs::write(&src, &bytes).unwrap();

    let server = QuicTransport::new().unwrap();
    let (addr, mut incoming) = server.serve_addr(Bind { port: 0 }).await.unwrap();

    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();
    let (ptx2, _prx2) = mpsc::unbounded_channel();

    let sess = session();
    let send = async {
        let mut link = client.dial(&route, &sess).await.unwrap();
        let req = SendRequest {
            transfer_id: "t1".into(),
            name: "src.bin".into(),
            path: src.to_string_lossy().into(),
            size: bytes.len() as u64,
            chunk_size: 64 * 1024,
        };
        send_file(&mut *link, &storage_s, req, &ctrl_s, &ptx, 3)
            .await
            .unwrap()
    };
    let out_str = out.to_string_lossy().to_string();
    let recv = async {
        let mut link = incoming.next().await.expect("a connection").unwrap();
        receive_file(&mut *link, &storage_r, &out_str, &ctrl_r, &ptx2)
            .await
            .unwrap()
    };

    let (send_outcome, received) = tokio::join!(send, recv);
    assert_eq!(send_outcome, TransferOutcome::Completed);
    assert_eq!(received.outcome, TransferOutcome::Completed);
    assert_eq!(received.bytes, bytes.len() as u64);

    let written = std::fs::read(out.join("src.bin")).unwrap();
    assert_eq!(written, bytes, "file must arrive byte-for-byte over QUIC");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_surfaces_as_error_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    std::fs::write(&src, pattern(16 * 1024 * 1024)).unwrap(); // large enough to still be in flight

    let server = QuicTransport::new().unwrap();
    let (addr, mut incoming) = server.serve_addr(Bind { port: 0 }).await.unwrap();
    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());
    let storage = FsStorage::new();
    let ctrl = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();
    let sess = session();

    // Server accepts then immediately drops the connection.
    let killer = async {
        let link = incoming.next().await.expect("a connection").unwrap();
        drop(link);
        drop(incoming);
        drop(server);
    };
    let sender = async {
        let mut link = client.dial(&route, &sess).await.unwrap();
        let req = SendRequest {
            transfer_id: "t2".into(),
            name: "big.bin".into(),
            path: src.to_string_lossy().into(),
            size: 16 * 1024 * 1024,
            chunk_size: 64 * 1024,
        };
        // retries = 0: a dropped peer must return a Connection error promptly,
        // not hang or panic.
        send_file(&mut *link, &storage, req, &ctrl, &ptx, 0).await
    };

    let (_k, result) = tokio::join!(killer, sender);
    assert!(
        result.is_err(),
        "a mid-transfer disconnect must surface as an error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dial_to_dead_address_errors() {
    let client = QuicTransport::new().unwrap();
    // Nothing listening here.
    let route = direct_route("127.0.0.1", 1);
    let sess = session();
    let result = tokio::time::timeout(Duration::from_secs(5), client.dial(&route, &sess)).await;
    // Either the dial returns an error, or it times out — both mean "did not
    // connect to a dead port", which is the behaviour under test.
    if let Ok(r) = result {
        assert!(r.is_err(), "dial to a dead port must error");
    }
}

/// **An IPv6 peer is dialable.** `QuicTransport::new()` used to bind `0.0.0.0`
/// alone, so every IPv6 address — including the tailnet `fd7a:` address a
/// Tailscale peer always advertises — was unreachable on every platform. This
/// serves on IPv6 loopback and dials it, which cannot connect without the
/// second endpoint `new()` now creates.
///
/// Both halves are bounded: an unbounded `incoming.next()` here hangs the whole
/// suite when the dial fails, which is exactly what the first version of this
/// test did.
#[tokio::test]
async fn an_ipv6_peer_can_be_dialled() {
    let server = QuicTransport::new().expect("transport");
    let (local, incoming) = match server.serve_addr_on("[::1]:0".parse().expect("addr")).await {
        Ok(pair) => pair,
        // A host with IPv6 disabled cannot serve on it either; that is not a
        // failure of this code and must not fail the suite.
        Err(_) => return,
    };
    assert!(local.is_ipv6(), "expected an IPv6 listener, got {local}");

    let client = QuicTransport::new().expect("transport");
    let route = direct_route("::1", local.port());
    let sess = session();

    // Only the dial is asserted. A successful `dial` means quinn completed the
    // QUIC handshake with that IPv6 listener — which is the whole property
    // under test. The server's `incoming` deliberately does NOT yield yet: per
    // `dial`'s own doc the stream "materialises on the server once the first
    // frame is written by the engine", so awaiting an accept here would hang
    // waiting for a transfer this test never starts.
    let dialed = tokio::time::timeout(Duration::from_secs(10), client.dial(&route, &sess))
        .await
        .expect("dial timed out — the IPv6 endpoint did not connect");
    assert!(
        dialed.is_ok(),
        "an IPv6 peer could not be dialled: {:?}",
        dialed.err()
    );
    drop(incoming);
}
