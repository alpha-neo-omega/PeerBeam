# PeerSession Specification

> **Status: DERIVED specification** (stability-critical). Conforms to
> [VISION.md](VISION.md), [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md),
> [FUTURE_ARCHITECTURE.md](FUTURE_ARCHITECTURE.md). Where it conflicts, they govern.
> **Specification only — no production code is written for Phase A1.**

Companion documents: [PROTOCOL.md](PROTOCOL.md) (overview + frame layout),
[STATE_MACHINES.md](STATE_MACHINES.md), [MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md),
[VERSIONING.md](VERSIONING.md).

PeerSession is the single communication abstraction:
`Peer → PeerSession → Typed Message → Handler → Engine`. It generalizes today's
one-transfer-per-link wire ([TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md)) into a
multiplexed, versioned, resumable session that any capability uses without inventing
its own networking.

---

## 1. Identifiers

| Id | Definition | Lifetime | Source |
|---|---|---|---|
| **PeerId** | The peer's stable `DeviceId` | Permanent (persistent identity, Phase A) | existing `DeviceId` |
| **SessionId** | 128-bit random value minted by the initiator, echoed by the responder | One logical session, **stable across reconnects** | new |
| **ChannelId** | The underlying QUIC stream id, unique within a session | One channel | transport |
| **ChannelType** | Registry id of the message type a channel carries (0 = Control) | — | [MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md) |

PeerId answers *who*; SessionId answers *which conversation* (and survives a dropped
connection so a reconnect rebinds rather than starting over); ChannelId answers
*which stream within it*.

## 2. Session lifecycle (overview)

```
DISCONNECTED → CONNECTING → AUTHENTICATING → NEGOTIATING → ACTIVE
     ↑                                                        │
     └──────────── CLOSED ←── SHUTTING_DOWN ←─────────────────┤
                       ↑                                       │
                   RECONNECTING ←──── (connection lost) ───────┘
```

Full transition tables and the six required sub-machines are in
[STATE_MACHINES.md](STATE_MACHINES.md). Sections 3–7 below specify each stage.

## 3. Session establishment

1. **Route selection.** `RouteManager` picks the highest-priority reachable route to
   the PeerId (LAN → USB → Ethernet → Wi-Fi → Tailscale → direct internet → relay).
   PeerSession is transport-agnostic; it receives a connected transport, it does not
   choose the path.
2. **Transport connect.** A QUIC (quinn) connection is opened (or accepted). The
   **control channel** (ChannelId for stream 0) is opened first.
3. **Authenticate** (§4) over the control channel.
4. **Negotiate** (§6) protocol version + capabilities + SessionId over the control
   channel.
5. **ACTIVE.** Data channels may now be opened on demand (§8).

The initiator drives 1–4; the responder mirrors. Establishment has a bounded overall
timeout (§14); any failure returns to DISCONNECTED with a typed error (§17).

## 4. Authentication flow

PeerSession reuses the **existing** mutual-auth handshake
([auth.rs](../rust/crates/peerbeam-transfer/src/auth.rs)) unchanged, promoted from
*per-transfer* to *per-session* and run once over the control channel:

```text
A→B: Hello{ device_id, name, pubkey_A, nonce_A }
B→A: Hello{ device_id, name, pubkey_B, nonce_B }
A→B: Confirm{ HMAC(send_key, transcript) }
B→A: Confirm{ HMAC(recv-verified transcript) }
```

- X25519 ECDH → **directional** send/recv keys. HMAC-SHA256 **key confirmation**
  proves the peer holds the private key (mutual auth).
- The **transcript binds both pubkeys, both nonces, and both `device_id`/`name`**,
  so a replayed handshake yields different keys and an on-path identity rewrite
  breaks confirmation.
- Yields the session master keys consumed by SecureLink (§5).

This is invariant I5 (E2E, mutual auth) satisfied by reuse, not reinvention.

## 5. SecureLink integration and the encryption boundary

- Every session frame is sealed with **AES-256-GCM**, nonce =
  `4-byte direction prefix ‖ 8-byte big-endian counter`, exactly as today.
