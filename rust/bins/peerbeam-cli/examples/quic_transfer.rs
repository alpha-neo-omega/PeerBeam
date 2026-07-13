//! End-to-end file transfer over the real QUIC transport, in one process.
//!
//! Shows the engine's public API: `QuicTransport` (dial/serve) → `authenticate`
//! (mutual X25519) → `SecureLink` (per-frame AES-256-GCM) → `send_file` /
//! `receive_file`. Two distinct identities talk over loopback.
//!
//! Run:  cargo run --example quic_transfer -p peerbeam-cli

use futures::StreamExt;
use tokio::sync::mpsc;

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{EncryptionProvider, TransferProvider};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file, send_file, Identity, SecureLink, SendRequest, TransferControl,
};
use peerbeam_transfer_quic::{direct_route, QuicTransport};

fn identity(
    name: &str,
    dir: &std::path::Path,
) -> (AeadCrypto, peerbeam_trust_fs::FsTrust, Identity) {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let trust = peerbeam_trust_fs::FsTrust::open(dir.join(format!("{name}.trust"))).unwrap();
    let id = Identity {
        device_id: DeviceId::from(name),
        name: name.into(),
        keypair,
    };
    (enc, trust, id)
}

fn session(total: u64) -> TransferSession {
    TransferSession {
        id: TransferId::from("example"),
        peer: DeviceId::from("peer"),
        direction: Direction::Sending,
        status: TransferStatus::Transferring,
        files: Vec::new(),
        total_bytes: total,
        transferred_bytes: 0,
        started_at: chrono::Utc::now(),
        completed_at: None,
        is_resume: false,
    }
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("pb-example-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("out")).unwrap();
    let src = dir.join("hello.txt");
    std::fs::write(&src, b"Hello from PeerBeam over QUIC!").unwrap();

    // Receiver: serve QUIC on an OS-assigned port.
    let server = QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap();
    let (addr, mut incoming) = server
        .serve_addr_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    let (enc_r, trust_r, id_r) = identity("receiver", &dir);
    let out = dir.join("out").to_string_lossy().into_owned();
    let recv = async move {
        let mut link = incoming.next().await.unwrap().unwrap();
        let sess = authenticate(&mut *link, &id_r, &enc_r, &trust_r)
            .await
            .unwrap();
        let mut secure = SecureLink::new(&mut *link, &enc_r, sess);
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        receive_file(&mut secure, &FsStorage::new(), &out, &ctrl, &tx)
            .await
            .unwrap()
    };

    // Sender: dial the receiver, authenticate, stream the file.
    let client = QuicTransport::bound("127.0.0.1:0".parse().unwrap()).unwrap();
    let (enc_s, trust_s, id_s) = identity("sender", &dir);
    let src_str = src.to_string_lossy().into_owned();
    let size = std::fs::metadata(&src).unwrap().len();
    let send = async move {
        let route = direct_route("127.0.0.1", addr.port());
        let mut link = client.dial(&route, &session(size)).await.unwrap();
        let sess = authenticate(&mut *link, &id_s, &enc_s, &trust_s)
            .await
            .unwrap();
        let mut secure = SecureLink::new(&mut *link, &enc_s, sess);
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let req = SendRequest {
            transfer_id: "example".into(),
            name: "hello.txt".into(),
            path: src_str,
            size,
            chunk_size: 64 * 1024,
        };
        send_file(&mut secure, &FsStorage::new(), req, &ctrl, &tx, 3)
            .await
            .unwrap()
    };

    let (received, _sent) = tokio::join!(recv, send);
    println!(
        "received {} ({} bytes) → {}",
        received.name,
        received.bytes,
        dir.join("out").join("hello.txt").display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
