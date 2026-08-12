# PeerSession Message Registry

> **Status: DERIVED specification** (stability-critical). Conforms to the
> constitutional documents; **specification only — no production code**. Companion to
> [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md) and [VERSIONING.md](VERSIONING.md).

Two coordinated namespaces make PeerSession extensible without protocol changes:

1. **ChannelType** (`u16`) — *what a channel is* (Transfer, Chat, …). Chosen when a
   channel is opened.
2. **MessageType** (`u16`) — *a message within that channel*. Each ChannelType owns
   its own MessageType namespace, starting at `1` (`0` reserved).

A session frame carries `channel_id` (bound to a ChannelType at open) + `message_type`
(within that channel) — see the frame layout in [PROTOCOL.md](PROTOCOL.md §4).

The registry is deliberately sparse and range-partitioned so it can grow for a decade
without renumbering (DR2 — stability over cleverness).

---

## 1. ChannelType ranges

| Range | Purpose | Allocation rule |
|---|---|---|
| `0x0000` | **Control** (session's own protocol) | fixed, reserved forever |
| `0x0001 – 0x00FF` | Core session-reserved | reserved for future session mechanics |
| `0x0100 – 0x0FFF` | **First-party capabilities** | assigned in this document by amendment/PR |
| `0x1000 – 0x7FFF` | Future first-party | reserved; do not use until assigned |
| `0x8000 – 0xBFFF` | **Plugin channels** | allocated at negotiation, namespaced by plugin id (§5) |
| `0xC000 – 0xFFFF` | Vendor-private / experimental | never present in official builds; ignored by them |

## 2. Assigned first-party ChannelTypes

Reserved now; **not implemented** in Phase A1 (only `Control` and `Transfer` are
near-term). Assignments are stable once published.

| Id | ChannelType | Phase | Notes |
|---|---|---|---|
| `0x0000` | Control | A | session protocol; always present |
| `0x0100` | Transfer | A | today's file/folder transfer, reframed as a channel |
| `0x0101` | Chat | B | text/markdown messages |
| `0x0102` | Clipboard | B | clipboard payloads / sync |
| `0x0103` | Presence | B | device status heartbeats |
| `0x0104` | Sync | C | folder/dataset reconciliation (reuses Transfer for bytes) |
| `0x0105` | Notes | C | rides Sync; may not need its own channel |
| `0x0106` | Command | C/D | consent-gated automation / permissioned actions |
| `0x0107 – 0x0FFF` | *(reserved)* | — | future first-party capabilities |

## 3. Control channel (0x0000) message set

The session's own protocol. MessageTypes here are stable and additive-only.

| Id | Message | Direction | Purpose |
|---|---|---|---|
| `1` | `SessionHello` | both | protocol version, capabilities, feature flags, SessionId |
| `2` | `OpenChannel` | both | request a channel `{channel_type, params}` |
| `3` | `ChannelOpened` | both | accept `{channel_id}` |
| `4` | `ChannelRefused` | both | reject `{channel_type, reason: Denied\|Unsupported\|Limit}` |
| `5` | `CloseChannel` | both | close `{channel_id, reason}` |
| `6` | `Ping` | both | keepalive `{nonce, ts}` |
| `7` | `Pong` | both | keepalive reply `{nonce, ts}` |
| `8` | `Shutdown` | both | graceful teardown `{reason}` |
| `9` | `ResumeRequest` | redialler | reconnect `{token}` — a single-use, master-keyed resume token (§below); sent **plaintext** on the fresh control stream (it is self-authenticating) |
| `10` | `ResumeAck` | accepter | `{accepted: bool, reason}` — **sealed** under the epoch control key when accepted (proves master possession); plaintext on refusal |
| `11` | `Unsupported` | both | generic "I don't understand type X" |

**Resume token (M6).** The `ResumeRequest` token is an HMAC-SHA256 over an immutable
binding — `{session_id, device-id pair (unordered), protocol version, epoch,
created_at, expires_at}` — keyed by a **resume key** derived from the session master
secret (never transmitted). It is single-use: each token authorises one strictly
increasing `epoch`, and the accepter rejects any epoch it has already consumed
(replay). Verification is fail-closed: a tampered, wrong-pair, wrong-version,
expired, or replayed token is refused (`ResumeAck{accepted:false}`) and the session
falls back to a fresh handshake. On acceptance both peers re-derive **all** channel
keys at the new epoch (M4 derivation mixes the epoch), so counters restart under
fresh keys with no nonce reuse. Resume never repeats the authenticated handshake
(I5/I6 are met by the master-keyed token + sealed ack). See
[STATE_MACHINES.md §7](STATE_MACHINES.md#7-reconnect-and-resume).

## 4. Per-channel MessageType namespaces

Each ChannelType defines its own messages (illustrative; **not** implemented here —
detail belongs to each capability's future spec):

- **Transfer (0x0100):** `Meta`, `ResumeAck`, `Chunk`, `Complete`, `Verify`,
  `Cancel`, `Pause`, `Resume` — i.e. today's [transfer
  protocol](TRANSFER_PROTOCOL.md), unchanged, now scoped to this channel.
- **Chat (0x0101):** `Message = 1` (implemented, 1a); `Receipt`, `Reaction`, `Edit` reserved (not implemented). The Chat handler honors §6: unknown MessageTypes flagged `OPTIONAL` are ignored and the channel continues; unknown required types close that channel only. `Message`'s body is capped at `MAX_BODY = 16384` bytes (`peerbeam-chat::message::MAX_BODY`, pinned by a unit test) — this is a **frozen wire constant**: raising it is a breaking change for any peer still on the old cap (an older peer's decoder would reject an over-cap frame as `ChatError::TooLarge`, closing that channel), so it requires capability negotiation, not a silent bump.
- **Presence (0x0103):** `Heartbeat`, `Subscribe`, `Unsubscribe`.

A capability may add MessageTypes to its own namespace at will; that is a
backward-compatible (minor) change (§6).

## 5. Plugin message allocation

- Plugins never receive first-party ids. A plugin declares a **plugin identifier**
  (a stable string, e.g. reverse-DNS) during capability negotiation; the session maps
  it to an available ChannelType in the `0x8000–0xBFFF` range **for that session**.
- Two plugins cannot collide: the mapping is per-session and keyed by the negotiated
  plugin id, not by a globally hard-coded number.
- A peer that lacks the plugin simply never advertises it, so its channel is never
  opened (capability negotiation, §6). Plugin channels are subject to the same trust,
  consent, sealing, and validation as first-party channels (I5/I6) — a plugin gets no
  privilege the protocol does not already grant.
- The public plugin API is a **Phase D** concern; this range is reserved now so it
  exists when needed, with no implementation today (DR2 — no speculative build).

## 6. Unknown message behavior

Fail-safe, never fail-crash (I11):

- **Unknown ChannelType** (in `OpenChannel`) → responder replies `ChannelRefused{
  reason: Unsupported }`. The session continues.
- **Unknown MessageType within a known channel** → governed by the frame's
  `OPTIONAL` flag:
  - `OPTIONAL` set → the receiver **ignores** the message and continues (forward
    compatibility for additive messages).
  - `OPTIONAL` unset (i.e. required) → the receiver closes **that channel** with a
    typed `Unsupported` error; the rest of the session is unaffected.
- **Unknown Control MessageType** → reply `Unsupported{message_type}`; never tear down
  the session for an unrecognized control message unless it was required for
  establishment.

## 7. Forward compatibility rules

- **Additive-only within a major version.** New ChannelTypes and new MessageTypes may
  be added; existing ids and their meanings are **immutable**.
- **The `OPTIONAL` flag is the forward-compat lever:** ship a new message as
  `OPTIONAL` so older peers skip it safely; promote it to required only in a new
  major version (VERSIONING.md).
- **No renumbering, ever.** A wrong or deprecated id is retired (left reserved), not
  reused.
- **Capability-advertised, not assumed.** A sender only emits a MessageType/channel
  the peer advertised support for; forward compatibility is negotiated, not hoped for.

## 8. Amending the registry

Assigning a first-party ChannelType or a Control MessageType is a change to this
document. Because these ids are a long-term contract, treat additions conservatively
(DR2) and record them here. The registry is **derived**, so ordinary additions do not
require a constitutional amendment — but a change that would break an existing id's
meaning is a wire-breaking change and is governed by [VERSIONING.md](VERSIONING.md)
and, where it touches an invariant, the constitution.

---

*Evolution rules: [VERSIONING.md](VERSIONING.md).*
