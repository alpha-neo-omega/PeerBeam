# Feature Priorities

> **Status: DERIVED (not constitutional).** May evolve without amendment. Must
> conform to [VISION.md](VISION.md),
> [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md), and
> [FUTURE_ARCHITECTURE.md](FUTURE_ARCHITECTURE.md). Canonical phase list:
> [ROADMAP.md](../ROADMAP.md). Feature detail: [FEATURE_CATALOG.md](FEATURE_CATALOG.md).

This document explains **why each feature sits in its phase** and the order work
must follow. It is written for a **solo maintainer**: each phase is a coherent,
shippable increment, not a big-bang.

---

## The ordering principle

Phases follow **dependency and risk**, not excitement:

1. **Substrate before capabilities.** Nothing that talks to a peer is built until
   PeerSession, identity, negotiation, and the app-store exist — otherwise each
   feature would grow its own networking and the platform would fracture (I2,
   FUTURE_ARCHITECTURE §1).
2. **Security fixes ride with the substrate**, not after — trust hardening is
   Phase A because it changes what "trusted" means for everything above it.
3. **Highest value on the thinnest new surface first.** Once the substrate exists,
   the first capabilities are the ones that are small *because* the substrate is
   there (chat, presence, pipe).
4. **Depth and reach last.** Sync, automation, and internet reach are larger and
   depend on earlier layers.

Ties within a phase are broken by DR1 (constitutional preservation) then DR2
(simplicity): do the piece that most strengthens the spine and is simplest first.

---

## Phase A — Foundation · *must-have after v1.0*

**Why now:** every later feature depends on these; building any capability first
would violate I2 and create parallel systems.

| Feature | Why this phase |
|---|---|
| PeerSession typed channel | The spine. Everything is a message on it. |
| Persistent device identity | Trust and history are meaningless if identity churns each restart. |
| Protocol + capability negotiation | I9 must exist before a second message type, or the wire can never evolve safely. |
| Encrypted app-store | Chat/notes/clipboard-history need one local-first encrypted store, not one each. |
| Resumable transfers in UI | Engine capability already exists; low cost, high trust payoff; validates the session's resume story. |
| Trust hardening + optional PIN | Security fix that redefines "trusted" for everything above — must land with the substrate. |

**Dependency order:** identity → PeerSession → negotiation → app-store → (resume UI,
trust hardening in parallel).

## Phase B — First capabilities · *major features*

**Why now:** these are the smallest, highest-value capabilities *given* the
substrate — each is essentially one message type + one adapter, proving the pattern
publicly.

| Feature | Why this phase |
|---|---|
| Secure P2P chat + file-in-chat | Flagship proof that PeerBeam is a platform, not a transfer tool. Minimal on top of PeerSession + app-store. |
| Device dashboard + presence | Presence is a prerequisite for clipboard sync and remote features; high daily value; modest cost. |
| Auto clipboard sync | Classic cross-device win; depends on presence to target live devices. |
| Encrypted pipe (`peerbeam pipe`) | Pure CLI, pure stream — the moat feature, and cheap on the session (I7/I10). |
| Rules-based auto-save | Small, self-contained, improves the core transfer everyone already uses. |

**Dependency order:** presence → (chat, clipboard sync, pipe in parallel);
auto-save independent.

## Phase C — Power-user capabilities

**Why now:** depth that needs Phase B primitives (chat, presence, app-store) and is
larger or more specialized. Value is high for heavy users, so it follows the broad
wins rather than leading.

| Feature | Why this phase |
|---|---|
| Clipboard history, read receipts, chat search + reactions | Refinements of Phase B capabilities; only make sense once those ship. |
| Shared folders / folder sync | Large (change detection, conflict handling); a `SyncProvider` other features reuse. |
| Remote file browser (read-only) | Needs the session + a consent model; gates media streaming later. |
| Local notes, activity timeline | Ride the app-store and sync; personal-productivity depth. |
| Automation: watch / schedule / receive-hooks | CLI-first power; receive-hooks carry real risk, so they come after the model is mature. |
| Find/ring, log/snippet sharing | Small, pleasant, depend on presence/chat. |

## Phase D — Future ecosystem

**Why last:** largest surface, external dependencies (browsers, iOS, relay hosting),
and features that must wait until the core ports are **proven and versioned** before
being exposed (I2/I9, DR2 — no premature abstraction).

| Feature | Why this phase |
|---|---|
| Plugin + scripting API | Only safe to expose ports as public API after they are stable. |
| Web interface, iOS frontend | Ride a stable FFI and (for web) the relay. |
| WebRTC transport + relay/code-phrase | Internet reach; the last `RouteManager` tiers; needs a hostable relay. |
| Media streaming/preview | Large; depends on remote browser. |
| Permissioned remote commands | Highest security scrutiny; ships only when consent/audit is mature. |
| Optional local policy file | Small, but no earlier value; the only enterprise concession. |

---

## Solo-maintainer budget

- **One capability at a time.** Each ships with the full quality gate (fmt, clippy
  `-D warnings`, tests, `flutter analyze`/`test`), is live-verified (DR3), and is
  committed per milestone — the same discipline used through the v0.2.x line.
- **A phase is done when it is shippable**, not when it is exhaustive. Prefer
  fewer, complete, verified features over many partial ones (DR2; CLAUDE.md:
  coherence and user value over feature count).
- **Reject scope creep at the phase boundary.** A Phase C idea does not jump into
  Phase A because it is interesting; it waits for its dependencies.
