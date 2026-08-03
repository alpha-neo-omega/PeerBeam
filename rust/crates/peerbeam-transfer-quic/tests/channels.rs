//! Multiplexing over two real QUIC endpoints: one connection, several
//! independent channels (bidirectional streams) via `QuicChannels`.

use bytes::Bytes;
use chrono::Utc;
use futures::StreamExt;

use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ChannelTransport, Frame, FrameKind};
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
    }
}

fn frame(bytes: &'static [u8]) -> Frame {
    Frame {
        kind: FrameKind::Control,
        payload: Bytes::from_static(bytes),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiplexes_independent_channels_over_real_quic() {
    let server = QuicTransport::new().unwrap();
    let (addr, mut incoming) = server
        .serve_channels_on("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = QuicTransport::new().unwrap();
    let route = direct_route("127.0.0.1", addr.port());

    // The server accepts only while its stream is polled, so dial and accept
    // must run concurrently.
    let sess = session();
    let (dialed, accepted) = tokio::join!(client.dial_channels(&route, &sess), incoming.next());
    let ct_client = dialed.unwrap();
    let ct_server = accepted.expect("a connection").unwrap();

    // Two independent channels (bidirectional streams) on the one connection.
    let mut c1 = ct_client.open_stream().await.unwrap();
    c1.send_frame(frame(b"one")).await.unwrap();
    let mut c2 = ct_client.open_stream().await.unwrap();
    c2.send_frame(frame(b"two")).await.unwrap();

    let mut s1 = ct_server.accept_stream().await.unwrap().expect("stream 1");
    let f1 = s1.recv_frame().await.unwrap().expect("frame on 1");
    assert_eq!(&f1.payload[..], b"one");

    let mut s2 = ct_server.accept_stream().await.unwrap().expect("stream 2");
    let f2 = s2.recv_frame().await.unwrap().expect("frame on 2");
    assert_eq!(&f2.payload[..], b"two");

    // Channels are independent: replying on channel 2 does not touch channel 1.
    s2.send_frame(frame(b"pong-two")).await.unwrap();
    let r2 = c2.recv_frame().await.unwrap().expect("reply on 2");
    assert_eq!(&r2.payload[..], b"pong-two");

    // Closing the whole transport ends the stream sequence cleanly.
    ct_client.close().await.unwrap();
}