- **Per-channel keys.** Because a session multiplexes many streams, each channel
  derives its own send/recv keys and nonce prefixes from the session master keys and
  the ChannelType/ChannelId via the existing `kdf()`:
  `channel_key = KDF(session_key, channel_id ‖ channel_type)`. This gives every
  channel an independent counter space, so concurrent streams can never reuse a
  nonce — the one real hazard multiplexing introduces, closed by construction.
- **Encryption boundary:** plaintext exists **only in-process, above SecureLink**.
  On the wire, and at any relay, only ciphertext appears. QUIC's TLS is defense in
  depth; the app-layer seal is the guarantee that survives an untrusted relay
  (I3/I5).

## 6. Version and capability negotiation

Immediately after authentication, over the control channel, peers exchange a
`SessionHello`:

- **Protocol version** — `major.minor` (see [VERSIONING.md](VERSIONING.md)). Both
  proceed at the highest common `major`; mismatched majors → typed
  `VersionIncompatible` and close.
- **Capabilities** — the set of ChannelTypes each side supports, with per-capability
  **feature flags**. A peer may only open a channel type the other advertised.
- **SessionId** — initiator's SessionId, echoed to confirm.

Negotiation is **capability-based, not version-sniffing**: behavior is chosen from
advertised capabilities, so a peer missing a feature simply never has that channel
opened, rather than failing. This is what lets new capabilities ship without
breaking old peers (I9; FUTURE_ARCHITECTURE §4).

## 7. TrustStore interaction and capability permissions

- **TOFU pin.** On first contact the peer's public-key fingerprint is pinned; a
  changed fingerprint later (id reuse or MITM) aborts the session (existing
  behavior).
- **`approved` is distinct from `pinned`.** Pinning is anti-MITM; it is **not**
  authorization. Opening a **sensitive** channel type (e.g. remote browse, control
  commands, live clipboard) requires the peer to be `approved` **and** an explicit,
  revocable, per-capability consent — never inferred from "we have a session"
  (I6).
- **Per-channel-type authorization.** The control plane consults a capability-policy
  before accepting an `OpenChannel`: benign types (Presence when opted-in, Transfer
  request→prompt) vs sensitive types (explicit consent each grant). Denied opens
  return `Denied`; the session continues.

## 8. Multiplexing and channels

- One session, **N channels**, each a QUIC bi-stream (uni-stream for one-way
  signals such as Presence heartbeats). Channels open and close independently.
- A channel is opened by a control-plane `OpenChannel{ channel_type, params }`;
  the peer replies `ChannelOpened{ channel_id }` or `Denied/Unsupported`.
- **Isolation:** a slow bulk channel (Transfer/Sync) never head-of-line-blocks an
  interactive one (Chat/Clipboard) because they are separate QUIC streams. This is
  the core reason multiplexing rides QUIC streams rather than a custom mux (DR2).
- Channel count is bounded by policy to prevent resource exhaustion; excess opens
  are `Denied` with a retry hint.

## 9. Ordering guarantees

- **Within a channel:** total order, exactly-once, reliable — the QUIC stream
  guarantee. Data messages therefore carry **no sequence index** (as chunks do not
  today).
- **Across channels:** **no ordering.** Channels are independent; a Chat message and
  a Transfer chunk have no relative order. Capabilities that need cross-channel
  ordering must sequence within a single channel.
- The control channel is ordered with respect to itself; `OpenChannel` establishes a
  happens-before only for that channel's creation.

## 10. Reliability expectations

- Default: **reliable, ordered** per channel (QUIC streams).
- Optional **best-effort** signals (e.g. presence heartbeats, "typing") may use QUIC
  datagrams or a uni-stream where loss is acceptable; this is a per-capability choice
  declared in the registry, defaulting to reliable.
- No capability may assume delivery of a best-effort message; reliability is a
  declared property, not an accident.

## 11. Priority model

Three tiers, mapped to QUIC stream priority:

1. **Control** (channel 0) — always highest; negotiation/keepalive/shutdown must not
   be starved.
