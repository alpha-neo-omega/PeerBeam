//! Large-file / unlimited-size streaming.
//!
//! Uses a synthetic `StorageProvider` that *generates* source bytes on demand
//! and *discards* received bytes — so an arbitrarily large payload flows
//! through the real send/receive pipeline while holding no full copy anywhere.
//! That is the structural proof of CLAUDE.md's "unlimited file size / never
//! load the whole file into RAM": there is nowhere in this test a buffer sized
//! to the payload, yet the transfer completes and the whole-file checksum
//! (computed on both ends) still agrees.

mod common;

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};

use common::MemLink;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::StorageProvider;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl, TransferOutcome};
use tokio::sync::mpsc;

/// A reader that yields `remaining` zero bytes then EOF — never allocates the
/// full stream.
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

/// A writer that counts bytes and discards them.
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

/// Storage whose source is a byte generator (`path` = "gen:<size>") and whose
/// destination is a bit-bucket. No disk, no whole-file buffer.
struct GenStorage;

#[async_trait]
impl StorageProvider for GenStorage {
    async fn open_write(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
        Ok(Box::new(NullWriter))
    }
    async fn open_append(&self, _path: &str) -> Result<Box<dyn AsyncWrite + Unpin + Send>> {
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
    async fn size(&self, _path: &str) -> Result<Option<u64>> {
        Ok(None) // fresh transfer, nothing on disk
    }
    async fn list_files(&self, _root: &str) -> Result<Vec<(String, u64)>> {
        Ok(Vec::new())
    }
    async fn finalize(&self, _temp: &str, dest: &str) -> Result<String> {
        Ok(dest.to_string())
    }
}

async fn stream_size(total: u64) {
    let storage = GenStorage;
    let (mut la, mut lb) = MemLink::pair(4); // tiny cap → bounded in-flight
    let ctrl_s = TransferControl::new();
    let ctrl_r = TransferControl::new();
    let (ptx, _prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "big".into(),
        name: "big.bin".into(),
        path: format!("gen:{total}"),
        size: total,
        chunk_size: 1024 * 1024,
    };
    let send = send_file(&mut la, &storage, req, &ctrl_s, &ptx, 3);
    let recv = receive_file(&mut lb, &storage, "/dev/null-dir", &ctrl_r, &ptx);
    let (rs, rr) = tokio::join!(send, recv);

    assert_eq!(rs.unwrap(), TransferOutcome::Completed);
    let rr = rr.unwrap();
    assert_eq!(rr.outcome, TransferOutcome::Completed);
    assert_eq!(rr.bytes, total, "receiver must account for every byte");
}

#[tokio::test]
async fn streams_128_mib_without_buffering_whole_file() {
    stream_size(128 * 1024 * 1024).await;
}

/// Beyond 4 GiB — proves 64-bit sizing (nothing truncates at `u32`). Ignored by
/// default because it hashes ~10 GiB total; run with `--ignored`.
#[tokio::test]
#[ignore = "slow: hashes ~10 GiB; run explicitly for the >4 GiB size check"]
async fn streams_over_four_gib() {
    stream_size(5 * 1024 * 1024 * 1024).await;
}
