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

## Recursive folder transfer

Built on the single-file core, adding structure preservation and resume.

### Wire protocol

```
Manifest(root, [(rel_path, size) …])        S→R   announce the tree
ResumeState([bytes_already_on_disk …])       R→S   what the receiver has
for each not-yet-complete file:
  FileHeader(index, rel_path, size, offset)  S→R
  Chunk … Chunk                              S→R   (streamed from offset)
  FileEnd(index)                             S→R
Complete                                      S→R
```

Folder messages are small JSON in `Control` frames; file bytes stay raw in
`Chunk` frames. Chunks between a `FileHeader` and `FileEnd` belong to that
file (the link is ordered).

### Preserve structure

Each file keeps its path relative to the folder root; the receiver recreates
the tree under `dest_dir/<root>/…`. Relative paths are **sanitized** — empty,
`.`, `..`, and absolute components are rejected — so a malicious manifest
cannot escape the destination.

### Resume

The receiver inspects the destination and reports, per file, how many bytes
it already has (`StorageProvider::size`). The sender then:

- **skips** files already complete (`have == size`) — no `FileHeader`, no
  chunks; and
- **resumes** partial files by streaming from `offset` (`open_read(offset)`)
  while the receiver **appends** (`open_append`).

So a re-run after an interruption sends only what is missing. Requires three
storage-port methods: `list_files`, `size`, `open_append`.

### Testing

- **Unit**: folder-message codec round-trip; path sanitization (traversal
  rejected); `dest_path` composition; plus `FsStorage` `list_files` / `size`
  / `open_append`.
- **Integration** (`tests/folder.rs`): nested tree transferred with
  structure + content preserved; **resume** with a pre-populated destination
  asserting *exactly* the missing remainder crosses the wire (complete file
  skipped, partial appended); cancel-then-rerun completing the tree.

## Interrupted-transfer recovery

Single-file transfers negotiate a resume offset and verify integrity, and a
recovery driver reconnects and resumes automatically.

### Resume + integrity (single file)

The single-file protocol gained a resume handshake and a checksum:

```
Meta(name, size, chunk_size)   S→R
ResumeAck(offset)              R→S   receiver's bytes-on-disk
Chunk … Chunk                 S→R   streamed from offset
Complete(checksum)            S→R   whole-file SHA-256
Verify(ok)                    R→S   receiver's integrity verdict
```

- **Resume** — the receiver reports how many bytes it already has; the sender
  streams from that offset and the receiver appends. The sender seeds its
  hash with the already-present prefix (read once) so the whole-file checksum
  is correct even on a resumed run.
- **Integrity** — both sides compute a streaming SHA-256; the receiver
  compares against `Complete`'s checksum and reports `Verify(ok)`. A mismatch
  surfaces as `DomainError::Integrity` on both ends — corrupt data is never
  silently accepted.

### State persistence (`peerbeam-reliability-fs`)

`FsReliability` implements the `ReliabilityStore` port: SHA-256 checksums and
per-transfer checkpoints written as `<dir>/<id>.json`. A checkpoint records
the in-flight session so a transfer can be resumed even after a **process
restart** (a fresh store reads the same file). Saved when a recoverable
transfer starts, cleared on success.

### Automatic retry (`send_file_recover` / `receive_file_recover`)

A `LinkFactory` supplies a fresh `Link` on demand. The recovery drivers retry
across new links up to `max_attempts` with backoff; because each attempt
re-runs the resume handshake, it continues from the receiver's on-disk bytes
rather than restarting. Cancellations and integrity failures are terminal
(never retried).

### Testing

- **Unit**: `FsReliability` — known-answer SHA-256, save/load/resume/clear
  round-trip, and survival across a store reopen (restart).
- **Integration**:
  - `integrity.rs` — a link that corrupts one chunk → both sides fail with
    `Integrity`; a clean transfer verifies OK.
  - `recovery.rs` — a broker that fails the first connect on each side, then
    succeeds; with a pre-existing 20 KiB partial of a 50 KiB file the drivers
    reconnect, resume, and complete — asserting **only the missing remainder
    crosses the wire**, the file matches, and the checkpoint is cleared.

## Not yet (future milestones)

Parallel chunks/files, per-chunk checksums, compression, and encryption are
separate layers that compose onto this same pipeline. Empty directories are
not transferred (only files and their parent paths).