2. **Interactive** — Chat, Clipboard, Presence, Control-commands: latency-sensitive,
   small.
3. **Bulk** — Transfer, Sync, media: throughput-sensitive, large.

Scheduler guarantees control liveness and prevents bulk from starving interactive;
within a tier, round-robin fairness. Priorities are advisory hints to the transport,
not a wire contract.

## 12. Flow control

- **Rely on QUIC** connection-level and per-stream flow control; PeerSession does not
  reinvent windowing (DR2).
- **Application backpressure** uses the existing progress/ack mechanism: a receiver's
  consumption governs how fast a sender advances (as receiver-confirmed transfer
  progress works today), keeping buffers bounded (I10).
- No path buffers a whole message; producers stream and yield under backpressure.

## 13. Keepalive

- Control-plane `Ping`/`Pong` at a configured interval measure liveness and RTT
  (feeding `RouteManager`'s route-quality decisions). QUIC's own keepalive is
  enabled as a lower layer.
- Missed `Pong`s beyond a threshold mark the connection dead → RECONNECTING (§16).

## 14. Timeout behavior

Configured, not wire-fixed (so tuning never breaks compatibility):

| Timeout | Scope | On expiry |
|---|---|---|
| Connect | route dial | try next route / fail establish |
| Handshake | auth exchange | abort → DISCONNECTED |
| Negotiate | SessionHello | abort → DISCONNECTED |
| Idle | no traffic **and** no keepalive | graceful CLOSE |
| Request-ack | a control request awaiting reply | typed error, channel-scoped |

## 15. Reconnection

- On connection loss while ACTIVE, the session enters **RECONNECTING**:
  `RouteManager` re-selects the best current route (which may differ — Wi-Fi dropped,
  Tailscale remains) and reconnects.
- The new connection **re-authenticates** and **re-negotiates**, then **rebinds to
  the same SessionId** via a resume token (§16), so the logical session continues.
- Bounded attempts with backoff (generalizing today's `recover.rs`
  `max_attempts` + linear backoff). Exhausted attempts → CLOSED with a typed error.

## 16. Resume behavior

Two layers, so nothing restarts unnecessarily:

- **Session resume.** A resume token (bound to SessionId + peer identity, never
  reused, integrity-protected) lets a reconnect re-attach to the same logical session
  and its open channels rather than forming a new one.
- **Channel resume.** Each channel type defines its own checkpoint semantics; the
  session provides reconnect + re-open, the channel provides the offset. **Transfer**
  reuses the built mechanism: receiver reports on-disk bytes, sender resumes from
  `offset`, checkpointed via the `ReliabilityStore` so it survives a process
  restart. Chat/Sync resume from their last acknowledged item; stateless channels
  (Presence) simply re-subscribe.

## 17. Error handling

A typed taxonomy with fixed dispositions (fail-closed — never guess, I11/DR3):

| Class | Examples | Disposition |
|---|---|---|
| Transport | connection dropped, dial failed | retryable → RECONNECTING (bounded) |
| Auth/Trust | key-confirmation mismatch, changed fingerprint | **fatal**, no retry, session aborts |
| Protocol/Version | incompatible major, malformed control frame | **fatal**, session closes |
| Capability | `Unsupported` type, `Denied` open | **channel-scoped**, session continues |
| Integrity | GCM open failure, checksum mismatch | fatal for that channel/transfer |
| Validation | length bound exceeded, bad path, bad enum | reject the message, close the channel |

An error never escalates wider than necessary: a bad Chat frame closes the Chat
channel, not the session.

## 18. Security summary

- **Authentication sequence** — §4 (existing mutual X25519 + HMAC key confirmation).
- **Replay protection** — per-channel monotonic counter nonces; a duplicate/reordered
  frame carries the wrong counter and fails to open. Resume tokens are single-use.
- **Integrity** — AES-256-GCM tag per frame; any tamper fails to open.
- **Encryption boundary** — §5; plaintext only in-process, ciphertext on the wire and
  at any relay.
- **Trust verification** — TOFU fingerprint pin; changed key aborts; `approved` +
  per-capability consent gate sensitive channels (§7).
- **Capability permissions** — the control plane authorizes each channel-type open
  against trust + consent policy (§7).
- **Message validation** — typed decode, length bounds, enum/range checks, path
  sanitization (no `..`/absolute, as folder transfer already enforces).
- **Failure behavior** — fail-closed everywhere: unknown/invalid/denied → reject and
  scope the failure; never proceed on assumption.

## 19. Extensibility — every capability fits without changing the protocol

Each capability is **one ChannelType + a message set + a handler**, opened over the
same control plane, sealed by the same SecureLink, gated by the same TrustStore. No
protocol change is required to add any of these (FUTURE_ARCHITECTURE §3):

| Capability | Channel | Message shape (illustrative) | Reliability | Resume |
|---|---|---|---|---|
| **Chat** | `Chat` | `Message{id, markdown}`, `Receipt{id, state}` | reliable | last-acked id |
| **Clipboard** | `Clipboard` | `Clip{kind, payload-or-transfer-ref}` | reliable | latest wins |
| **Presence** | `Presence` | `Heartbeat{online, battery, storage, net}` | best-effort | re-subscribe |
| **Sync** | `Sync` | `Manifest`, `Need`, `Delta` (reuses transfer for bytes) | reliable | per-file offset |
| **Notes** | `Notes` (rides Sync) | note deltas | reliable | last-acked rev |
| **Automation** | `Control` | `Rule`, `Trigger` (local action; consent-gated) | reliable | n/a |
| **Plugin** | `Plugin` (namespaced range) | opaque, plugin-defined | declared | plugin-defined |

If any of these ever *required* changing the frame layout, auth, or SecureLink, that
is the signal the capability is mis-designed — not that the protocol should change
(FUTURE_ARCHITECTURE §3; amend only via governance).

## 20. Compatibility strategy

- Behavior is chosen by **negotiated capability**, so old and new peers interoperate
  at the highest common version and skip unsupported channels.
- **Unknown-type tolerance:** unknown ChannelType → `Unsupported`; unknown
  message_type within a known channel → per-channel policy (ignore-optional or
  channel-error), never a session crash.
- The **FFI ABI** (`pb_abi_version`) is versioned independently of the protocol.
- Compatibility is a **tested** claim (DR3), recorded in the compatibility matrix,
  not an assumption. Details in [VERSIONING.md](VERSIONING.md).

## 21. Constitutional compliance

| Invariant | How PeerSession satisfies it |
|---|---|
| I1 inward deps | Session/MessageChannel are domain ports; the domain stays IO-free |
| I2 port+adapter | capabilities = channel types + adapters; no core edits to add one |
| I3 P2P-first | device-to-device session; relay is an optional untrusted route |
| I4 no cloud/tracking | protocol carries no accounts, beacons, or telemetry |
| I5 E2E default | SecureLink seals every frame; per-channel keys; relay sees ciphertext |
| I6 trust-gated | TOFU pin + `approved` + per-capability consent gate channel opens |
| I7 engine/CLI-first | protocol is engine-level; FFI/CLI expose every capability |
| I8 one responsibility | session mux vs per-channel handlers cleanly separated |
| I9 versioned wire | **closes the current no-version-byte gap** via negotiation |
| I10 streaming | per-channel streaming, QUIC flow control, no whole-payload buffers |
| I11 secure/local-first | fail-closed defaults; offline channels queue and resume |
| I12 cross-platform | QUIC/quinn on all targets; platform limits documented, not forked |

Decision rules applied: **DR1** — the design maximally preserves the constitution
(one abstraction, no parallel systems); **DR2** — multiplexing reuses QUIC streams
and auth/SecureLink reuse existing code rather than new machinery; **DR3** — every
claim here is grounded in the existing wire, and the spec is validated by
implementation + tests before it is trusted.

---

*Lifecycles: [STATE_MACHINES.md](STATE_MACHINES.md) · Message types:
[MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md) · Evolution:
[VERSIONING.md](VERSIONING.md).*
