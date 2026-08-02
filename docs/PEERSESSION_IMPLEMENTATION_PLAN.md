# PeerSession Implementation Plan (Phase A2)

> **Status: DERIVED planning document.** Conforms to the constitutional set
> ([VISION](VISION.md) · [ARCHITECTURAL_INVARIANTS](ARCHITECTURAL_INVARIANTS.md) ·
> [FUTURE_ARCHITECTURE](FUTURE_ARCHITECTURE.md) · [ROADMAP](../ROADMAP.md)). Where it
> conflicts, they govern. **Validation + planning only — no production code is
> modified.** Implements the spec in [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md).

Companions: [MILESTONES](PEERSESSION_MILESTONES.md) ·
[MIGRATION](PEERSESSION_MIGRATION.md) · [TEST_PLAN](PEERSESSION_TEST_PLAN.md) ·
[RISK_REGISTER](PEERSESSION_RISK_REGISTER.md) ·
[DEPENDENCY_GRAPH](PEERSESSION_DEPENDENCY_GRAPH.md).

---

## 1. Architecture validation summary

**Conclusion: PeerSession can be implemented inside the current architecture with no
redesign and no invariant violation.** Every element of the A1 spec maps to an
existing seam. The strongest evidence is that the transport *already multiplexes*:
one QUIC connection carries a bidirectional frame stream **and** a separate
unidirectional progress stream today.

