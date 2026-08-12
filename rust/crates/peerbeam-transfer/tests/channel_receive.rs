//! `receive_on_channel` (M8.5): a PeerSession receiver dispatches a file **or** a
//! folder over one incoming transfer channel by peeking the first frame — the
//! folder-capable receive primitive the frontend cutover needs. Real per-channel
//! crypto + real handshake over an in-memory transport.

mod common;

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::unbounded_channel;

use common::MemTransport;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, Frame, FrameKind, Link, TrustStore};
use peerbeam_domain::session::{Capability, CapabilitySet, ChannelId, ChannelType};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    peek_incoming_meta, receive_on_channel, send_file_on_session, send_folder_on_session,
    ChannelReceived, FolderSendRequest, Identity, IncomingStreamChannel, PeerSession, SendRequest,
    SessionConfig, SessionHandle, SessionRole, TransferControl, TransferOutcome, TransferPreview,
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

// ── peek_incoming_meta ──────────────────────────────────────────
//
// The correlation crux: a receiver must be able to learn the sender's transfer
// id / name / size *before* it registers the transfer and prompts the user, and
// the transfer must then run exactly as if nothing had been read. These tests
// pin both halves — what the peek reports, and that the receive is untouched.

/// A link that yields a scripted sequence of frames and then EOF, so the peek's
/// decode/replay behaviour can be exercised on frames a real sender would never
/// produce (garbage, wrong kind, nothing at all).
struct ScriptedLink {
    frames: VecDeque<Frame>,
}

#[async_trait]
impl Link for ScriptedLink {
    async fn send_frame(&mut self, _frame: Frame) -> DResult<()> {
        Ok(())
    }
    async fn recv_frame(&mut self) -> DResult<Option<Frame>> {
        Ok(self.frames.pop_front())
    }
    async fn close(&mut self) -> DResult<()> {
        Ok(())
    }
}

fn scripted(frames: Vec<Frame>) -> IncomingStreamChannel {
    IncomingStreamChannel {
        channel: ChannelId::new(1),
        channel_type: TRANSFER,
        link: Box::new(ScriptedLink {
            frames: frames.into(),
        }),
    }
}

/// Drain everything the (possibly replaying) link will yield, as the kinds and
/// payloads a receive loop would have seen.
async fn drain(mut ch: IncomingStreamChannel) -> Vec<(FrameKind, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(Some(f)) = ch.link.recv_frame().await {
        out.push((f.kind, f.payload.to_vec()));
    }
    out
}

/// Peeking a real file transfer reports the sender's id/name/size, and the
/// subsequent `receive_on_channel` still writes every byte — the peeked Meta
/// frame is replayed, not consumed.
#[tokio::test]
async fn peek_reports_file_meta_and_the_receive_is_unaffected() {
    let mut p = establish().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let path = src.path().join("report.pdf");
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
            transfer_id: "sender-chose-this-id".into(),
            name: "report.pdf".into(),
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
        let (inc, preview) = peek_incoming_meta(inc).await;
        let received = receive_on_channel(inc, &p.b, &storage_r, &dst_dir, &ctrl_r, &ptx_r).await;
        (preview, received)
    };
    let (sr, (preview, rr)) = tokio::join!(send, recv);
    assert_eq!(sr.expect("send"), TransferOutcome::Completed);

    assert_eq!(preview.transfer_id, "sender-chose-this-id");
    assert_eq!(preview.name, "report.pdf");
    assert_eq!(preview.size, data.len() as u64);
    assert!(!preview.is_folder);

    // ...and the transfer itself is byte-identical to one that was never peeked.
    match rr.expect("recv") {
        ChannelReceived::File(f) => {
            assert_eq!(f.bytes, data.len() as u64);
            assert_eq!(f.transfer_id, "sender-chose-this-id");
            assert_eq!(std::fs::read(dst.path().join("report.pdf")).unwrap(), data);
        }
        other => panic!("expected file, got {other:?}"),
    }
}

