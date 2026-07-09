//! End-to-end clipboard transfer over an in-memory link: text, URL, code,
//! and image (binary, chunk-streamed) round-trips.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use peerbeam_domain::entity::{ClipboardItem, ClipboardKind};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Frame, Link};
use peerbeam_transfer::{receive_clipboard, send_clipboard};

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

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

async fn roundtrip(item: ClipboardItem) -> ClipboardItem {
    let (mut la, mut lb) = MemLink::pair(4);
    let send = send_clipboard(&mut la, &item, 3);
    let recv = receive_clipboard(&mut lb);
    let (rs, rr) = tokio::join!(send, recv);
    rs.unwrap();
    rr.unwrap()
}

#[tokio::test]
async fn roundtrips_plain_text() {
    let item = ClipboardItem::text("just some words".into(), t0());
    let got = roundtrip(item.clone()).await;
    assert_eq!(got, item);
    assert_eq!(got.kind, ClipboardKind::Text);
}

#[tokio::test]
async fn roundtrips_url() {
    let item = ClipboardItem::text("https://example.com/path".into(), t0());
    let got = roundtrip(item.clone()).await;
    assert_eq!(got.kind, ClipboardKind::Url);
    assert_eq!(got.mime, "text/uri-list");
    assert_eq!(got, item);
}

#[tokio::test]
async fn roundtrips_code() {
    let item = ClipboardItem::text("fn main() {\n    let x = 1;\n}".into(), t0());
    let got = roundtrip(item.clone()).await;
    assert_eq!(got.kind, ClipboardKind::Code);
    assert_eq!(got, item);
}

#[tokio::test]
async fn roundtrips_image_bytes() {
    // A "PNG" of 200 KiB of pattern data, streamed as chunks.
    let bytes: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
    let item = ClipboardItem::image(bytes.clone(), "image/png", t0());
    let got = roundtrip(item.clone()).await;
    assert_eq!(got.kind, ClipboardKind::Image);
    assert_eq!(got.mime, "image/png");
    assert_eq!(got.as_bytes(), Some(&bytes[..]));
    assert_eq!(got, item);
}
