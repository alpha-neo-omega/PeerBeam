//! Pumping an unbounded byte stream over a [`Link`], both directions.
//!
//! ```text
//! Chunk … Chunk  →  Control::Complete{checksum}   S→R
//! Control::Verify{ok}                             R→S
//! ```
//!
//! Both loops are the transfer engine's, reused: [`read_fill`] coalesces short
//! reads into full chunks, [`send_with_retry`] frames them, [`recv_verify`]
//! closes. Peak memory is one chunk buffer per direction whatever the stream's
//! length (I10) — and its length is never known, which is why there is no
//! `Meta` frame and no progress total.

use bytes::Bytes;
use futures::io::{AsyncRead, AsyncWrite};
use futures::AsyncWriteExt;
use sha2::{Digest, Sha256};

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{FrameKind, Link};

use crate::protocol::{chunk_frame_owned, control_frame, parse_control, Control};
use crate::stream::{read_fill, recv_verify, send_with_retry, to_hex};

/// Chunk size a pipe uses when its caller has no configured preference.
///
/// Not a wire constant: the receiver appends whatever arrives in arrival order
/// and never compares sizes, so the two ends may disagree freely and a future
/// build may change this without breaking anyone.
pub const PIPE_CHUNK: u32 = 64 * 1024;

/// How much of a stream moved, and in how many frames.
///
/// `chunks` is not decoration: it is the accounting that makes the streaming
/// property *assertable*. A run that buffered the whole stream and sent it in
/// one go would report `chunks == 1` for any size, so a test that pipes N bytes
/// in fixed chunks and checks the count is a direct structural proof that
/// neither end accumulated (see `tests/pipe_gates.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipeStats {
    /// Payload bytes moved (frame overhead excluded).
    pub bytes: u64,
    /// Chunk frames moved.
    pub chunks: u64,
}

/// Read `src` to EOF and stream it to the peer over `link` in `chunk_size`
/// pieces, then close with a checksum and wait for the peer's verdict.
///
/// **Binary-safe and byte-exact.** Nothing here inspects, decodes, splits on
/// newlines or otherwise touches the bytes: a chunk is whatever the reader gave,
/// framed raw. Embedded NULs, invalid UTF-8 and arbitrary binary all survive,
/// which is the entire point of `tar cz . | peerbeam pipe --to laptop`.
///
/// **EOF is the terminator.** `src` returning 0 ends the stream; there is no
/// length to announce up front and none is announced.
///
/// Frames are sent with **no retries**. A retry would re-send a chunk that has
/// no sequence number, and a stream has no resume to fall back on, so a
/// transient failure here is terminal by nature — reporting it honestly beats a
/// retry that could only duplicate bytes.
pub async fn send_pipe(
    link: &mut dyn Link,
    src: &mut (dyn AsyncRead + Unpin + Send),
    chunk_size: u32,
) -> Result<PipeStats> {
    let chunk = chunk_size.max(1) as usize;
    let mut hasher = Sha256::new();
    let mut stats = PipeStats::default();

    loop {
        // A fresh owned buffer per chunk, read-filled to full `chunk` size
        // (short reads coalesced) and moved straight into the frame — no
        // per-chunk copy, and never more than one chunk held at a time.
        let mut buf = vec![0u8; chunk];
        let n = read_fill(src, &mut buf).await?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        hasher.update(&buf);
        send_with_retry(link, chunk_frame_owned(Bytes::from(buf)), 0).await?;
        stats.bytes += n as u64;
        stats.chunks += 1;
    }

    let checksum = to_hex(&hasher.finalize());
    send_with_retry(link, control_frame(&Control::Complete { checksum }), 0).await?;
    if recv_verify(link).await? {
        Ok(stats)
    } else {
        Err(DomainError::Integrity(
            "the receiver reported a checksum mismatch — what it wrote out is not what was piped in"
                .into(),
        ))
    }
}

