# PeerSession Milestones (Phase A2)

> **Status: DERIVED planning document.** Conforms to the constitutional set;
> **planning only — no production code modified.** See
> [PEERSESSION_IMPLEMENTATION_PLAN.md](PEERSESSION_IMPLEMENTATION_PLAN.md) and
> [PEERSESSION_MIGRATION.md](PEERSESSION_MIGRATION.md).

**Ground rules (every milestone):** the repo compiles, all tests pass, the full gate
(`cargo fmt`, `clippy -D warnings`, `cargo test --workspace`, `flutter analyze`/`test`)
is green, existing transfers keep working, and the change is a small PR on a
short-lived branch. **No broken intermediate commits.** Order is dependency-safe (see
[DEPENDENCY_GRAPH](PEERSESSION_DEPENDENCY_GRAPH.md)).

The strategy is a **strangler**: PeerSession is built alongside the working transfer
path and only becomes the default at M7, after parity is proven.

---

## M1 — Message framing + registry types (domain)
- **Objective:** add `ChannelType`, `MessageType`, and a `SessionFrame` header
  (channel_id + message_type + flags + length) with a codec. No wiring.
- **Crates:** `peerbeam-domain`. **Files:** new `port/session.rs` (or
  `entity/session_frame.rs`), registry module.
- **Complexity:** S. **Dependencies:** none.
- **Risks:** codec/endianness ambiguity → pin exact encoding now.
- **Verification:** unit tests — frame roundtrip, unknown-type decode,
  length-bound rejection, malformed input.
