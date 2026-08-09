# Feature Catalog

> **Status: DERIVED (not constitutional).** This document may evolve without a
> constitutional amendment. It must conform to
> [VISION.md](VISION.md), [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md),
> and [FUTURE_ARCHITECTURE.md](FUTURE_ARCHITECTURE.md). Where it conflicts with them,
> they govern. Phasing rationale lives in
> [FEATURE_PRIORITIES.md](FEATURE_PRIORITIES.md); the canonical timeline is
> [ROADMAP.md](../ROADMAP.md).

Every accepted feature is evaluated on nine dimensions: **Why · Reuses ·
Dependencies · Complexity · Security · Storage · Offline · CLI** (priority is the
phase). Complexity: **S** small, **M** medium, **L** large. Each feature is a
message type + handler + adapter on **PeerSession** — none invents its own
networking, trust, or crypto (I5/I6/I9).

---

## Core Platform

### PeerSession typed message channel — Phase A · L
- **Why:** the spine of the platform; makes every future capability a message type
  rather than a parallel system (FUTURE_ARCHITECTURE §1).
- **Reuses:** `SecureLink` (E2E), `RouteManager` (path/failover), `peerbeam-transfer`
  framing, `peerbeam-transfer-quic`.
- **Dependencies:** none (foundation).
- **Security:** E2E + mutual auth inherited from `SecureLink` (I5); one place for
  crypto/trust.
- **Storage:** none itself.
- **Offline:** channel setup requires reachability; message intent can be queued
  locally.
- **CLI:** indirect — every CLI capability rides it.

### Persistent device identity — Phase A · S
- **Why:** a stable keypair across restarts so peers stop re-pinning; closes the
  main known-issue trust gap.
- **Reuses:** `peerbeam-crypto`, `peerbeam-trust-fs`.
- **Dependencies:** none.
- **Security:** private key stored with OS-appropriate protection; rotation is an
  explicit, logged action (I6).
- **Storage:** one small keypair per device.
- **Offline:** yes (local only).
- **CLI:** yes — identity shown by `peerbeam status`.

### Protocol version + capability negotiation — Phase A · S
- **Why:** enforce I9 from day one so the wire can evolve without silent drift.
- **Reuses:** `peerbeam-transfer` protocol layer.
- **Dependencies:** PeerSession.
- **Security:** prevents downgrade surprises; unknown message types tolerated
  (FUTURE_ARCHITECTURE §4).
- **Storage:** none.
- **Offline:** n/a.
- **CLI:** negotiated version surfaced in `peerbeam status`/JSON.

### Encrypted local app-store (`AppStore` port) — Phase A · M
- **Why:** local-first, encrypted persistence for chat/notes/clipboard history
  (I11) without inventing a store per feature.
- **Reuses:** `peerbeam-storage-fs`, `peerbeam-crypto`.
- **Dependencies:** persistent identity (key material).
- **Security:** encrypted at rest; per-capability namespaces; user-set retention.
- **Storage:** small append/KV stores; user-controlled, clearable.
- **Offline:** yes (local-first).
- **CLI:** yes — inspect/clear via subcommands.

### Trust hardening + optional pairing-code verification — Phase A · S–M
- **Why:** fix the sharp edge where a peer is pinned during handshake even if the
  transfer is declined; add optional first-contact **pairing-code (safety-number) comparison**: a
  128-bit code derived from both device keys, compared out of band to detect a
  man-in-the-middle; off by default. (A short 6-digit code was considered but
  is grindable; the compared code is full-width.)
- **Reuses:** `peerbeam-transfer` auth, `peerbeam-trust-fs`.
- **Dependencies:** none.
- **Security:** separates "key-pinned (anti-MITM)" from "approved (authorized)";
  auto-accept requires real approval (I6).
- **Storage:** trust records (exists).
- **Offline:** yes.
- **CLI:** yes — trust list/approve/revoke.

---

## Files & Synchronization

### Resumable transfers surfaced in the UI — Phase A · M
- **Why:** the engine already resumes (`reliability-fs`, `recover.rs`); expose it so
  interrupted transfers resume instead of restart.
