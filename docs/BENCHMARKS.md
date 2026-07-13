# Benchmarks (baseline)

Baseline measurements of the Rust engine + CLI. **No performance optimization
applied** — these are the starting numbers.

- **Host**: Intel i5-1135G7 (8 threads), Linux, warm cache.
- **Build**: `--release`.
- **Method**: Python `perf_counter` (wall, 25-run median) + `getrusage` peak
  RSS; throughput from `peerbeam benchmark`.
- **Scope**: headless engine/CLI only. Flutter (UI startup/RAM/frames) and
  on-device Android are **not** covered — they need a device/display.

## Results

| Metric | Measurement | Value |
|---|---|---|
| Startup | `peerbeam --version` | **1.6 ms** (median) |
| Startup | `peerbeam status` (config + keygen) | **1.7 ms** |
| RAM, idle | peak RSS (`status`/`discover`) | **~13 MB** |
| RAM, transfer | peak RSS (loopback, any size) | **~13 MB** (streams) |
| CPU / crypto | AES-256-GCM seal | **~900 MiB/s** (1 core) |
| CPU / crypto | AES-256-GCM open | **~1010 MiB/s** (1 core) |
| Transfer | loopback 512 MiB (in-process link) | **~490 MiB/s** |
| Discovery | `discover --timeout 2` overhead | **~5 ms** |
| Binary | release `peerbeam` on disk | **4.5 MB** |

## Notes

- **Startup** is ~300× under the 500 ms target — but this is the native CLI,
  not the Flutter app (unmeasured).
- **Transfer RAM is flat at ~13 MB regardless of file size** — confirms the
  transfer streams (chunk-bounded), holding no whole file in memory.
- **Loopback ≠ network.** It's the local pipeline ceiling: per-chunk copy
  through an in-process channel + **whole-file SHA-256 on both ends** +
  destination disk write. No QUIC/TLS/auth on this path yet.
- **The pipeline bottleneck is hashing, not crypto.** Loopback (~490 MiB/s,
  no encryption) is *slower* than AES-GCM alone (~900–1010 MiB/s), so the
  double SHA-256 verification + disk I/O dominates — worth noting before any
  optimization pass.

## Benchmark-harness fixes applied

Two defects in the first run were corrected so the numbers are trustworthy
(these are bench-harness fixes, not product optimization):

1. **Crypto units** — 64 KiB/iteration was counted as 64 MiB (1024× inflation,
   showing an impossible ~10⁶ MiB/s). Fixed the MiB accounting; also added
   `black_box` + per-iteration nonces to prevent future loop elision.
2. **Loopback RAM** — the harness built the sample file with a single
   full-size `vec![…; size]` (so a 256 MiB run reported ~260 MB RSS, measuring
   the harness, not the transfer). Now streamed in 1 MiB blocks → ~13 MB.

## Optimization pass 1 — send-path allocation & framing

**Goal:** attack the bottleneck the baseline flagged. **Method:** confirm where
the time actually goes, then remove the largest *removable* cost, measuring
before/after on the same host with the same command.

### Where the time actually goes (profiling, not guessing)

| Probe | Command | Result |
|---|---|---|
| SHA-256 (1 core) | `peerbeam benchmark hash` | **~1319 MiB/s** (SHA-NI already on) |
| OpenSSL SHA-256 | `openssl speed -evp sha256` | ~1210 MiB/s (agrees) |
| AES-256-GCM | `peerbeam benchmark crypto` | ~900 / ~1010 MiB/s |
| Loopback, chunk sweep | `benchmark loopback --chunk {64..16384}` | 448 → 532 MiB/s |

Findings that redirected the work:

1. **Hashing is *not* the low-hanging fruit** — the backlog said "use
   hardware-accelerated SHA", but `sha2` already autodetects SHA-NI
   (~1.3 GiB/s). Nothing to gain there.
