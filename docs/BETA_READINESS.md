# Beta Readiness Report (M5)

## Recommendation: 🟢 Beta

PeerBeam moves real files between real devices, end to end, encrypted and
authenticated, with a clean full test suite. It is **not Stable** — Windows/macOS
packages are unbuilt here and several transports/scale scenarios are unverified —
but it is solidly **Beta**.

## Evidence
- **Live proof:** Android phone → Linux over Wi-Fi + QUIC — a 72 KB JPEG arrived
  byte-exact (valid `FFD8…FFD9`, whole-file SHA-256 verified, `0600` safe write),
  peer TOFU-pinned. Engine ran on-device (ports 49500 + 49600 bound).
- **Automated:** 64 Rust test suites + 32 Flutter tests pass. Real-QUIC E2E
  (send/receive/accept), resume-after-disconnect, 8 concurrent, 11 GiB stream,
  IPv4/IPv6, RouteManager failover/migration, SecureLink replay/tamper, folder
  edge cases (unicode/long/hidden/deep, symlink-skip), FFI (real dlopen).
- **Quality gate:** `cargo fmt`/`clippy -D warnings` clean, `flutter analyze`
  clean, `dart format` normalized.
- **Security:** code review found no critical issue (path traversal, symlink
  exfil, replay, tamper all handled). See [Security Report](SECURITY_REPORT.md).
- **Performance:** AES-GCM ~1.3 GiB/s, SHA-256 ~1.6 GiB/s, QUIC loopback
  ~447 MiB/s @ 0.7 ms connect; flat ~13 MB RAM (streams). See
  [Performance Report](PERFORMANCE_REPORT.md).

## Phase status
| Phase | Status |
|---|---|
| 1 Cross-platform verify | Linux ✅ + Android 📱; Windows/macOS 🔧 (unbuilt here) |
| 2 Transport verify | loopback/Wi-Fi/Tailscale ✅🧪; Ethernet/USB/subnets/netem 🔧 |
| 3 Stress | concurrent/large/resume/folder-edge 🧪; 50-100 GB / power-loss / sleep-wake 🔧 |
| 4 Performance | ✅ benchmarks + report |
| 5 Security | ✅ review + report, no critical issues |
| 6 Reliability | resume/disconnect/corruption/multi-peer 🧪; disk-full/low-mem 🔧 |
| 7 Platform integration | Android service/notifs 📱; desktop entry ✅; Win/mac 🔧 |
| 8 Code quality | ✅ clean gate |
| 9 Documentation | ✅ updated + this report set |
| 10 Release readiness | ✅ matrices + reports + recommendation |

## Release checklist (to reach Stable)
- [ ] Build + smoke-test Windows MSIX + macOS DMG on their hosts.
- [ ] Real two-machine transfers: LAN, Ethernet, USB tethering, cross-subnet.
- [ ] netem latency/loss + 50/100 GB sustained runs (privileged/CI).
- [ ] Persistent device identity (stop TOFU re-pinning per run).
- [ ] Wire folder send in the CLI; network clipboard receive.
- [ ] External security review.
- [ ] Signed/notarized release artifacts (certs in CI secrets).

## Blockers to full M5 completion (environmental, documented)
Single unprivileged Linux host + one Android phone: no Windows/macOS toolchain,
no second machine, no root (netem/netns), no disk for 50/100 GB. All such rows
are code-audited and marked unverified rather than faked.