/// The folder half: a manifest peek reports the root name and the summed size,
/// and `receive_on_channel` still dispatches to the folder receiver and writes
/// the whole tree.
#[tokio::test]
async fn peek_reports_folder_manifest_and_the_receive_is_unaffected() {
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
            transfer_id: "folder-id".into(),
            root_path: src.path().to_string_lossy().into_owned(),
            chunk_size: 2048,
        },
        &ctrl_s,
        &ptx_s,
        0,
    );
    let recv = async {
        let inc = p.b_incoming.recv().await.expect("incoming");
        let (inc, preview) = peek_incoming_meta(inc).await;
        let received = receive_on_channel(inc, &p.b, &storage_r, &dst_dir, &ctrl_r, &ptx_r).await;
        (preview, received)
    };
    let (sr, (preview, rr)) = tokio::join!(send, recv);
    assert_eq!(sr.expect("send folder"), TransferOutcome::Completed);

    assert_eq!(preview.transfer_id, "folder-id");
    assert_eq!(preview.name, root_name);
    assert_eq!(preview.size, 5 + 4096, "manifest sizes are summed");
    assert!(preview.is_folder);

    match rr.expect("recv") {
        ChannelReceived::Folder(fr) => {
            assert_eq!(fr.files, 2);
            assert_eq!(fr.transfer_id, "folder-id");
            assert_eq!(
                std::fs::read(dst.path().join(&root_name).join("a.txt")).unwrap(),
                b"alpha"
            );
        }
        other => panic!("expected folder, got {other:?}"),
    }
}

/// Fail-soft: a first frame that cannot be decoded (a Meta frame whose payload
/// is not `TransferMeta` JSON — a peer can send anything) yields an empty
/// preview, and the undecodable frame is still replayed so the receive path
/// sees exactly the stream the peer actually sent.
#[tokio::test]
async fn peek_on_an_undecodable_first_frame_learns_nothing_but_still_replays_it() {
    let junk = Frame {
        kind: FrameKind::Meta,
        payload: bytes::Bytes::from_static(b"not json at all"),
    };
    let tail = Frame {
        kind: FrameKind::Chunk,
        payload: bytes::Bytes::from_static(b"payload"),
    };
    let (ch, preview) = peek_incoming_meta(scripted(vec![junk, tail])).await;

    assert_eq!(preview, TransferPreview::default(), "learned nothing");
    assert_eq!(
        drain(ch).await,
        vec![
            (FrameKind::Meta, b"not json at all".to_vec()),
            (FrameKind::Chunk, b"payload".to_vec()),
        ],
        "the peeked frame must be replayed ahead of the rest, in order"
    );
}

/// Fail-soft: a Control frame that is not a folder manifest (e.g. a `Cancel`
/// the peer opened with) is likewise "nothing learned" — and still replayed.
#[tokio::test]
async fn peek_on_a_non_manifest_control_frame_learns_nothing_but_still_replays_it() {
    let ctrl = Frame {
        kind: FrameKind::Control,
        payload: bytes::Bytes::from_static(br#""Cancel""#),
    };
    let (ch, preview) = peek_incoming_meta(scripted(vec![ctrl])).await;

    assert_eq!(preview, TransferPreview::default());
    assert_eq!(
        drain(ch).await,
        vec![(FrameKind::Control, br#""Cancel""#.to_vec())]
    );
}

/// Fail-soft: a channel that closes before any data yields an empty preview and
/// a link that is handed back untouched (nothing was read, so nothing is owed).
#[tokio::test]
async fn peek_on_an_empty_channel_learns_nothing_and_returns_the_link_unchanged() {
    let (ch, preview) = peek_incoming_meta(scripted(Vec::new())).await;
    assert_eq!(preview, TransferPreview::default());
    assert!(drain(ch).await.is_empty());
}

/// A hostile sender's name never survives the peek as a path: the preview
/// carries the same single, sanitized component `receive_file` would write, so
/// an approval prompt cannot be made to display (or a caller to persist)
/// something that escapes the save directory.
#[tokio::test]
async fn peek_sanitizes_a_traversing_file_name_to_a_bare_component() {
    let meta = Frame {
        kind: FrameKind::Meta,
        payload: bytes::Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "transfer_id": "x",
                "name": "../../../etc/passwd",
                "size": 10u64,
                "chunk_size": 1024u32,
            }))
            .unwrap(),
        ),
    };
    let (_ch, preview) = peek_incoming_meta(scripted(vec![meta])).await;
    assert_eq!(preview.name, "passwd", "only the base component survives");
    assert_eq!(preview.transfer_id, "x");
    assert_eq!(preview.size, 10);
}
