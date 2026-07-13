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

## Not yet measured

- Flutter app startup / RAM / jank (needs a device or emulator).
- On-device Android transfer + battery.
- **Real network transfer speed** — requires the QUIC `TransferProvider`; the
  in-process loopback is a stand-in until then.

## Reproduce

```
cargo build --release -p peerbeam-cli
target/release/peerbeam benchmark crypto
target/release/peerbeam benchmark loopback --size 512
target/release/peerbeam discover --timeout 2 --json
```

## Optimization backlog (deferred — do not act yet)

- Overlap / stream the SHA-256 with I/O, or use hardware-accelerated SHA.
- Re-benchmark over the real QUIC path once it exists (with `SecureLink`
  AES-GCM per frame) — the only number that reflects true transfer speed.
- Larger chunk sizes / vectored I/O once measured on a network link.
