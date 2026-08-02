# PeerSession Risk Register (Phase A2)

> **Status: DERIVED planning document.** Conforms to the constitutional set;
> **planning only — no production code modified.** Companion to
> [MILESTONES](PEERSESSION_MILESTONES.md) and [TEST_PLAN](PEERSESSION_TEST_PLAN.md).

Every architectural risk in implementing [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md).
Likelihood/Impact are Low/Med/High. Each risk names the milestone that owns its
mitigation.

---

### R1 — Per-channel nonce reuse (cryptographic)
- **Cause:** multiplexing gives each channel its own AES-GCM counter; a derivation or
  reconnect bug could repeat a (key, nonce) pair, which breaks GCM confidentiality.
- **Impact:** High — silent loss of confidentiality/integrity (violates I5).
- **Likelihood:** Med (subtle, but concentrated in one function).
- **Mitigation:** derive `channel_key = KDF(session_key, channel_id ‖ channel_type)`
  via the existing `kdf`; fresh handshake on every reconnect → fresh master keys, so
  counter spaces never resume across connections. Isolate in `secure.rs` (M4).
- **Verification:** adversarial unit tests (distinct keys, no reuse across channels/
  reconnect) **plus a mandatory manual security review** (TEST_PLAN §2).
- **Residual:** Low after review.

### R2 — Multiplexing regressions (HOL blocking / flow control)
- **Cause:** moving from 1 bi-stream to N introduces cross-stream scheduling and
  QUIC flow-control interplay.
