# File transfer

Streaming, chunked file transfer that is independent of the transport. It
moves bytes over any `Link` (QUIC/TCP/…) and any `StorageProvider`
(filesystem/…), so it is fully testable with in-memory links and temp files.

```
send_file:   StorageProvider.open_read(stream) → chunk → Link.send_frame
receive_file: Link.recv_frame → chunk → StorageProvider.open_write(stream)
```

## Crates

- **`peerbeam-storage-fs`** — filesystem `StorageProvider`. Streamed
  `open_read(offset)` / `open_write` via `tokio::fs` bridged to the domain's
  `futures` IO traits. Never buffers a whole file.
- **`peerbeam-transfer`** — the transfer mechanics: `protocol` (pure wire
  codec), `control` (pause/cancel handle), `stream` (`send_file` /
  `receive_file`).

## Wire protocol

One ordered sequence of frames per transfer:

```
Meta(name,size,chunk_size)  →  Chunk … Chunk  →  Control::Complete
```

Chunk bytes ride in the raw `Frame::payload` (no base64, no JSON wrapper) —
zero per-chunk bloat. `Meta` and `Control` (`Ack` / `Complete` / `Cancel`)
are small JSON payloads. Because a `Link` preserves order, chunks need no
index; the receiver appends in arrival order.

## Requirements met

| Requirement | How |
|---|---|
| Unlimited file size | streamed both directions; no size assumption |
| Never load into RAM | peak memory = one `chunk_size` buffer per direction; the bounded link channel caps in-flight frames |
| Chunked | `SendRequest::chunk_size` bounds each read/frame |
| Progress | a `Progress` emitted per chunk on an mpsc channel (both directions) |
| Cancel | `TransferControl::cancel` — send loop aborts before its next chunk and sends `Control::Cancel`; receiver stops on that frame |
| Pause | `TransferControl::pause` / `resume` — send loop blocks between chunks until resumed (or cancelled) |
| Retry | each frame send retried up to N times with linear backoff on transient link errors |

## Control

`TransferControl` is a cloneable handle (shared `Arc` state) the UI keeps
while the transfer task holds another. The send loop checks it every chunk:
blocks while paused (woken on resume/cancel), aborts promptly on cancel.

## Testing

- **Unit**: protocol codec round-trips (meta/control/chunk, garbage
  rejection); control flag transitions + async pause/resume wakeup;
  filesystem storage round-trip incl. offset reads.
- **Integration** (`tests/transfer.rs`): an in-memory **bounded** `Link`
  (real backpressure) + real temp files:
  - 2 MiB file in 64 KiB chunks → byte-for-byte match, many progress updates,
    final progress equals size.
  - cancel-while-paused → both sides end `Cancelled`.
  - pause → resume → `Completed`, file matches.
  - flaky link failing the first sends → retry recovers, file matches.

## Not yet (future milestones)

Resume from checkpoint, parallel chunks/files, per-chunk checksums,
compression, and encryption are separate layers that compose onto this same
pipeline (see the transfer architecture). This milestone is the streaming
core: one file, one ordered link, memory-bounded, controllable.
