# PeerSession Test Plan (Phase A2)

> **Status: DERIVED planning document.** Conforms to the constitutional set;
> **planning only — no production code or tests modified.** Companion to
> [MILESTONES](PEERSESSION_MILESTONES.md) and [RISK_REGISTER](PEERSESSION_RISK_REGISTER.md).

Testing reuses the existing harnesses (`peerbeam-transfer-quic/tests/{endpoints,network}.rs`,
`peerbeam-engine/tests/route_manager.rs`, `peerbeam-ffi/tests/transfer_ffi.rs`, the
per-crate unit tests, `secure.rs` replay tests, `recover.rs` resume tests, and the
[BENCHMARKS.md](BENCHMARKS.md) perf harness). Every category below names **what it
asserts**, **where it runs**, and **which milestone it gates** (DR3 — a milestone is
not done until its tests are green).

---

## 1. Coverage matrix

| Category | Asserts | Location | Gates |
|---|---|---|---|
| **Unit** | frame/registry codec: roundtrip, bounds, unknown-type decode | `peerbeam-domain` unit | M1 |
| **Protocol** | session frame layout; channel_id/message_type parse; malformed rejected | `peerbeam-transfer` unit | M1–M2 |
| **Authentication** | Hello/Confirm roundtrip; MAC-mismatch abort; transcript identity binding; TOFU pin + changed-key reject | reuse `auth.rs` tests, extend for per-session | M2 |
| **Version negotiation** | highest common major chosen; no-common-major → `VersionIncompatible`; capability intersection | `peerbeam-transfer` integration | M2 |
| **Unknown message handling** | unknown ChannelType → `Unsupported`; unknown `OPTIONAL` msg ignored; unknown required msg → channel error, session survives | integration | M2, M5 |
| **Unknown capabilities** | peer advertises unknown capability → not opened, no failure | integration | M2 |
| **Multiplexing / mixed channels** | 2+ channels concurrent; interactive channel not head-of-line-blocked by bulk | `peerbeam-transfer-quic` integration | M3 |
| **Concurrency** | N channels in parallel; no cross-channel data bleed; **no (key,nonce) reuse across channels** | `peerbeam-transfer-quic` + `secure.rs` | M3, M4 |
| **Replay attacks** | duplicate/reordered frame rejected (per-channel counter); single-use resume token replay rejected | reuse `secure.rs` replay tests per channel | M4, M6 |
| **Integration (2-node)** | full session: connect → auth → negotiate → open channel → exchange → close | `peerbeam-transfer-quic/tests` | M5 |
| **Large transfers** | multi-GB file over a Transfer channel; bounded memory; byte-exact | reuse existing large-file tests, run over channel | M5 |
| **Transfer parity** | pause/resume, cancel, checksum, folder tree — identical behavior vs legacy | reuse transfer suite over channel | M5 |
| **Resume** | kill mid-transfer → resume from on-disk offset (`ReliabilityStore`), byte-exact; survives process restart | reuse `recover.rs` tests, session-level | M6 |
| **Reconnect** | link drop → `RouteManager` re-selects route → session rebinds SessionId → continues | `peerbeam-engine` integration | M6 |
| **Network interruption** | Wi-Fi/route loss mid-session; failover to another route; no data loss | failure-injection harness | M6 |
| **Failure injection** | drop frames, close mid-handshake, kill mid-negotiate → typed error, fail-closed | fault harness | M2, M6 |
| **Malformed packets** | fuzz the frame/registry parser; every input → typed error, never panic/UB | `cargo`-fuzz target on the parser | M1 |
| **Protocol downgrade attempts** | forced lower version / stripped capability → rejected or safely degraded, never silent weakening | negotiation tests | M2 |
| **Plugin compatibility** | namespaced plugin channel; peer without plugin ignores it; no id collision | integration | (Phase D; harness reserved) |
| **Stress** | many sessions × many channels; sustained; resource bounds hold | stress harness | M3, M7 |
| **Performance regression** | transfer throughput over channel ≥ legacy baseline (within budget) | [BENCHMARKS.md](BENCHMARKS.md) harness, before/after | M5, M7 |
| **Memory regression** | no whole-payload buffering (I10); bounded per-channel buffers; peak vs baseline | memory harness | M5, M7 |

## 2. Security test focus (M4, M6)

The two security-critical areas get dedicated, adversarial tests:

- **Nonce isolation (M4).** Prove that `channel_key = KDF(session_key, channel_id ‖
  channel_type)` yields distinct keys per channel, and that no (key, counter) pair can
  repeat across concurrent channels or across a reconnect (fresh handshake → fresh
  master keys). This closes the one hazard multiplexing introduces.
- **Resume-token safety (M6).** Tokens are single-use, integrity-protected, bound to
  SessionId + peer identity, short-lived. Tests: replay a used token → rejected;
  forge/expire a token → safe fresh session (fail-closed), never an incorrect resume.

Both require a **manual security review** in addition to automated tests
([risk register](PEERSESSION_RISK_REGISTER.md)).

## 3. Compatibility tests

- **Old peer ↔ new peer:** an un-upgraded (legacy) peer and a session-capable peer
  negotiate to the legacy path; two session peers negotiate the session path. No
  transfer breaks (migration §4).
- **Version/capability matrix:** every supported (major, minor, capability-set) pair in
  the support window is exercised and recorded in the compatibility matrix — a **tested**
  claim, not an assumption (DR3).
- **FFI ABI:** `pb_abi_version` mismatch is detected at load; new session events are
  additive.

## 4. Live verification (not just automated)

Per project practice, each cutover-relevant milestone is **live-verified** on real
hardware (Linux ↔ Android over LAN and Tailscale), as the transfer path has been
through the v0.2.x line — not only unit/integration green (DR3). M7 requires a live
cross-device transfer over PeerSession before the default flips.

## 5. Test infrastructure additions (no product code)

- A **two-endpoint session harness** (extends the existing QUIC endpoint tests) that
  drives connect→auth→negotiate→channels in-process.
- A **fault-injection Link** wrapper (drop/delay/close) implementing the `Link` port,
  for failure/interruption/replay tests — a test-only adapter, matching the existing
  in-memory test Links.
- A **fuzz target** for the frame/registry parser.
- Reuse the existing **benchmark** and **large-file** harnesses for regression gates.

## 6. Exit criteria for Phase A testing

- All categories above green for M1–M8.
- Transfer parity (byte-exact) + no performance/memory regression vs the legacy
  baseline.
- Security review sign-off on M4 and M6.
- One documented live cross-device transfer over PeerSession (M7).