- **Reuses:** `peerbeam-reliability-fs`, transfer recover paths.
- **Dependencies:** none.
- **Security:** unchanged; checkpoints hold no plaintext beyond the partial file.
- **Storage:** checkpoint store (exists).
- **Offline:** resumes on reconnect.
- **CLI:** yes — resume state in transfer status.

### Shared folders / continuous folder sync — Phase C · L
- **Why:** keep a folder mirrored between *your* trusted devices, P2P (not cloud).
- **Reuses:** transfer, `reliability-fs`, new `SyncProvider`.
- **Dependencies:** PeerSession, app-store (index).
- **Security:** trusted peers only; E2E; conflict handling explicit (I6).
- **Storage:** per-folder sync index/metadata.
- **Offline:** yes — reconcile on reconnect.
- **CLI:** yes — `peerbeam sync <dir> --to <device>` (daemon-friendly).

### Remote file browser (read-only, permissioned) — Phase C · M
- **Why:** browse a shared location on a trusted device before pulling.
- **Reuses:** `SecureLink`, `peerbeam-storage-fs`, PeerSession.
- **Dependencies:** PeerSession.
- **Security:** explicit per-share consent, read-only first, scoped to a chosen root
  (I6); never full-filesystem.
- **Storage:** none (live listing).
- **Offline:** requires the peer online.
- **CLI:** yes — `peerbeam ls <device>:<share>`.

### Media streaming / preview — Phase D · L
- **Why:** preview/stream large media without a full download.
- **Reuses:** streaming transfer (range reads), storage-fs.
- **Dependencies:** remote browser, PeerSession.
- **Security:** trusted + consented; streamed, never fully buffered (I10).
- **Storage:** transient cache only.
- **Offline:** requires the peer online.
- **CLI:** partial — range fetch to stdout.

---

## Communication

### Secure P2P chat (text/markdown) — Phase B · M
- **Why:** text is the smallest message on an already-authenticated link; flagship
  of the "platform" claim.
- **Reuses:** PeerSession, `AppStore`, `RouteManager`.
- **Dependencies:** PeerSession, app-store.
- **Security:** E2E; 1:1 only (no hub, I3); history encrypted at rest.
- **Storage:** append-only encrypted log per peer; user retention.
- **Offline:** compose offline, queued, delivered on reachability.
- **CLI:** yes — `peerbeam chat <device>` (interactive + piped).

### File sharing inside chat — Phase B · S
- **Why:** unify "send a file" and "talk about it" on one channel.
- **Reuses:** transfer (as a chat message type).
- **Dependencies:** chat, PeerSession.
- **Security:** same transfer guarantees; recorded in the chat log.
- **Storage:** file goes to the normal receive dir; a reference in the log.
- **Offline:** queued like chat.
- **CLI:** yes — attach in `peerbeam chat`.

### Delivery / read receipts — Phase C · S
- **Why:** know a message arrived/was seen, privacy-respecting and opt-in.
- **Reuses:** the receiver-confirmed progress back-channel pattern.
- **Dependencies:** chat.
- **Security:** opt-in per peer; no third party involved.
- **Storage:** status flags in the chat log.
- **Offline:** delivered on reconnection.
- **CLI:** yes — status in transcript.

### Chat search + reactions — Phase C · S
- **Why:** find past messages; lightweight reactions.
- **Reuses:** `AppStore` (local index).
- **Dependencies:** chat.
- **Security:** local-only index; no external service.
- **Storage:** small local index + reaction records.
- **Offline:** yes (local).
- **CLI:** yes — `peerbeam chat --search`.

---

## Clipboard

### Auto clipboard sync (opt-in, trusted) — Phase B · M
- **Why:** copy on A, paste on B, automatically — between *your* trusted devices.
- **Reuses:** `ClipboardProvider`, PeerSession.
- **Dependencies:** PeerSession, presence (to target live devices).
- **Security:** opt-in per device pair, revocable; E2E; off by default (I6/I11).
- **Storage:** none required (history is a separate feature).
- **Offline:** syncs when the peer is reachable.
- **CLI:** yes — `peerbeam clipboard --sync`.

