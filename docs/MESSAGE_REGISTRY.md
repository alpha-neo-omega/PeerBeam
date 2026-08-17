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
| `0x0101` | Chat | B | text/markdown messages; file references (2a) + file declines (2b), implemented |
| `0x0102` | Clipboard | B | clipboard payloads / sync |
| `0x0103` | Presence | B | device status heartbeats; `Status` (implemented), opt-in and trusted-only |
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

Each ChannelType defines its own messages. Entries marked **implemented** are live on
the wire today and are binding; the rest are reserved and illustrative, with detail
belonging to each capability's future spec:

- **Transfer (0x0100):** `Meta`, `ResumeAck`, `Chunk`, `Complete`, `Verify`,
  `Cancel`, `Pause`, `Resume` (implemented) — i.e. today's [transfer
  protocol](TRANSFER_PROTOCOL.md), unchanged, now scoped to this channel.
- **Chat (0x0101):** `Message = 1` (implemented, 1a); `FileRef = 2` (implemented,
  2a) — a reference to a file shared in the conversation: a bare file `name`
  (never a path — the sender's filesystem layout is private), a `size`, and an
  `id` that doubles as the file's transfer id on the Transfer channel
  (0x0100), so the chat row and the byte transfer are correlated by one
  shared id. Sent `OPTIONAL` (§6/§7) so a peer that does not understand it
  simply ignores the frame instead of failing the channel. `FileRef` also
  carries the first feature bit assigned on the Chat capability:
  `CHAT_FEAT_FILEREF = 1 << 0` (`peerbeam_domain::session::CHAT_FEAT_FILEREF`,
  a bit of `Capability.features` for `ChannelType::CHAT`). A sender only
  offers `FileRef` to a peer whose negotiated Chat capability includes this
  bit — an older peer advertises `features: 0`, `CapabilitySet::intersect`
  ANDs it away, and the sender never emits a `FileRef` to it (Capability-
  advertised, not assumed, §7); this is layered on top of, not instead of,
  the `OPTIONAL` flag above.
  **Since 2b, `FileRef.id` is validated on decode** (`FileRef::from_frame`),
  as `name` already was. The rule is the transfer-id rule, because this id
  *is* a transfer id as well as a storage key: 1–128 bytes, `[A-Za-z0-9._-]`
  only, never `.` or `..`. **This narrows what the wire accepts** — a peer
  minting an exotic id that a 2a build decoded is now refused, and because
  the refusal is a frame-decode error it closes the Chat channel (never the
  session or its other channels, per §6's channel-scoped rule). It is
  permitted under §7's *validation may tighten* rule because **the looser
  domain was never interoperable**: this one field has two consumers, and the
  receiving side has always validated it as a transfer id
  (`is_valid_transfer_id`), minting a fresh id when it failed. So an exotic id
  accepted by the chat decoder opened a row keyed by that id while its bytes
  were correlated under a different, locally-minted one — the row and its
  file stopped being one thing. No working behaviour is being removed; two
  consumers of one field disagreed, and 2b makes them agree. The same rule
  runs on encode (`FileRef::to_frame`), so no PeerBeam build can emit an id
  its own peers would reject; a non-conforming id therefore only arrives from
  another implementation. An id is **rejected, never sanitised** — a rewritten
  id could collide with an unrelated row or an unrelated file.
  `FileDecline = 3` (implemented, 2b) — "the file you offered under this `id`
  was turned down": an `id` (the `FileRef`'s, hence also the transfer's) and a
  `timestamp`, nothing else. It is an ordinary chat message rather than a
  transfer-channel signal so that a decline made while the *sender* is offline
  queues in the decliner's own outbox and delivers later, over machinery that
  already exists. Sent `OPTIONAL` (§6/§7), like `FileRef`. It carries the
  second feature bit assigned on the Chat capability:
  `CHAT_FEAT_FILEDECLINE = 1 << 1`
  (`peerbeam_domain::session::CHAT_FEAT_FILEDECLINE`). Advertising it asserts
  that **this peer understands/handles MessageType 3** — that telling it "I
  declined your file" will mean something — and a sender must see the bit in
  the *negotiated* set before putting one on the wire (Capability-advertised,
  not assumed, §7). A peer that predates the feature advertises `features: 0`,
  `CapabilitySet::intersect` clears the bit, and no `FileDecline` is ever put
  on the wire toward it — it would skip the frame harmlessly, but sending a
  type the negotiation says the peer does not speak is exactly the silent
  drift §7 exists to prevent. A file offered to such a peer therefore gets no
  refusal signal at all, and PeerBeam retires it with a bounded local budget
  instead: three offers that *reached* the peer and were refused, then
  terminal (a connection failure is not counted — nobody was asked).
  PeerBeam's own builds advertise the bit for both halves, sending a
  `FileDecline` when their user refuses a file and settling their own outgoing
  row on receiving one. `Receipt`, `Reaction`, `Edit` reserved (not
  implemented; they were never numbered, so `3` was free and nothing was
  renumbered). The Chat handler honors §6: unknown MessageTypes flagged
  `OPTIONAL` are ignored and the channel continues; unknown required types
  close that channel only. `Message`'s body is capped at `MAX_BODY = 16384`
  bytes (`peerbeam-chat::message::MAX_BODY`, pinned by a unit test) — this is
  a **frozen wire constant**: raising it is a breaking change for any peer
  still on the old cap (an older peer's decoder would reject an over-cap
  frame as `ChatError::TooLarge`, closing that channel), so it requires
  capability negotiation, not a silent bump.
- **Presence (0x0103):** `Status = 1` (implemented) — one heartbeat describing
  the sender: `battery_percent`, `charging`, `storage_free_bytes`, `network`,
  `app_version`, `sent_at`. Every field except `sent_at` is optional and
  `#[serde(default)]`, because *absent* is the normal answer for most of them
  — a desktop has no battery, and the Windows/macOS battery collector is
  deliberately not implemented. A receiver must render absence as absence; a
  missing reading is not a zero. Sent on channel open and every 60s while the
  channel stays open; nothing is persisted, so a restart shows no status
  rather than presenting a stale reading as current.
  Sent `OPTIONAL` (§6/§7) and gated on the capability's first feature bit,
  `PRESENCE_FEAT_STATUS = 1 << 0`
  (`peerbeam_domain::session::PRESENCE_FEAT_STATUS`): a peer that does not
  advertise it is sent nothing and shows as "status not shared", never as an
  error.
  **Two receiver-side validations are binding**, because every field is
  peer-supplied: `battery_percent > 100` rejects the whole message rather than
  clamping it (a device that cannot count to 100 has not earned belief in its
  other readings), and a `network` word outside the closed vocabulary
  (`lan` · `wifi` · `ethernet` · `tailscale` · `unknown`) is dropped to `None`
  on decode rather than reaching a surface verbatim.
  Sending is additionally gated locally, and neither gate is on the wire: the
  sender must have opted in (default **off**), and the peer must be
  **trusted**. Battery, free disk and network kind are a device fingerprint;
  they go to the user's own pinned devices or nowhere. See
  `peerbeam_presence::gate::may_share_status`, which is the single place all
  three conditions are decided.
  `Subscribe` / `Unsubscribe` remain **reserved, not implemented** — today's
  model is an unconditional heartbeat to peers that already passed the gates.

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
- **Validation may tighten; meaning may not.** The set of values a field accepts may be
  narrowed within a major version *only* when the looser domain was never interoperable
  — e.g. a value one consumer accepted while another already refused it. Such a change
  must be recorded in §4 against the MessageType, must state the new rule exactly, must
  be enforced symmetrically on encode and decode so no conforming build can emit what
  its peers refuse, and must name the resulting failure mode (per §6, a decode failure
  closes that channel only). Widening is always safe. Narrowing that would break a
  genuinely working peer is a major-version change.

## 8. Amending the registry

Assigning a first-party ChannelType or a Control MessageType is a change to this
document. Because these ids are a long-term contract, treat additions conservatively
(DR2) and record them here. The registry is **derived**, so ordinary additions do not
require a constitutional amendment — but a change that would break an existing id's
meaning is a wire-breaking change and is governed by [VERSIONING.md](VERSIONING.md)
and, where it touches an invariant, the constitution.

---

*Evolution rules: [VERSIONING.md](VERSIONING.md).*