/// Receive a byte stream from `link`, writing it to `out` as it arrives.
///
/// Each chunk is written **and flushed** before the next frame is read: nothing
/// accumulates here (I10), and a consumer downstream of `out` — the `> file` or
/// the next process in the shell pipeline — sees bytes while the stream is still
/// running rather than in one burst at the end.
///
/// **A closed link is an error, never a clean end.** Only an explicit
/// `Control::Complete` ends the stream, and its SHA-256 is checked against what
/// was actually written. Treating a dropped connection as EOF would mean
/// `peerbeam pipe --listen > project.tgz` exiting `0` on a truncated archive,
/// which is exactly the silent corruption this terminator exists to prevent.
/// The bytes are already out by then — a pipe cannot un-write stdout — so the
/// error is the report, and the caller's exit code is how a script learns.
pub async fn receive_pipe(
    link: &mut dyn Link,
    out: &mut (dyn AsyncWrite + Unpin + Send),
) -> Result<PipeStats> {
    let mut hasher = Sha256::new();
    let mut stats = PipeStats::default();

    loop {
        let Some(frame) = link.recv_frame().await? else {
            return Err(DomainError::Transfer(
                "the pipe closed before the sender finished — what was written out is incomplete"
                    .into(),
            ));
        };
        match frame.kind {
            FrameKind::Chunk => {
                out.write_all(&frame.payload)
                    .await
                    .map_err(|e| DomainError::Storage(format!("write piped bytes: {e}")))?;
                out.flush()
                    .await
                    .map_err(|e| DomainError::Storage(format!("flush piped bytes: {e}")))?;
                hasher.update(&frame.payload);
                stats.bytes += frame.payload.len() as u64;
                stats.chunks += 1;
            }
            FrameKind::Control => match parse_control(&frame)? {
                Control::Complete { checksum } => {
                    // Flush BEFORE answering: the sender reads our `Verify` as
                    // "your bytes are out", so it must not be sent while any of
                    // them are still sitting in a buffer here.
                    out.flush()
                        .await
                        .map_err(|e| DomainError::Storage(format!("flush piped bytes: {e}")))?;
                    let ok = to_hex(&hasher.clone().finalize()) == checksum;
                    // Best-effort: the sender may already be gone, and our own
                    // verdict below does not depend on the reply arriving.
                    let _ = send_with_retry(link, control_frame(&Control::Verify { ok }), 0).await;
                    return if ok {
                        Ok(stats)
                    } else {
                        Err(DomainError::Integrity(
                            "the piped bytes did not match the sender's checksum — what was \
                             written out is corrupt"
                                .into(),
                        ))
                    };
                }
                // Either side may abandon a pipe; the sender does so by dying,
                // which the `None` arm above reports, but an explicit `Cancel`
                // is handled rather than silently skipped.
                Control::Cancel => return Err(DomainError::Cancelled),
                // A pipe negotiates no resume, signals no pause and sends no
                // verdict of its own. Skipping an unexpected control frame
                // rather than failing on it is §6's fail-safe rule.
                Control::ResumeAck { .. }
                | Control::Verify { .. }
                | Control::Pause
                | Control::Resume => {}
            },
            // Not part of a pipe's framing. Ignored, never written out: `out` is
            // a shell's stdout and only chunk payloads may reach it.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default chunk size bounds a pipe's memory per direction, so it is
    /// worth pinning that it is a sane fixed value rather than something
    /// derived from a stream length nobody knows.
    #[test]
    fn the_default_chunk_is_64_kib() {
        assert_eq!(PIPE_CHUNK, 64 * 1024);
    }

    /// A zero chunk size would otherwise mean an infinite loop of empty reads;
    /// `max(1)` is the guard, checked here because the value reaches this
    /// module from configuration.
    #[tokio::test]
    async fn a_zero_chunk_size_still_makes_progress() {
        use crate::protocol::parse_control;
        use peerbeam_domain::port::Frame;
        use std::collections::VecDeque;

        struct Recorder {
            sent: Vec<Frame>,
            inbound: VecDeque<Frame>,
        }
        #[async_trait::async_trait]
        impl Link for Recorder {
            async fn send_frame(&mut self, f: Frame) -> Result<()> {
                self.sent.push(f);
                Ok(())
            }
            async fn recv_frame(&mut self) -> Result<Option<Frame>> {
                Ok(self.inbound.pop_front())
            }
            async fn close(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let verify = control_frame(&Control::Verify { ok: true });
        let mut link = Recorder {
            sent: Vec::new(),
            inbound: VecDeque::from([verify]),
        };
        let mut src = futures::io::Cursor::new(vec![7u8, 8, 9]);
        let stats = send_pipe(&mut link, &mut src, 0).await.expect("send");
        assert_eq!(stats.bytes, 3);
        assert_eq!(
            stats.chunks, 3,
            "clamped to one byte per chunk, not stalled"
        );
        let last = link.sent.last().expect("a Complete frame");
        assert!(matches!(parse_control(last), Ok(Control::Complete { .. })));
    }
}