### Clipboard history + rich types (image/code) — Phase C · S–M
- **Why:** recall recent clips; support images and code snippets, not just text.
- **Reuses:** `AppStore`, transfer (for large/binary clips).
- **Dependencies:** auto clipboard sync or manual clipboard send.
- **Security:** local, encrypted at rest; clearable; opt-in.
- **Storage:** bounded encrypted ring buffer.
- **Offline:** yes (local).
- **CLI:** yes — `peerbeam clipboard history`.

---

## Productivity

### Device dashboard + presence — Phase B · M
- **Why:** see your devices at a glance — online, battery, storage, network.
- **Reuses:** `PresenceProvider`, `DeviceStore`, PeerSession.
- **Dependencies:** PeerSession.
- **Security:** presence shared only with trusted peers; each field opt-in (I6).
- **Storage:** last-known snapshot; no history required.
- **Offline:** shows last-known + offline state.
- **CLI:** yes — `peerbeam dashboard` / `peerbeam status <device>`.

### Local notes (synced to your own devices) — Phase C · M
- **Why:** quick notes that follow your device mesh, no cloud note service.
- **Reuses:** `AppStore`, `SyncProvider`.
- **Dependencies:** app-store, folder-sync machinery.
- **Security:** encrypted at rest; synced only to your trusted devices (I11).
- **Storage:** small encrypted note store.
- **Offline:** yes — edit offline, sync later.
- **CLI:** yes — `peerbeam notes`.

### Activity timeline — Phase C · S
- **Why:** a local log of what happened (transfers, chats, syncs).
- **Reuses:** `AppStore`, existing history.
- **Dependencies:** app-store.
- **Security:** local-only; clearable; no telemetry (I4).
- **Storage:** bounded local log.
- **Offline:** yes.
- **CLI:** yes — `peerbeam history` (extends existing).

---

## Developer Tools

### Encrypted pipe — `peerbeam pipe` — Phase B · M
- **Why:** stream stdin↔stdout between devices over an E2E link — the CLI moat; a
  scriptable primitive competitors lack (I7).
- **Reuses:** `SecureLink`, PeerSession, CLI.
- **Dependencies:** PeerSession.
- **Security:** E2E; trusted peers; no intermediary sees data (I5).
- **Storage:** none (pure stream, I10).
- **Offline:** requires the peer online.
- **CLI:** yes — the feature *is* CLI (`cmd | peerbeam pipe <device>`).

### Log / terminal-output / snippet sharing — Phase C · S
- **Why:** push logs, command output, or code to a trusted device as a message.
- **Reuses:** chat message types, pipe.
- **Dependencies:** chat or pipe.
- **Security:** trusted peers; E2E.
- **Storage:** optional in chat log.
- **Offline:** queued.
- **CLI:** yes — first-class.

---

## Automation

### Rules-based auto-save — Phase B · S
- **Why:** route received items to folders by rule instead of a single inbox.
- **Reuses:** `peerbeam-storage-fs`, `peerbeam-config`.
- **Dependencies:** none.
- **Security:** rules are local; no code execution by default (I11).
- **Storage:** rule config.
- **Offline:** yes.
- **CLI:** yes — rules in config.

### Watch-folder · scheduled sends · receive-hooks — Phase C · M
- **Why:** automate sending (on change / on schedule) and post-receive actions.
- **Reuses:** transfer, `peerbeam-config`, CLI daemon.
- **Dependencies:** PeerSession.
- **Security:** receive-hooks run commands → **explicit opt-in, per-rule consent,
  clearly dangerous** (I6); disabled by default.
- **Storage:** rule/schedule config.
- **Offline:** queues until reachable.
- **CLI:** yes — `peerbeam watch`, `peerbeam send --at`.

---

## Remote Device

### Find / ring my device — Phase C · S
- **Why:** locate a device on the mesh (buzz/notify).
- **Reuses:** PeerSession, notifications.
- **Dependencies:** PeerSession, presence.
- **Security:** trusted peers only; harmless action (I6).
- **Storage:** none.
- **Offline:** requires the peer online.
- **CLI:** yes — `peerbeam ring <device>`.

### Permissioned remote commands (narrow allowlist) — Phase D · M
- **Why:** a *small* set of consented actions (ring, notify, "bring file X") — not
  remote control.