- **Impact:** Med — interactive channels stalled by bulk; throughput loss.
- **Likelihood:** Med.
- **Mitigation:** reuse QUIC-native streams (already proven with bi + uni today,
  [link.rs:152](../rust/crates/peerbeam-transfer-quic/src/link.rs#L152)); priority tiers
  (control > interactive > bulk); do not build a custom mux (DR2). (M3)
- **Verification:** concurrency + mixed-channel tests (bulk must not block interactive);
  stress harness.
- **Residual:** Low–Med.

### R3 — Cutover regression (transfers break when the default flips)
- **Cause:** at M7 the transfer path switches from legacy to PeerSession.
- **Impact:** High — the core feature regresses in the field.
- **Likelihood:** Low (gated by parity + live test).
- **Mitigation:** strangler — legacy stays default until byte-exact parity (M5) and
  reconnect/resume (M6) pass; flag-based instant rollback retained through M8
  (MIGRATION §6).
- **Verification:** full transfer suite over a channel + live Linux↔Android transfer
  before the flip (DR3).
- **Residual:** Low.

### R4 — Performance regression from session framing overhead
- **Cause:** per-frame channel/message-type header + per-channel sealing add cost.
- **Impact:** Med — slower/heavier transfers (pressures the CLAUDE.md perf targets).
- **Likelihood:** Med.
- **Mitigation:** bulk chunks keep riding raw with no JSON/base64 (preserve today's
  zero-per-chunk-bloat, [protocol.rs](../rust/crates/peerbeam-transfer/src/protocol.rs));
  header is a few bytes; measure before/after. (M5, M7)
- **Verification:** [BENCHMARKS.md](BENCHMARKS.md) before/after gate — throughput ≥
  baseline within budget.
- **Residual:** Low.

### R5 — "Session" naming collision → confusion and churn
- **Cause:** `TransferSession` (entity), auth `Session` (keys), and PeerSession coexist
  (IMPLEMENTATION_PLAN §6).
- **Impact:** Med — reader/maintainer confusion; risk of wiring the wrong "session".
- **Likelihood:** Med.
- **Mitigation:** introduce `PeerSession` as a distinct, clearly-doc'd type; do **not**
  rename the other two in the same change (avoid churn, DR2); track an optional later
  clarity rename. (M1–M2)
- **Verification:** code review focus; doc comments distinguishing the three.
- **Residual:** Low–Med (until an optional rename).

### R6 — Resume-token replay / incorrect resume
- **Cause:** a reconnect resumes the wrong session/channel or a token is replayed.
- **Impact:** High — corrupt transfer or session hijack (violates I6/I11).
- **Likelihood:** Low–Med.
- **Mitigation:** single-use, integrity-protected tokens bound to SessionId + peer
  identity, short-lived; stale/forged → safe fresh session (fail-closed). (M6)
- **Verification:** replay/forge/expire tests + security review (TEST_PLAN §2).
- **Residual:** Low.

### R7 — QUIC stream limits / resource exhaustion
- **Cause:** many channels/sessions open many streams; unbounded opens exhaust
  resources.
- **Impact:** Med — DoS/instability.
- **Likelihood:** Low–Med.
- **Mitigation:** per-session channel cap with `Denied{Limit}`; rely on QUIC stream
  flow control; tune `MAX_STREAMS`. (M3)
- **Verification:** stress test at/above the cap; limit-rejection test.
- **Residual:** Low.

### R8 — Scope creep in the "generalize transfer" refactor
- **Cause:** touching `stream.rs`/`folder.rs`/`secure.rs` invites broader rewrites.
- **Impact:** Med — large PRs, long branches, instability (against the brief + DR2).
- **Likelihood:** Med.
- **Mitigation:** milestone discipline — each PR is one seam; transfer logic is
  *wrapped*, not rewritten (it already takes a `Link`); legacy untouched until M9.
- **Verification:** PR size review; milestone DoD gates.
- **Residual:** Low.

### R9 — Wire incompatibility during migration
- **Cause:** session wire differs from legacy wire; mismatched peers.
- **Impact:** Med — a transfer fails to start.
- **Likelihood:** Low (pre-1.0 both rebuild; negotiation picks one path).
- **Mitigation:** version + capability negotiation selects legacy vs session; pre-1.0
  latitude; post-1.0 dual-major window ([VERSIONING.md](VERSIONING.md)).
- **Verification:** old↔new compatibility tests (TEST_PLAN §3).
- **Residual:** Low.

### R10 — Concurrency test flakiness hides real races
- **Cause:** multiplexing + reconnect are timing-sensitive; flaky tests get muted.
- **Impact:** Med — a real race ships.
- **Likelihood:** Med.
- **Mitigation:** deterministic in-memory Links for logic; `loom`/stress for the
  scheduler; condition-based waits, not sleeps; never mute — root-cause.
- **Verification:** repeated/stress runs in CI; flake budget = zero.
- **Residual:** Low–Med.

### R11 — Memory regression violating I10
- **Cause:** a channel handler buffers a whole message instead of streaming.
- **Impact:** High — breaks invariant I10 (bounded memory); OOM on large payloads
  (the exact class of the earlier Android large-file crash).
- **Likelihood:** Low–Med.
- **Mitigation:** channels stream with bounded buffers; app backpressure via the
  existing progress mechanism; code review for `read_to_end` on user data. (M5)
- **Verification:** memory-regression harness on large transfers; peak-memory gate.
- **Residual:** Low.

### R12 — Late security-review change forces rework
- **Cause:** the M4/M6 security review finds the key/token design needs change after
  dependent milestones exist.
- **Impact:** Med — rework of M5+.
- **Likelihood:** Low–Med.
- **Mitigation:** review the derivation/token **design** (this spec) before M4 coding,
  not after; keep M4 isolated in `secure.rs`.
- **Verification:** design-level security review sign-off pre-M4 (recommended in the A1
  risks and TEST_PLAN §2).
- **Residual:** Low.

---

## Top risks to watch
**R1** (nonce reuse) and **R6** (resume-token) are the highest-impact and are both
gated by an explicit security review in addition to tests. **R3** (cutover) is
mitigated structurally by the strangler + flag rollback. Everything else is Med-or-
lower with a concrete verification.
