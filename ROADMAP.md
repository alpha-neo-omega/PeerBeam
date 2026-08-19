# PeerBeam Roadmap

> **Status: CONSTITUTIONAL**
>
> This document is authoritative.
>
> Any feature, milestone, refactor, protocol change, or architectural decision must conform to this document.
>
> Changes require an explicit constitutional amendment.

---

This is the **canonical roadmap**. It is intentionally short: it fixes the
*phases*, their *order*, and their *rationale*. The per-feature detail lives in the
derived, evolvable documents.

**Constitutional documents** (authoritative, amend only):
[VISION.md](docs/VISION.md) ·
[ARCHITECTURAL_INVARIANTS.md](docs/ARCHITECTURAL_INVARIANTS.md) ·
[FUTURE_ARCHITECTURE.md](docs/FUTURE_ARCHITECTURE.md) · this file.

**Derived documents** (may evolve without amendment):
[FEATURE_CATALOG.md](docs/FEATURE_CATALOG.md) — every feature, fully evaluated ·
[FEATURE_PRIORITIES.md](docs/FEATURE_PRIORITIES.md) — phase placement and rationale.

Everything below extends the one abstraction defined in the Vision:
`Peer → PeerSession → Typed Message → Handler → Engine`. File transfer is one
message type; every future capability is another. No feature bypasses PeerSession.

---

## Status

**Release Candidate.** The transport foundation is built and shipping: QUIC
transport with receiver-confirmed progress, zero-config multi-transport discovery
(LAN + mDNS + Tailscale), TOFU trust with a management screen, streaming
file/folder/clipboard/text transfer, resume infrastructure, a first-class CLI, and
CI-built artifacts for Linux, Android, Windows (portable), and macOS.

**Phases A, B and C are complete** (Amendment 1). The platform substrate is
frozen, and the productivity capabilities that sit on it are built: nine
first-party channels are allocated and eight implemented, with seven revocable
per-device permissions. The one allocated channel not yet built is Command
(`0x0106`), which belongs to Phase D.

The next work is **Phase D** — reach and extensibility, once the core is proven
and versioned.

## ✅ Done (historical milestones — never removed)

Preserved from prior roadmaps and releases; see release notes in `docs/`.

- **v0.2.x line** — receiver-confirmed progress + speed/ETA; persistent settings,
  history, and trust; Android public-storage receive (Downloads/SAF); cooperative
  pause; explicit trust model; Android notifications; large-file streaming;
  cross-platform CI (Linux/Android/Windows/macOS); macOS GUI engine bundling.
- **Send text / quick message** *(LocalSend-style)* — compose and send a message;
  receiver sees a message dialog with Copy.
- **Stacking selection** *(LocalSend-style)* — build one selection from files,
  folders, text, and clipboard; send the batch to one device.
- **Phase A — Foundation** *(complete)* — PeerSession typed message channels;
  persistent device identity; protocol version + capability negotiation (I9);
  the encrypted `AppStore` every capability shares; resumable transfers surfaced
  in the UI; trust hardening and optional PIN pairing.
- **Phase B — First platform capabilities** *(complete)* — peer-to-peer chat with
  file references and declines; device dashboard and presence; opt-in clipboard
  sync; `peerbeam pipe`; rules-based auto-save.
- **Phase C — Power-user capabilities** *(complete)* — clipboard history;
  delivery/read receipts; chat search and reactions; shared folders with
  bidirectional sync (per-file version vectors, conflicts kept rather than
  resolved); read-only remote file browser; local notes; activity timeline;
  watch-folder, scheduled sends and receive-hooks; find/ring my device; snippet
  and terminal-output sharing.

## Phase A — Foundation (must-have after v1.0)

Freeze the platform substrate. Everything later depends on it; doing it first is
what prevents parallel systems (I2, FUTURE_ARCHITECTURE §1).

- PeerSession typed message channel (generalize transfer framing)
- Persistent device identity (stable keypair across restarts)
- Protocol version + capability negotiation (I9)
- Encrypted local app-store (`AppStore` port)
- Resumable transfers surfaced in the UI (engine capability already exists)
- Trust hardening + optional PIN pairing (security fix)

## Phase B — First platform capabilities (major features)

The first productivity wins, each a message type on the session.

- Secure peer-to-peer chat (text/markdown) + file sharing inside chat
- Device dashboard + presence (online, battery, storage, network)
- Auto clipboard sync (opt-in, trusted only)
- Encrypted pipe — `peerbeam pipe` (stdin↔stdout between devices)
- Rules-based auto-save for received items

## Phase C — Power-user capabilities

Depth for people who live in the tool; several are CLI-first.

- Clipboard history · delivery/read receipts · chat search + reactions
- Shared folders / continuous folder sync (trusted, P2P)
- Remote file browser (read-only, permissioned)
- Local notes (synced to your own devices) · activity timeline
- Automation: watch-folder · scheduled sends · receive-hooks
- Find / ring my device · log / terminal-output / snippet sharing

## Phase D — Future ecosystem

Reach and extensibility, once the core is proven and versioned.

- Plugin + scripting API (stabilize the domain ports as a public extension surface)
- Web interface · iOS frontend
- WebRTC transport depth · relay + code-phrase (internet reach without a tailnet)
- Media streaming / preview
- Permissioned remote commands (narrow, consented allowlist — never remote control)
- Optional local policy file (the only "enterprise" concession)

## Reach enablers (cross-cutting, pre-existing)

Relay + code-phrase and the web receiver from earlier roadmaps are retained as the
**reach** substrate that cross-network chat/sync depend on. They remain optional and
untrusted (I3/I5) and are scheduled within Phase D.

---

## Historical roadmaps

[docs/FEATURE_ROADMAP.md](docs/FEATURE_ROADMAP.md) and
[docs/LONG_TERM_ROADMAP.md](docs/LONG_TERM_ROADMAP.md) are **historical**. They are
preserved for context and are superseded by this file. Where they conflict with the
constitutional set, the constitutional set governs.

---

## Amendments

### Amendment 1 — Phases A–C recorded complete (2026-08-19)

**What changed.** The Status section described growing the productivity platform
as the *next* work, and the Done list stopped at the v0.2.x line. Phases A, B and
C are built, so both statements had become false.

**Why an amendment.** This file is constitutional. The phases, their order and
their rationale are unchanged — only the record of which are finished. Nothing
here reorders a phase, moves a feature between phases, or alters a rationale.

**What did not change.** Phase D is untouched and unstarted. The Command channel
(`0x0106`) remains allocated but unbuilt, in Phase D where it was placed.

**Date.** 2026-08-19.

**Approval.** Approved by the repository owner (althaf@curanova.ai).

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