- **Reuses:** PeerSession, `TrustStore`.
- **Dependencies:** PeerSession, presence.
- **Security:** **highest scrutiny.** Fixed allowlist, per-command explicit consent,
  revocable, logged. Never arbitrary execution (I6; Vision non-goal: not
  TeamViewer).
- **Storage:** permission grants + audit log.
- **Offline:** requires the peer online.
- **CLI:** yes — explicit subcommands only.

---

## Future Ecosystem

### Plugin + scripting API — Phase D · L
- **Why:** let third parties add discovery/transfer/clipboard/storage providers via
  the proven ports.
- **Reuses:** `peerbeam-domain` ports.
- **Dependencies:** ports stable and versioned (I2/I9).
- **Security:** capability-scoped; signed/trusted plugins; sandboxed where possible.
- **Storage:** plugin-defined, within the app-store contract.
- **Offline:** plugin-dependent.
- **CLI:** yes — plugins expose subcommands.

### Web interface — Phase D · L
- **Why:** receive/browse without installing (rides the relay/WebRTC path).
- **Reuses:** FFI/engine, relay.
- **Dependencies:** relay + code-phrase, WebRTC transport.
- **Security:** E2E preserved end to end; relay sees only ciphertext (I5).
- **Storage:** none server-side (no server).
- **Offline:** n/a (browser client).
- **CLI:** n/a.

### iOS frontend — Phase D · L
- **Why:** the engine is already frontend-agnostic (I7); add the platform adapter.
- **Reuses:** FFI, engine.
- **Dependencies:** FFI stable.
- **Security:** same engine guarantees.
- **Storage:** platform app storage.
- **Offline:** yes.
- **CLI:** n/a.

### WebRTC transport + relay/code-phrase — Phase D · L
- **Why:** reach any device over the internet with no shared LAN/tailnet, via a
  short phrase; the last two `RouteManager` tiers.
- **Reuses:** `RouteManager`, `SecureLink`, new relay + rendezvous.
- **Dependencies:** PeerSession, relay server (optional, self-hostable).
- **Security:** app-layer E2E so the relay is untrusted and sees only ciphertext
  (I3/I5).
- **Storage:** none (relay is stateless brokering).
- **Offline:** requires internet.
- **CLI:** yes — `peerbeam send --code`, `peerbeam receive <phrase>`.

### Optional local policy file — Phase D · S
- **Why:** the *only* enterprise concession — a local file to disable features or
  restrict trust, for those who want it.
- **Reuses:** `peerbeam-config`.
- **Dependencies:** none.
- **Security:** local, optional; no server, no MDM (Vision non-goal).
- **Storage:** one config file.
- **Offline:** yes.
- **CLI:** yes — respected by the daemon.

---

## Rejected features

Rejected because they violate the [Vision](VISION.md) non-goals or an
[invariant](ARCHITECTURAL_INVARIANTS.md). Recorded so they are not re-proposed
without an amendment.

| Rejected | Reason |
|---|---|
| Group chat rooms / channels (Slack/Discord style) | Needs a hub to broker a room → violates I3 (P2P-first). Only 1:1 and, later, a trusted device-*mesh* (no server) may be considered. |
| Cloud shared folders (Dropbox style) | Requires hosted storage → violates I4 (no mandatory cloud). P2P folder sync between *your* devices is the sanctioned form. |
| Screen share / full remote control (TeamViewer style) | Named non-goal; unbounded security surface → violates I6 and the Vision. Only a narrow consented command allowlist survives. |
| SSO / MDM / admin console / cloud audit | Requires servers/accounts → violates I3/I4; unrealistic for a solo maintainer. Only an *optional local* policy file is kept. |
| Cross-device message editing at scale; early universal full-text search | Distributed edit-conflict and global-index cost with low near-term value → deferred by DR2 (simplicity). Per-feature local search is kept. |
| JSON viewer · Favorites · Collections · Tags (as platform modules) | UI conveniences, not capabilities → violate I2/I8 as standalone modules. Folded into the relevant feature UIs instead. |
| Any feature opening its own socket/handshake/crypto | Violates I5/I6/I9 and FUTURE_ARCHITECTURE §1 — must be a message type on PeerSession. |
