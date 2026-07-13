# Final Performance Review — M8

Release-gate performance pass. Complements [Benchmarks](BENCHMARKS.md) and
[Performance Report](PERFORMANCE_REPORT.md) with a fresh measurement taken this
session.

Legend: ✓ Verified here · 🟡 Code-reviewed · ⚪ Environment-limited.

## Headline measurement (this session)

```
$ peerbeam benchmark loopback --size 512
transferred 512 MiB in 0.71s = 726 MiB/s
```

✓ **726 MiB/s** end-to-end over the loopback path — encrypt → frame → transport
→ receive → verify. This is well above any LAN/Wi-Fi line rate, so on real
networks the transport, not PeerBeam's CPU/crypto, is the bottleneck. Measured
on the developer's Linux host; absolute numbers vary by CPU.

## Memory

🟡 Streaming by design: files move chunk-by-chunk (default 64 KiB) via
`read_fill`/`chunk_frame_owned`; the sender moves an owned buffer into each
frame with no per-chunk copy, and the receiver appends in arrival order.
**No file is ever fully loaded into RAM** — confirmed by code review of
`stream.rs`/`folder.rs`. Peak per-transfer buffering is O(chunk size), not
O(file size). Not independently profiled with a memory tool here (⚪).

## CPU

🟡 Dominated by AES-256-GCM + SHA-256; hardware-accelerated on modern x86/ARM.
The 726 MiB/s figure implies per-byte cost is small. Discovery/idle CPU is
event-driven (no busy polling — repositories are event-driven `ChangeNotifier`s
on the Flutter side, async tasks on the Rust side). Not profiled per-core (⚪).

## Disk

🟡 Sequential append on receive; atomic temp-then-rename on finalize
(`storage-fs`). Resume reads current on-disk length to negotiate offset —
O(1) metadata, not a re-read of the file.

## Network

🟡 QUIC (quinn) transport; `RouteManager` selects the fastest available route
(LAN → USB → Ethernet → Wi-Fi → Tailscale → DirectInternet → Relay) with
failover/migration. Real cross-route throughput is ⚪ (single-host environment).

## Transfer speed / large files

- Loopback 512 MiB ✓ (above). Chunked streaming means large-file behaviour is
  flat in memory. 🟡
- One prior live Android→Linux transfer completed byte-exact (recorded in
  [Network Testing](NETWORK_TESTING.md)); not re-run this session.

## Discovery latency

🟡 UDP broadcast + mDNS + Tailscale providers run concurrently and merge; first
results appear within the broadcast/mDNS interval. Not timed with a stopwatch
across real subnets here (⚪).

## Concurrent transfers

🟡 The transfer manager tracks multiple sessions; each transfer is an
independent async task over its own link. No shared mutable hot path that would
serialize them was found. Stress test with many simultaneous large transfers is
⚪ (not run at scale here).

## Startup / shutdown

- CLI cold `--help`/`benchmark` responsive; release binary 8.1 MB. 🟡 The 500 ms
  startup target from CLAUDE.md is plausible for the CLI but **not measured** as
  a hard number here (⚪), and app (Flutter) startup depends on platform.
- Shutdown: `pb_shutdown` tears down engine/manager cleanly under `catch_unwind`.
  🟡

## Findings

| Area | State | Note |
|---|---|---|
| Throughput | ✓ 726 MiB/s loopback | CPU/crypto not the bottleneck on real nets |
| Memory streaming | 🟡 | No full-file load; not tool-profiled |
| Concurrency at scale | ⚪ | Not stress-tested with many parallel large transfers |
| Startup time target | ⚪ | Not measured as a hard number |
| Cross-route throughput | ⚪ | Single-host environment |

## Verdict

Performance is **release-grade** on the verified path; the design (streaming,
zero-copy send, hardware crypto, route selection) is sound. Remaining items are
measurement gaps (profiling, scale, cross-route), not known regressions.
