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

The next work is **not** more transport. It is growing the productivity platform on
top of PeerSession, in constitutional order.

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

*None.*

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
