# Performance Report (M5)

Host: Intel i5-1135G7 (8 threads), Linux, release build, warm cache.
Method: `peerbeam benchmark …` + `getrusage`/`/proc`. Loopback = in-process
`Link`; QUIC = real endpoints on 127.0.0.1. Real-network numbers still need two
physical machines (only loopback + a single Android↔Linux transfer measured).

## Core throughput
| Metric | Value |
|---|---|
| AES-256-GCM seal / open (1 core) | ~1307 / ~1429 MiB/s |
| SHA-256 (1 core, SHA-NI) | ~1643 MiB/s |
| Loopback transfer (in-process) | ~700 MiB/s |
| QUIC transfer (loopback, 256 MiB) | ~447 MiB/s |
| QUIC connect latency | ~0.7 ms |
| Large file over QUIC (11 GiB) | ~310 MiB/s, constant memory |

## Footprint
| Metric | Value |
|---|---|
| CLI startup (`--version`) | ~1–2 ms |
| RAM idle / during transfer | ~13 MB (streams; flat regardless of file size) |
| Release CLI binary | 8.1 MB |
| Engine cdylib (`libpeerbeam_ffi.so`) | 7.1 MB |
| Android arm64 engine `.so` | 6.8 MB |

## Discovery / concurrency
- Discovery overhead ~5 ms to first scan; real LAN discovery Android↔Linux
  verified (each sees the other within the 2 s scan interval).
- 8 simultaneous transfers complete with no cross-talk (test).

## Bottleneck analysis (unchanged from earlier pass)
Loopback is bounded by the **mandatory dual whole-file SHA-256** (both ends,
HW-accelerated ~1.6 GiB/s) + memory bandwidth, not crypto or the link. QUIC lands
~85 % of the transport-free loopback — the delta is TLS record crypto + UDP +
the QUIC stack. Both are inherent; no further micro-optimization is warranted
without a real-WAN measurement.

## Not measured here
- Real LAN/WAN throughput between two machines (loopback + one phone transfer
  only). Android battery impact. Per-platform desktop startup/RAM/jank.
