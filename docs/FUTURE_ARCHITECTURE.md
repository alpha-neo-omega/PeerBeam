# Future Architecture

> **Status: CONSTITUTIONAL**
>
> This document is authoritative.
>
> Any feature, milestone, refactor, protocol change, or architectural decision must conform to this document.
>
> Changes require an explicit constitutional amendment.

---

This document defines **how** PeerBeam grows without changing what already works.
It is the bridge between the frozen present ([ARCHITECTURE.md](ARCHITECTURE.md),
which describes the code as built) and the long-term [Vision](VISION.md). It is
constitutional because the *mechanism* of growth — not just the goals — must stay
stable.

The rule it enforces: **new capabilities extend PeerSession; they do not create
parallel systems, and they do not modify existing architecture.**

---

## 1. The central abstraction: PeerSession

Every peer interaction follows one path:

```
Peer → PeerSession → Typed Message → Handler → Engine
```

- **Peer** — a device known to the [DeviceStore](ARCHITECTURE.md), reachable via a
  discovery provider, and pinned in the `TrustStore` (invariant I6).
- **PeerSession** — an authenticated, end-to-end-encrypted, route-managed channel
  between two trusted devices. It is the generalization of today's transfer
  connection: it is built from the existing `SecureLink` (E2E, I5), placed on the
  best path by `RouteManager` (with failover/migration), and gated by the
  `TrustStore`. A session multiplexes many logical streams.
- **Typed Message** — a framed unit on the session carrying a **message-type tag**
  and a versioned payload. **File transfer is one message type.** Chat, clipboard,
  presence, sync-delta, and control are others.
- **Handler** — the code that interprets one message type. Handlers are registered
  at the composition root; the session dispatches each message to its handler.
- **Engine** — `peerbeam-engine` remains the composition root that wires providers
  and owns event/device streams. It gains a session/dispatch role; it does not gain
  feature logic (invariant I8).

**Non-negotiable (I-level):** no capability may open its own socket, run its own
handshake, or define its own crypto. If it talks to a peer, it is a message type on
a PeerSession. This is what keeps trust, encryption, routing, and protocol
versioning in exactly one place.

## 2. How the layers absorb this

The [hexagonal layering](ARCHITECTURE.md) is unchanged. The additions are **new
ports and new adapters** — the exact extension mechanism the architecture was built
for (invariant I2).

```
        Frontends:  Flutter · CLI · (future) web · (future) iOS
                              │ uses
        Composition root:  peerbeam-engine
          + session dispatch (message-type → handler)   ← added role, not new logic
                              │ depends on
        Application:  peerbeam-app  (registry, DeviceStore)
                              │ depends only on
        Domain:  peerbeam-domain
          + NEW ports:  Session · MessageHandler · PresenceProvider ·
                        SyncProvider · AppStore
                              ▲ implement ports
        Adapters (existing, unchanged):
          discovery-* · transfer · transfer-quic · storage-fs ·
          crypto · reliability-fs · trust-fs · clipboard-mem
        Adapters (new, one per capability):
          messaging · presence · sync · appstore-fs
```

Existing adapters are **not modified** to add a capability. `peerbeam-transfer`
gains exactly one thing: its framing learns a message-type discriminator, behind
the protocol-version negotiation of I9, so that "file transfer" becomes one
labeled variant rather than the only thing on the wire.

## 3. Extension points (the ports new work implements)

| Port (in `peerbeam-domain::port`) | Purpose | Implemented by |
|---|---|---|
| `Session` / `MessageChannel` | Open/accept a PeerSession; send/receive typed messages; multiplex streams | `peerbeam-transfer` (generalized) + `peerbeam-transfer-quic` |
| `MessageHandler` | Interpret one message type; produce engine events | one per capability adapter |
| `PresenceProvider` | Publish/subscribe device presence (online, battery, storage, network) | `peerbeam-presence` |
| `SyncProvider` | Reconcile a dataset (folder, notes) between two trusted peers | `peerbeam-sync` |
| `AppStore` | Encrypted, local-first, append/KV store for capability data (chat log, notes, clipboard history) | `peerbeam-appstore-fs` |

A capability is therefore always the same shape:

> **one message type** (+ handler) · **one adapter crate** · **optional `AppStore`
> namespace** · **FFI surface** · **CLI subcommand**.

Nothing else in the system changes. This uniformity is the point: a contributor
learns the pattern once.

### Worked example — adding "chat" (illustrative, not a commitment)

1. Define a `Chat` message type and register a `MessageHandler` for it.
2. New crate `peerbeam-messaging` implements the handler; it persists history via
   an `AppStore` namespace (`peerbeam-appstore-fs`), encrypted at rest (I11).
3. The engine wires the handler at build time (I2). No edit to `transfer`,
   `crypto`, `trust`, `discovery`, or `RouteManager`.
4. FFI gains `chat_*` events/commands; CLI gains `peerbeam chat <device>` (I7).
5. Transport, trust, encryption, routing, and resume come **for free** from the
   session.

If this example ever required changing `SecureLink`, `RouteManager`, or the domain
core, that is the signal that the design is wrong — not that the architecture needs
bending.

## 4. Protocol evolution

The wire protocol evolves under invariant I9 (versioned, negotiated) with these
rules:

- **Negotiate at session start.** Peers exchange supported protocol versions and a
  set of supported message types; they proceed at the highest mutually supported
  version.
- **Message-type registry.** Message types are assigned stable identifiers.
  Adding a type is backward-compatible: it does not change existing frames.
- **Unknown-type tolerance.** A peer that receives a message type it does not
  support responds with a typed "unsupported" acknowledgement and continues the
  session. New capabilities never break old peers.
- **Explicit, windowed breaking changes.** A breaking wire change bumps the
  protocol version, is negotiated, and is supported alongside the prior version for
  a documented window. No silent drift (I9).
- **Pre-1.0 latitude.** Before 1.0 the wire may change freely, but every change is
  still versioned so the negotiation machinery is exercised from day one.

## 5. Compatibility strategy

- **FFI ABI version** (`pb_abi_version`) gates breaking native-boundary changes
  independently of the app version, so a Flutter build and an engine build can
  verify compatibility at load.
- **Semantic versioning after 1.0** for the app and the (future) public plugin API.
- **Support window.** Each release states the oldest protocol/ABI it interoperates
  with. Compatibility is a tested claim (DR3), recorded in the compatibility
  matrix, not an assumption.
- **Capability negotiation, not version sniffing.** Behavior is chosen by
  negotiated *capabilities*, so a peer missing a feature degrades gracefully rather
  than failing.

## 6. What this architecture deliberately excludes

- **No parallel networking stacks.** A capability that wants its own connection,
  handshake, or crypto is rejected — it belongs on PeerSession (I5/I6/I9).
- **No feature logic in the engine or a frontend.** The engine dispatches;
  handlers/adapters decide; frontends present (I2/I7/I8).
- **No speculative extension points.** Ports are added when a real capability needs
  them, never "in case" (DR2). The public plugin API is exposed only after its
  ports are proven and versioned.
- **No platform-shaped forks of the core.** Platform limits are documented at the
  edge (e.g. Android + Tailscale), never by branching the architecture (I12).

---

## Amendments

*None.*

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
