# PeerBeam Protocol

> **Status: DERIVED specification.** Not constitutional, but **stability-critical**:
> this protocol is expected to remain valid for many years and changes under the
> versioning rules in [VERSIONING.md](VERSIONING.md). It must conform to
> [VISION.md](VISION.md), [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md),
> and [FUTURE_ARCHITECTURE.md](FUTURE_ARCHITECTURE.md). Where it conflicts with them,
> they govern. Consider promoting it to constitutional by amendment once
> implemented and proven (see the risks note at the end of
> [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md)).

This is the top-level map of how PeerBeam devices talk. The detail lives in
[PEERSESSION_SPEC.md](PEERSESSION_SPEC.md) (behavior),
[STATE_MACHINES.md](STATE_MACHINES.md) (lifecycles),
[MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md) (message types), and
[VERSIONING.md](VERSIONING.md) (evolution).

**This is a specification, not an implementation.** No production code is written
for Phase A1. The spec describes how the *existing* wire
([TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md)) generalizes into **PeerSession**, the
single communication abstraction the constitution mandates
(`Peer → PeerSession → Typed Message → Handler → Engine`).

---

## 1. Design goal

One authenticated, end-to-end-encrypted, route-managed **session** between two
trusted devices, carrying **many typed message streams** (channels). File transfer
is one channel type; chat, clipboard, presence, sync, notes, control, and plugin
messages are others. No capability opens its own connection, handshake, or crypto
(invariants I5/I6/I9; FUTURE_ARCHITECTURE §1).

## 2. Layered model

PeerSession adds a **session/multiplexing layer** above the stack that exists today.
Everything below the PeerSession layer is already built; PeerSession generalizes the
application layer from "one transfer per link" to "many typed channels per session."

```
   Capabilities:  Transfer · Chat · Clipboard · Presence · Sync · Notes · Control · Plugin
        │                        (each is a Channel of one message type)
   ┌────▼──────────────────────────────────────────────────────────────┐
   │  PeerSession           multiplex · negotiate · keepalive · resume  │  ← NEW (A1)
   │  (control plane + N data channels)                                 │
   ├────────────────────────────────────────────────────────────────────┤
   │  SecureLink            per-frame AES-256-GCM seal/open (I5)        │  ← exists
   │  (per-channel keys, counter nonce)                                 │
   ├────────────────────────────────────────────────────────────────────┤
   │  Link                  ordered, reliable frame stream              │  ← exists
   ├────────────────────────────────────────────────────────────────────┤
   │  Transport             QUIC (quinn): one connection, N bi-streams  │  ← exists
   └────────────────────────────────────────────────────────────────────┘
        chosen and migrated by RouteManager · gated by TrustStore
```

**Key reuse decision (DR2 — simplicity):** multiplexing is **QUIC's native
multi-stream capability**, not a custom mux inside one stream. One PeerSession = one
QUIC connection; one channel = one QUIC bi-stream (or uni-stream for one-way
signals). This gives per-channel ordering, reliability, flow control, and
head-of-line-blocking isolation for free from the transport, and it is the smallest
change to today's "one Link = one bi-stream" model.

## 3. Two planes

- **Control plane** — a single reserved channel (channel `0`) per session. Carries
  session-level messages: version/capability negotiation, `OpenChannel` /
  `CloseChannel`, keepalive `Ping`/`Pong`, `Shutdown`, and `Unsupported`
  rejections. It is established first and lives for the whole session.
- **Data plane** — every other channel. Each carries messages of **one** type
  (Transfer, Chat, …). Opened on demand via the control plane, subject to trust +
  capability permission.

## 4. Frame layout

The transport delivers ordered, reliable **frames**. Today a `Frame` is
`{ kind: FrameKind, payload: Bytes }`. PeerSession keeps that envelope and defines
the payload structure of a **session frame**:

```
 ┌───────────────────────────────────────────────────────────────┐
 │ SecureLink seal:  AES-256-GCM( key = per-channel, nonce = dir‖ctr ) │
 ├───────────────────────────────────────────────────────────────┤
 │ Session frame (plaintext inside the seal):                    │
 │   channel_id     : varint   which channel (0 = control)       │
 │   message_type   : u16      registry id (see MESSAGE_REGISTRY) │
 │   flags          : u8       END_OF_MESSAGE, etc.              │
 │   length         : varint   payload byte length              │
 │   payload        : bytes    message body (raw or CBOR/JSON)   │
 └───────────────────────────────────────────────────────────────┘
```

Notes:
- The **exact integer encodings are fixed at implementation** within these fields;
  the *fields and their meaning* are the stable contract. This avoids
  over-constraining bytes now while keeping the shape years-stable.
- **Bulk payloads (file chunks) ride raw** in `payload` with no JSON/base64 wrapping
  — preserving today's zero-per-chunk-bloat property (I10). Control and small
  messages use a compact structured encoding (CBOR recommended; JSON acceptable).
- Because a channel is an ordered QUIC stream, **data messages carry no sequence
  index** — order is the transport's guarantee, exactly as chunks work today.
- Every session frame is sealed by SecureLink; the header above is **never on the
  wire in cleartext** (I5). The QUIC transport also encrypts (TLS 1.3), but the
  app-layer seal is the E2E guarantee that holds even across an untrusted relay
  (I3/I5).

## 5. Relationship to the existing transfer protocol

[TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md) is **not replaced** — it is reframed as
*"the Transfer message type carried on a PeerSession channel."* Its `Meta → Chunk… →
Complete → Verify`, resume-offset renegotiation, and `Cancel/Pause/Resume` controls
become the message set of channel type `Transfer`. The authentication handshake and
SecureLink sealing it already describes become **session-level** rather than
per-transfer. The historical document stays valid for the transfer channel; this
document governs the session around it.

## 6. What this protocol refuses

- No plaintext peer traffic; no capability-specific crypto or handshake (I5).
- No acting on an unverified or unconsented peer/channel (I6).
- No unversioned wire change — every change is negotiated (I9, VERSIONING.md).
- No whole-message buffering on bulk paths; channels stream (I10).
- No custom in-stream multiplexer when the transport already multiplexes (DR2).

---

*Continue to [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md) for the full behavioral
specification.*