- **Rollback:** delete the new module; nothing else references it.
- **DoD:** types + codec merged, unit-tested, gate green; existing `Frame`
  ([port/transfer.rs:40](../rust/crates/peerbeam-domain/src/port/transfer.rs#L40)) untouched.

## M2 — PeerSession skeleton + control channel (auth + negotiate)
- **Objective:** a `PeerSession` that, given a `Link`, runs the existing
  `authenticate()` then exchanges `SessionHello` (version + capabilities + SessionId)
  over control channel 0. No data channels yet.
- **Crates:** `peerbeam-transfer` (new `session.rs`), reuse
  [auth.rs:111](../rust/crates/peerbeam-transfer/src/auth.rs#L111),
  [secure.rs:26](../rust/crates/peerbeam-transfer/src/secure.rs#L26).
- **Complexity:** M. **Dependencies:** M1.
- **Risks:** negotiation edge cases (no common major).
- **Verification:** integration test over an in-memory/loopback `Link` (existing test
  Link infra in `peerbeam-transfer-quic/tests`); version-mismatch → `VersionIncompatible`.
- **Rollback:** feature-gated module; not yet called by transfer.
- **DoD:** two in-process endpoints establish a session + agree capabilities; gate green.

## M3 — Multiplexing: N channels over one QUIC connection
- **Objective:** extend the QUIC provider to open/accept **multiple bi-streams** per
  connection (one per channel), generalizing the existing bi + uni pattern.
- **Crates:** `peerbeam-transfer-quic`. **Files:**
  [link.rs](../rust/crates/peerbeam-transfer-quic/src/link.rs) (already does
  `open_uni`/`accept_uni` at 152/170), [lib.rs](../rust/crates/peerbeam-transfer-quic/src/lib.rs) (`open_bi`/`accept_bi` at 168/205).
- **Complexity:** M. **Dependencies:** M1, M2.
- **Risks:** stream limits, flow-control interplay, HOL isolation regressions.
- **Verification:** concurrency test — 2+ channels exchange concurrently; a slow bulk
  channel does not block an interactive one; stream-limit behavior.
- **Rollback:** keep single-stream path as default; multi-stream behind a flag.
- **DoD:** two channels run concurrently over one connection; gate green.

## M4 — Per-channel SecureLink keys (security-critical)
- **Objective:** derive per-channel send/recv keys + nonce prefixes from the session
  master keys via the existing `kdf` (`channel_key = KDF(session_key, channel_id ‖
  channel_type)`) so concurrent streams never share a counter space.
- **Crates:** `peerbeam-transfer`
  ([secure.rs:26](../rust/crates/peerbeam-transfer/src/secure.rs#L26), `kdf` in auth.rs).
- **Complexity:** M (H by risk). **Dependencies:** M3.
- **Risks:** nonce reuse if derivation or reconnect is wrong → **mandatory security
  review**.
- **Verification:** unit test — distinct channels get distinct keys; reuse of a
  (key, nonce) pair is impossible across channels and across reconnect (fresh
  handshake → fresh keys); reuse existing `secure.rs` replay tests per channel.
- **Rollback:** revert to a single session key (single-channel) — but M5+ depend on M4,
  so this milestone gates the rest.
- **DoD:** per-channel sealing proven; security review signed off; gate green.

## M5 — Transfer as a channel (parallel to legacy)
- **Objective:** run the existing transfer (`stream.rs`/`folder.rs`/`protocol.rs`) as
  the **Transfer channel handler** on a PeerSession, behind a config/feature flag, in
  parallel with the untouched legacy path.
- **Crates:** `peerbeam-transfer` (new `TransferMessageHandler` wrapping existing
  send/recv, which already take a `Link`), `peerbeam-engine` (session dispatch +
  handler registry via [builder.rs](../rust/crates/peerbeam-engine/src/builder.rs)).
- **Complexity:** M. **Dependencies:** M2, M3, M4.
- **Risks:** behavioral drift vs legacy (progress, pause, cancel, resume).
- **Verification:** the full existing transfer test suite runs green **over a channel**
  (large file, folder, pause/resume, cancel, checksum); byte-exact parity vs legacy.
- **Rollback:** flag defaults to legacy; delete the handler.
- **DoD:** transfers over PeerSession pass parity; legacy still default; gate green.

## M6 — Reconnect + resume on PeerSession
- **Objective:** session reconnect (via `RouteManager::link_factory`
  [route_manager.rs:131](../rust/crates/peerbeam-engine/src/route_manager.rs#L131)) with
  a single-use resume token rebinding SessionId, and per-channel resume (Transfer reuses
  `ReliabilityStore` offset).
- **Crates:** `peerbeam-transfer` (generalize
  [recover.rs:31](../rust/crates/peerbeam-transfer/src/recover.rs#L31)), `peerbeam-engine`.
- **Complexity:** M–H. **Dependencies:** M5.
- **Risks:** resume-token replay; incorrect resume corrupting a transfer.
- **Verification:** failure-injection — kill the link mid-transfer → RouteManager
  re-selects → transfer resumes from on-disk offset, byte-exact; stale/forged token →
  safe fresh session.
- **Rollback:** disable session resume; fall back to legacy recover-across-links.
- **DoD:** interrupt/resume + reconnect proven; gate green.

## M7 — Cutover: transfer rides PeerSession by default
- **Objective:** flip the default so transfers use the Transfer channel; keep legacy
  behind a flag for one release (dual-major window per
  [VERSIONING.md](VERSIONING.md)).
- **Crates:** `peerbeam-ffi`, `peerbeam-engine` (default wiring).
- **Complexity:** M. **Dependencies:** M5, M6.
- **Risks:** field regression; interop with un-upgraded peers (pre-1.0 both rebuild).
- **Verification:** live cross-device transfer (Linux↔Android) over PeerSession; full
  regression suite; performance + memory within budget vs legacy baseline.
- **Rollback:** flip the flag back to legacy (kept in place this whole milestone).
- **DoD:** PeerSession is the default transfer path, verified live; legacy still
  available; gate green.

## M8 — Session/channel status in FFI + CLI (additive)
- **Objective:** expose session/channel state additively (`pb_*` + event family; CLI
  `status` detail). No new capabilities.
- **Crates:** `peerbeam-ffi`, `bins/peerbeam-cli`.
- **Complexity:** S. **Dependencies:** M7.
- **Risks:** minor ABI growth → bump `pb_abi_version`.
- **Verification:** FFI round-trip test; CLI JSON snapshot.
- **DoD:** session state observable; gate green.

## M9 — Retire legacy transfer path (after the window)
- **Objective:** remove the legacy direct-on-Link transfer once the dual-major window
  closes and adoption is confirmed.
- **Crates:** `peerbeam-transfer`, `peerbeam-engine`.
- **Complexity:** S–M. **Dependencies:** M7 (+ elapsed support window).
- **Risks:** removing a still-needed fallback → gate on telemetry-free adoption
  evidence (a release cycle), not a guess.
- **Verification:** full suite green with legacy removed.
- **Rollback:** revert the removal commit (legacy is a clean deletion).
- **DoD:** one transfer path (PeerSession); dead code gone; gate green.

---

**Phase A completion** = M1–M8 (M9 is a later cleanup). Phase B capabilities (chat,
presence, clipboard sync) are **new channel types on the finished session** and are
out of scope here.