2. **Loopback (~500 MiB/s) is bounded by two things that can't be removed:**
   the *mandatory* whole-file SHA-256 on **both** ends (integrity), and memory
   bandwidth (both hashers + the channel copy run concurrently). Each hash pass
   alone is ~1.3 GiB/s; two concurrent passes + I/O land the pipeline near half
   that. This is a real ceiling, not waste.
3. **Chunk size matters a little** (64 KiB → 16 MiB ≈ +19%); the product
   default (1 MiB) is already on the plateau.

### What was removed

The one clearly-removable cost on the hot path: the sender allocated a fresh
`Bytes` **and memcpy'd** every chunk (`Bytes::copy_from_slice`) on top of the
read. Changed the send loop (single-file **and** folder) to read into an owned
buffer that is **moved** into the frame (`chunk_frame_owned` + `Bytes::from`),
plus a `read_fill` that coalesces short reads into full-size chunks. Net: one
fewer full-buffer memcpy per chunk, and fewer/larger frames. No behaviour change
(same wire format, same checksums; all transfer tests pass).

### Before / after

Same host, `benchmark loopback --size 1024 --chunk 1024`, 5 runs each:

| | mean | median | range |
|---|---|---|---|
| Before | 497 MiB/s | 499 | 477–508 |
| After | **506 MiB/s** | 506 | 500–514 |

**Honest verdict: ~+1.5–2% on loopback — within run-to-run noise.** The copy was
real but small next to the dual-hash + memory-bandwidth ceiling, exactly as the
profiling predicted. The change still earns its place: it lowers CPU-per-byte on
the send path (one less allocation + memcpy per chunk), which matters more on a
real NIC path where the copy competes with syscalls and crypto — but that number
can't be measured until the QUIC `TransferProvider` exists.

**Conclusion:** the loopback pipeline is at its practical ceiling given the
integrity model. Further speedups require either a weaker integrity guarantee
(don't hash both ends — rejected) or the real network path (where the bottleneck
moves off the CPU). No further micro-optimization is worthwhile here.

## QUIC transport (real network, loopback)

The QUIC `TransferProvider` (`peerbeam-transfer-quic`) measured over two real
endpoints on `127.0.0.1` via `peerbeam benchmark quic`:

| Metric | Command | Value |
|---|---|---|
| Throughput | `benchmark quic --size 512 --chunk 1024` | **~430 MiB/s** (i5-1135G7) |
| Connect latency | (handshake, same run) | **~0.7 ms** |

This is a full real transfer: QUIC (TLS 1.3 + UDP) + the transfer engine's
dual SHA-256. It lands at ~85% of the transport-free in-process loopback
(~500 MiB/s), the difference being TLS record crypto + UDP/framing + the QUIC
stack. Loopback (127.0.0.1) has no propagation delay, MTU limits, or loss —
real-LAN/WAN numbers will differ and are the next thing to measure once QUIC is
wired into `send`/`receive`.

Reproduce:

```
target/release/peerbeam benchmark quic --size 512 --chunk 1024
```

## Not yet measured

- Flutter app startup / RAM / jank (needs a device or emulator).
- On-device Android transfer + battery.
- **Real LAN/WAN transfer speed** — the QUIC transport is measured on
  *loopback* above; over-the-wire numbers (propagation delay, MTU, loss) still
  need two physical machines.

## Reproduce

```
cargo build --release -p peerbeam-cli
target/release/peerbeam benchmark crypto
target/release/peerbeam benchmark hash
target/release/peerbeam benchmark loopback --size 1024 --chunk 1024
target/release/peerbeam discover --timeout 2 --json
```

## Optimization backlog

- ~~Use hardware-accelerated SHA~~ — **already on** (`sha2` autodetects SHA-NI,
  ~1.3 GiB/s); confirmed via `benchmark hash`.
- ~~Remove per-chunk send-path copy~~ — **done** (see *Optimization pass 1*).
- Re-benchmark over the real QUIC path once it exists (with `SecureLink`
  AES-GCM per frame) — the only number that reflects true transfer speed, and
  the only place further CPU-per-byte savings will show up.
- Vectored / batched I/O only if measurement on a real network link shows the
  send path (not the NIC) is the limiter.
