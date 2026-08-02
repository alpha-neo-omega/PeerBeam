# PeerSession Migration Plan (Phase A2)

> **Status: DERIVED planning document.** Conforms to the constitutional set;
> **planning only — no production code modified.** See
> [MILESTONES](PEERSESSION_MILESTONES.md) and
> [IMPLEMENTATION_PLAN](PEERSESSION_IMPLEMENTATION_PLAN.md).

How the repository moves from "transfer runs directly on a `Link`" to "transfer runs
as one channel on a PeerSession" **without a big-bang rewrite, a long-lived branch, or
a broken transfer at any point.**

---

## 1. Strategy: strangler, not rewrite

PeerSession is built **beside** the working transfer path and consumes the same
`Link` the transfer already uses ([RouteManager::connect](../rust/crates/peerbeam-engine/src/route_manager.rs#L90) → `Box<dyn Link>`). Because a channel exposes the same
`send_frame`/`recv_frame` surface as `Link`
([port/transfer.rs:78](../rust/crates/peerbeam-domain/src/port/transfer.rs#L78)), the
existing `stream.rs`/`folder.rs` logic runs on a channel with minimal edits. The old
path stays the default until the new one reaches **byte-exact parity** (M5) and proves
reconnect/resume (M6); only then does the default flip (M7), and the old path remains
as a flagged fallback for one release before removal (M9).

At no milestone is there a moment where transfers are broken or "half-migrated on the
wire."

## 2. Invariants of the migration

1. **Never break existing transfers.** Legacy transfer code is untouched through
   M1–M4 and remains the default through M6. It is only removed in M9, after a support
   window.
2. **Every commit builds and every milestone is green.** Enforced by the CI gate
   (fmt/clippy/test + flutter); no milestone merges red.
3. **Small PRs, short-lived branches.** Each milestone is one focused PR (new module or
   one seam), merged within days. New code lands behind a flag so `main` stays
   shippable — enabling continuous development on unrelated features in parallel.
4. **Additive-first at the domain layer.** New ports/types are added; existing `Frame`,
   `Link`, `TransferProvider` signatures are not broken until the cutover, and even then
   via new variants, not edits.

## 3. Feature-flag / dual-path scheme

- A build/config flag selects **legacy** (direct-on-Link) vs **session** (Transfer
  channel) for the transfer path. Default = legacy until M7, then session.
- The flag lives at the engine wiring layer
  ([builder.rs](../rust/crates/peerbeam-engine/src/builder.rs)); both paths compile at
  all times, so the fallback is a config flip, not a code change.
- The QUIC multi-stream capability (M3) is also flagged; single-stream remains until
  the session path is the caller.

## 4. Wire-compatibility during the window

- The wire changes (session framing, per-channel keys) are **guarded by version +
  capability negotiation** ([VERSIONING.md](VERSIONING.md)); a session only forms if
  both peers negotiate it.
- **Pre-1.0 latitude:** PeerBeam is pre-1.0, so a peer running an old build simply uses
  the legacy path; two upgraded peers use the session path. There is no requirement to
  interoperate legacy-wire with session-wire in the same transfer — the negotiation
  picks one. (This matches today's reality that both peers must run compatible builds,
  documented in [TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md#compatibility).)
- After 1.0, the dual-major support window (VERSIONING §3) governs removal timing.

## 5. Sequence (maps to milestones)

```
M1 domain types ──▶ M2 session skeleton ──▶ M3 multiplex ─┐
                                             M4 per-chan keys ┤─▶ M5 transfer-as-channel
                                                              ┘        (parallel to legacy)
   ──▶ M6 reconnect/resume ──▶ M7 cutover (default flips) ──▶ M8 FFI/CLI status
   ──▶ (support window) ──▶ M9 remove legacy
```

Each arrow is a mergeable, green step. Legacy is live from M1 through M8.

## 6. Rollback posture

- **Before M7:** nothing to roll back — the session path is off by default; legacy is
  the shipping path.
- **At/after M7:** roll back by flipping the flag to legacy (retained until M9). No
  revert of merged work needed.
- **M9:** the legacy removal is a single, clean, revertible commit.

## 7. Continuous development

Because every milestone is flagged and green, unrelated work (bug fixes, docs, even
early Phase-B groundwork behind its own flag) proceeds on `main` throughout. There is
no freeze and no integration branch to rot.

## 8. What the migration must never do (constitutional guardrails)

- Never ship an intermediate state where a transfer can silently use an unversioned or
  unauthenticated path (I5/I9).
- Never make the session path mandatory before parity + reconnect/resume are verified
  (DR3 — evidence before cutover).
- Never introduce a long-lived branch or a mega-PR (explicit brief requirement; also
  DR2 — small, comprehensible changes).
- Never edit the domain to point outward or add a cloud/telemetry dependency to ease
  migration (I1/I4).
