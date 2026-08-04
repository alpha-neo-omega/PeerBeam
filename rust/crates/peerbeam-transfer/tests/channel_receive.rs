//! `receive_on_channel` (M8.5): a PeerSession receiver dispatches a file **or** a
//! folder over one incoming transfer channel by peeking the first frame — the
//! folder-capable receive primitive the frontend cutover needs. Real per-channel
//! crypto + real handshake over an in-memory transport.

mod common;

use std::sync::Arc;

use tokio::sync::mpsc::unbounded_channel;

use common::MemTransport;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::Progress;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_on_channel, send_file_on_session, send_folder_on_session, ChannelReceived,
    FolderSendRequest, Identity, IncomingStreamChannel, PeerSession, SendRequest, SessionConfig,
    SessionHandle, SessionRole, TransferControl, TransferOutcome,
};
use peerbeam_trust_fs::FsTrust;

const TRANSFER: ChannelType = ChannelType::TRANSFER;

type Sec = (Identity, Arc<dyn EncryptionProvider>, Arc<dyn TrustStore>);

fn security(name: &str) -> Sec {
    let enc = AeadCrypto::new();
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: DeviceId::from(name),
        name: name.to_string(),
        keypair,
    };
    let dir = tempfile::tempdir().unwrap();
    let trust = FsTrust::open(dir.path().join("trust.json")).unwrap();
    std::mem::forget(dir);
    (identity, Arc::new(enc), Arc::new(trust))
}

fn cfg() -> SessionConfig {
    SessionConfig::new(CapabilitySet::new().with(Capability::new(TRANSFER)))
        .with_stream_channel_type(TRANSFER)
}

/// A running session pair: both handles + the responder's incoming-channel receiver.
struct Pair {
    a: SessionHandle,
    b: SessionHandle,
    b_incoming: tokio::sync::mpsc::UnboundedReceiver<IncomingStreamChannel>,
}

async fn establish() -> Pair {
    let (ta, tb) = MemTransport::pair();
    let (a_ev, a_ev_rx) = unbounded_channel();
    let (b_ev, b_ev_rx) = unbounded_channel();
    let (a_ch, a_ch_rx) = unbounded_channel();
    let (b_ch, b_ch_rx) = unbounded_channel();
    let (a_in, a_in_rx) = unbounded_channel();
    let (b_in, b_incoming) = unbounded_channel();
    // Keep the unused receivers alive so their senders never report "closed".
    std::mem::forget(a_ev_rx);
    std::mem::forget(b_ev_rx);
    std::mem::forget(a_ch_rx);
    std::mem::forget(b_ch_rx);
    std::mem::forget(a_in_rx);
    let (id_a, enc_a, trust_a) = security("device-a");
    let (id_b, enc_b, trust_b) = security("device-b");
    let fa = PeerSession::open(
        ta,
        SessionRole::Initiator,
        cfg(),
        a_ev,
        a_ch,
        a_in,
        None,
        id_a,
        enc_a,
        trust_a,
    );
    let fb = PeerSession::open(
        tb,
        SessionRole::Responder,
        cfg(),
        b_ev,
        b_ch,
        b_in,
        None,
        id_b,
        enc_b,
        trust_b,
    );
    let (ra, rb) = tokio::join!(fa, fb);
    let mut a = ra.expect("initiator");
    let mut b = rb.expect("responder");
    let ah = a.handle();
    let bh = b.handle();
    tokio::spawn(async move { a.run().await });
    tokio::spawn(async move { b.run().await });
    Pair {
        a: ah,
        b: bh,
        b_incoming,
    }
}

/// A folder over a channel is received as `ChannelReceived::Folder`, byte-for-byte.
#[tokio::test]
async fn receive_on_channel_dispatches_a_folder() {
    let mut p = establish().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(src.path().join("b.bin"), vec![7u8; 4096]).unwrap();
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
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let send = send_folder_on_session(
        &p.a,
        &storage_s,
        FolderSendRequest {
            transfer_id: "f".into(),
            root_path: src.path().to_string_lossy().into_owned(),
            chunk_size: 2048,
        },
        &ctrl_s,
        &ptx_s,
        0,
    );
    let recv = async {
        let inc = p.b_incoming.recv().await.expect("incoming");
        receive_on_channel(inc, &p.b, &storage_r, &dst_dir, &ctrl_r, &ptx_r).await
    };
    let (sr, rr) = tokio::join!(send, recv);
    assert_eq!(sr.expect("send folder"), TransferOutcome::Completed);
    match rr.expect("recv") {
        ChannelReceived::Folder(fr) => {
            assert_eq!(fr.files, 2);
            assert_eq!(
                std::fs::read(dst.path().join(&root_name).join("a.txt")).unwrap(),
                b"alpha"
            );
        }
        other => panic!("expected folder, got {other:?}"),
    }
}

/// A single file over a channel is received as `ChannelReceived::File`.
#[tokio::test]
async fn receive_on_channel_dispatches_a_file() {
    let mut p = establish().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..80_000u32).map(|i| (i % 251) as u8).collect();
    let path = src.path().join("m.bin");
    std::fs::write(&path, &data).unwrap();

    let storage_s = FsStorage::new();
    let storage_r = FsStorage::new();
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx_s, _a) = unbounded_channel::<Progress>();
    let (ptx_r, _b) = unbounded_channel::<Progress>();
    let dst_dir = dst.path().to_string_lossy().into_owned();

    let send = send_file_on_session(
        &p.a,
        &storage_s,
        SendRequest {
            transfer_id: "t".into(),
            name: "m.bin".into(),
            path: path.to_string_lossy().into_owned(),
            size: data.len() as u64,
            chunk_size: 8192,
        },
        &ctrl_s,
        &ptx_s,
        0,
    );
    let recv = async {
        let inc = p.b_incoming.recv().await.expect("incoming");
        receive_on_channel(inc, &p.b, &storage_r, &dst_dir, &ctrl_r, &ptx_r).await
    };
    let (sr, rr) = tokio::join!(send, recv);
    assert_eq!(sr.expect("send"), TransferOutcome::Completed);
    match rr.expect("recv") {
        ChannelReceived::File(f) => {
            assert_eq!(f.bytes, data.len() as u64);
            assert_eq!(std::fs::read(dst.path().join("m.bin")).unwrap(), data);
        }
        other => panic!("expected file, got {other:?}"),
    }
}