| Spec element (A1) | Already exists in code | Change needed |
|---|---|---|
| Multiplexed channels | QUIC connection runs `open_bi`/`accept_bi` (main) **and** `open_uni`/`accept_uni` (progress) — [transfer-quic/src/link.rs:152,170](../rust/crates/peerbeam-transfer-quic/src/link.rs#L152), [lib.rs:168,205](../rust/crates/peerbeam-transfer-quic/src/lib.rs#L168) | generalize 1 bi + 1 uni → **N bi-streams** |
| Side-channel precedent | `Link::progress_sink`/`progress_source` ([port/transfer.rs:91,97](../rust/crates/peerbeam-domain/src/port/transfer.rs#L91)) — "run it on their own resource (e.g. a separate QUIC stream)" | promote the pattern to typed channels |
| Session establishment | `RouteManager::connect(peer, session) -> Box<dyn Link>` ([route_manager.rs:90](../rust/crates/peerbeam-engine/src/route_manager.rs#L90)) | wrap the returned Link in a PeerSession |
| Reconnect | `RouteManager::link_factory` ([route_manager.rs:131](../rust/crates/peerbeam-engine/src/route_manager.rs#L131)) + `LinkFactory` ([recover.rs:31](../rust/crates/peerbeam-transfer/src/recover.rs#L31)) | drive from the session state machine |
| Relay route | `RouteManager::with_relays` ([route_manager.rs:60](../rust/crates/peerbeam-engine/src/route_manager.rs#L60)) | none (already lowest-priority) |
| Authentication | `authenticate(link, identity, enc, trust) -> Session` ([auth.rs:111](../rust/crates/peerbeam-transfer/src/auth.rs#L111)) | run once per session on control channel |
| Per-frame sealing | `SecureLink` ([secure.rs:26](../rust/crates/peerbeam-transfer/src/secure.rs#L26)) | derive **per-channel** keys via existing `kdf` |
| Framing | `Frame{kind, payload}` + `FrameKind` ([port/transfer.rs:25,40](../rust/crates/peerbeam-domain/src/port/transfer.rs#L25)) | add `channel_id` + generalize `kind` → message-type registry |
| Resume | `ReliabilityStore` + `recover.rs` retry-across-links | add session resume token + per-channel checkpoints |
| Capability = port+adapter | domain `port/*.rs` traits, engine builder wiring | add ports, don't edit existing |

No step requires editing the domain's dependency direction, replacing a working
crate, or adding a cloud/account/telemetry surface. **I1–I12 all hold** (compliance
table in [PEERSESSION_SPEC.md §21](PEERSESSION_SPEC.md#21-constitutional-compliance)).

## 2. Name mapping (brief → repository)

The Phase A2 brief names some crates that do not exist under those names. The real
crates are:

| Brief name | Actual crate(s) / location |
|---|---|
| peerbeam-network | `peerbeam-transfer-quic` (transport) + `peerbeam-engine` (RouteManager, route selection) |
| peerbeam-securelink | `SecureLink` in `peerbeam-transfer/src/secure.rs` |
| peerbeam-trust | `peerbeam-trust-fs` + domain `port/trust.rs` (`TrustStore`) |
| peerbeam-discovery | `peerbeam-discovery-udp` / `-mdns` / `-tailscale` + domain `port/discovery.rs` |
| peerbeam-storage | `peerbeam-storage-fs` + domain `port/storage.rs` |
| "Session management" | today only `TransferSession` (entity) + auth `Session` (keys) — see §4 naming risk |

## 3. Where PeerSession integrates

PeerSession is a **new layer between `RouteManager` (which yields a `Link`) and the
transfer logic (which consumes a `Link`)**. Today `stream.rs`/`folder.rs` run a
protocol directly on the `Link` from `RouteManager::connect`. PeerSession interposes:
`RouteManager` → `Link` → **PeerSession** (auth + negotiate + mux) → per-channel
`Link`-like handles → transfer/chat/… handlers. Because a channel presents the same
`send_frame`/`recv_frame` surface as `Link` ([port/transfer.rs:78](../rust/crates/peerbeam-domain/src/port/transfer.rs#L78)), the existing transfer code can run **unchanged on a channel** (validated design choice, DR2).

## 4. Crate-by-crate impact

Risk = likelihood × blast-radius, scored S/M/H. "Additive" = existing APIs unchanged.

### peerbeam-domain
- **Current responsibility:** entities, ports, events, errors; zero IO (I1).
- **Changes:** add new ports — `Session`/`MessageChannel`, `MessageHandler`; add
  message-registry types (`ChannelType`, `MessageType`) and a `SessionFrame` shape.
  Extend `FrameKind`/`Frame` or add a session-frame type alongside.
- **Public APIs affected:** new traits/types (additive). Existing `Link`,
  `TransferProvider`, `Frame` unchanged.
- **New traits:** `Session`, `MessageChannel`, `MessageHandler`.
- **Migration complexity:** S. **Compatibility:** additive. **Testing:** unit
  (codec/registry). **Risk:** S.

### peerbeam-transfer (auth, secure, protocol, stream, folder, recover, control)
- **Current responsibility:** streaming file/folder/clipboard transfer, `authenticate`,
  `SecureLink`, `LinkFactory` recovery.
- **Changes:** (a) promote `authenticate` from per-transfer to per-session (same code,
  new call site); (b) extend `SecureLink` ([secure.rs:26](../rust/crates/peerbeam-transfer/src/secure.rs#L26)) to derive **per-channel** keys via the existing `kdf` (auth.rs) — the one security-critical change (nonce isolation); (c) reframe `stream.rs`/`folder.rs`/`protocol.rs` as the **Transfer channel handler** — they already operate on a `Link`, so they move behind a channel with minimal edits; (d) generalize `recover.rs` `LinkFactory` into session reconnect.
- **Internal APIs affected:** `SecureLink` key derivation; `protocol.rs` `Frame`
  construction (add channel/message-type header).
- **New adapters/handlers:** `TransferMessageHandler` (wraps existing send/recv).
- **Migration complexity:** M. **Compatibility:** wire change (guarded by negotiation,
  §Migration). **Testing:** protocol, replay, resume, large-file. **Risk:** M (H for
  the per-channel key change → security review).

### peerbeam-transfer-quic (link, lib, tls)
- **Current responsibility:** QUIC transport; one connection = one bi-stream Link +
  one uni progress stream.
- **Changes:** allow **multiple bi-streams** per connection (open_bi/accept_bi per
  channel) — a generalization of code that already opens a second (uni) stream
  ([link.rs:152,170](../rust/crates/peerbeam-transfer-quic/src/link.rs#L152)). Add a
  connection-level "open/accept channel stream" surface.
- **Public APIs affected:** `TransferProvider::dial`/`serve` may gain a session-aware
  variant; the `Link` returned becomes session-capable (additive trait, default off).
- **Migration complexity:** M. **Compatibility:** additive at the QUIC layer (streams
  are native). **Testing:** concurrency (N streams), flow-control, stress. **Risk:** M.

### peerbeam-engine (route_manager, device_manager, builder, engine)
- **Current responsibility:** composition root; RouteManager selection/connect/reconnect;
  DeviceManager; wiring.
- **Changes:** add a **session dispatch** role — own PeerSessions per peer, route
  `OpenChannel` to registered `MessageHandler`s. `RouteManager` unchanged in API; the
  engine wraps its `Link` in a PeerSession. Register handlers in `builder.rs`
  ([builder.rs:56](../rust/crates/peerbeam-engine/src/builder.rs#L56) pattern).
- **New traits:** none in engine (uses domain ports). **New wiring:** handler registry.
- **Migration complexity:** M. **Compatibility:** additive. **Testing:** integration
  (two-node session, mixed channels). **Risk:** M.

### peerbeam-ffi (transfer, runtime, events, dto, …)
- **Current responsibility:** C-ABI bridge; commands + event callback
  (`pb_set_event_callback`, `pb_transfers_active`, `pb_devices_json`, …).
- **Changes:** additive — new `pb_*` functions and event families for session/channel
  status (Phase B adds chat/presence commands). Existing transfer FFI keeps working
  (it rides the Transfer channel after cutover).
- **Public APIs affected:** new exports only; `pb_abi_version` bumped when the ABI
  grows.
- **Migration complexity:** S–M. **Compatibility:** ABI-versioned. **Testing:** FFI
  round-trip. **Risk:** S.

### bins/peerbeam-cli (cli, commands, engine)
- **Current responsibility:** `Command` enum ([cli.rs:49](../rust/bins/peerbeam-cli/src/cli.rs#L49)): send, clipboard, daemon, config, dev.
- **Changes:** additive new subcommands later (`chat`, `pipe`, `status` detail); none
  required for A-phase session plumbing.
- **Migration complexity:** S. **Compatibility:** additive. **Risk:** S.

### Flutter (lib/sdk, features)
- **Current responsibility:** frontend over FFI events/commands.
- **Changes:** none for A-phase; consumes new session/channel events additively when
  Phase B capabilities land. **Risk:** S.

### Unchanged crates (explicitly no change)
`peerbeam-crypto` (X25519/AES-GCM — reused as-is via `EncryptionProvider`),
`peerbeam-trust-fs` (`TrustStore` reused), `peerbeam-storage-fs`,
`peerbeam-reliability-fs` (reused for resume), `peerbeam-discovery-*`,
`peerbeam-clipboard-mem`, `peerbeam-config`, `peerbeam-platform`,
`peerbeam-telemetry`, `peerbeam-app` (`DeviceStore`).

## 5. New domain ports (additive — I2)

| Port | Purpose | Implemented by |
|---|---|---|
| `Session` / `MessageChannel` | open/accept a session; open channels; send/recv typed messages | `peerbeam-transfer` (generalized) + `-transfer-quic` |
| `MessageHandler` | interpret one channel type → engine events | one per capability |

`PresenceProvider`, `SyncProvider`, `AppStore` are **Phase B/C** ports, not A2.

## 6. The one naming risk (must resolve in M1)

Three distinct "session" concepts already coexist:
`TransferSession` (entity — a transfer's descriptor, passed to `dial`/`connect`,
[port/transfer.rs:112](../rust/crates/peerbeam-domain/src/port/transfer.rs#L112)),
auth `Session` (the derived crypto keys, [auth.rs](../rust/crates/peerbeam-transfer/src/auth.rs)),
and the new **PeerSession** (the connection-level session). The plan introduces
`PeerSession` as a clearly distinct type and does **not** rename the existing two in
the same change (avoid churn, DR2); a later, optional clarity rename is tracked in the
[risk register](PEERSESSION_RISK_REGISTER.md). This is an integration-clarity risk,
not a constitutional conflict.

## 7. Reuse ledger (what is NOT rebuilt)

X25519/AES-GCM, HMAC key confirmation, TOFU pinning, the QUIC endpoint + TLS,
route selection/probing/failover, the LinkFactory retry loop, the ReliabilityStore
checkpoint, the progress back-channel, the `Frame` codec, and all transfer streaming
logic are **reused**. PeerSession is assembly + generalization of these, per DR1/DR2.
