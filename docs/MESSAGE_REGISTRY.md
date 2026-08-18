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
| `0x0102` | Clipboard | B | clipboard sync; `Clip` (implemented), opt-in and trusted-only |
| `0x0103` | Presence | B | device status heartbeats; `Status` (implemented), opt-in and trusted-only |
| `0x0104` | Sync | C | **implemented** — folder reconciliation; bytes over Transfer |
| `0x0105` | Notes | C | **implemented** — note sync (`NoteBatch`) |
| `0x0106` | Command | C/D | consent-gated automation / permissioned actions |
| `0x0107` | Pipe | B | `peerbeam pipe` — an unbounded stdin↔stdout byte stream; a **stream** channel like Transfer, implemented |
| `0x0108` | Browse | C | **implemented** — read-only listing of shared folders |
| `0x0109 – 0x0FFF` | *(reserved)* | — | future first-party capabilities |

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
  row on receiving one. `Reaction = 4` (implemented) — "this emoji is, or is no longer, on the message
  named by `target_id`": a `target_id`, an `emoji`, a `remove` flag and a
  `timestamp`. **Add and remove are one message with a flag, not a toggle**: a
  toggle derives the new state from what the receiver believes the old one was,
  so a single dropped or duplicated frame leaves the two devices permanently
  disagreeing about whether the reaction is there. Stating the intended end
  state makes the message idempotent, on the wire and in the store alike. Sent
  `OPTIONAL` (§6/§7), like `FileRef` and `FileDecline`, and carrying the third
  Chat feature bit: `CHAT_FEAT_REACTION = 1 << 2`
  (`peerbeam_domain::session::CHAT_FEAT_REACTION`). Advertising it asserts that
  **this peer understands/handles MessageType 4**; a sender must see the bit in
  the *negotiated* set first. A peer that predates reactions would skip the
  OPTIONAL frame harmlessly, so the bit exists for the sender's own sake — it
  reports the gesture as undelivered rather than letting a user believe it
  landed on a screen that never showed it. `target_id` is bounded by `MAX_ID`
  and the reaction by `MAX_REACTION` (64 bytes); neither authorizes anything on
  its own — `ChatStore::apply_reaction` looks the record up **inside that
  peer's own namespace**, so a `target_id` naming a message in a different
  conversation finds nothing rather than reaching across, and an id we have
  deleted is a silent success rather than a channel failure. What counts as an
  emoji is the sending client's business: the bound is a resource limit, not a
  taste test. `Receipt = 5` (implemented) — "I have read your messages up to and including
  `read_through`": a watermark and a `timestamp`. **A watermark, not a
  per-message acknowledgement**, because ids are lexicographically time-ordered
  (`mint_id`), so one id names a prefix of the conversation: a whole thread-read
  costs one frame, re-applying marks nothing new, and a stale watermark arriving
  out of order cannot move a read row back to unread. Sent `OPTIONAL`, carrying
  `CHAT_FEAT_RECEIPT = 1 << 3`. **The bit asserts that a peer can *apply* a
  receipt, deliberately not that its user sends them** — whether this device
  discloses read times is `DeviceConfig::share_read_receipts`, **default off**,
  and conflating the two would put a privacy setting on the wire.
  `ChatStore::apply_receipt` marks only our own **outgoing** rows inside that
  peer's namespace (the direction check `settle_file_row` makes: a peer must not
  rewrite a row it sent us), so a watermark naming an unknown id marks whatever
  is below it and is otherwise a silent success. The opt-in gates **sending
  only**: receipts a peer sends are always applied, so opting out never costs
  you what others choose to tell you — the same asymmetry presence has. `Edit`
  reserved (not implemented; it was never numbered, so `3`, `4` and `5` were
  free and nothing was renumbered). The Chat handler honors §6: unknown MessageTypes flagged
  `OPTIONAL` are ignored and the channel continues; unknown required types
  close that channel only. `Message`'s body is capped at `MAX_BODY = 16384`
  bytes (`peerbeam-chat::message::MAX_BODY`, pinned by a unit test) — this is
  a **frozen wire constant**: raising it is a breaking change for any peer
  still on the old cap (an older peer's decoder would reject an over-cap
  frame as `ChatError::TooLarge`, closing that channel), so it requires
  capability negotiation, not a silent bump.
- **Clipboard (0x0102):** `Clip = 1` (implemented) — one clipboard payload:
  `text` (UTF-8, the only kind carried today) and `sent_at` (RFC3339, the
  sender's clock, display and ordering only — never trusted as absolute, since
  peer clocks are not synchronised). Images and files are **not** in scope and
  must not be smuggled through `text`; a receiver writes it to the system
  clipboard as plain text and nothing else, so carrying another kind needs a
  new MessageType and a new feature bit. There is deliberately **no** source
  application, window title or device-local path here: what was copied is the
  payload, where it came from is not.
  Sent `OPTIONAL` (§6/§7) and gated on the capability's first feature bit,
  `CLIPBOARD_FEAT_CLIP = 1 << 0`
  (`peerbeam_domain::session::CLIPBOARD_FEAT_CLIP`): a peer that does not
  advertise it is sent nothing and simply does not take part in sync, which is
  not an error.
  `text` is capped at `MAX_CLIP = 65536` bytes
  (`peerbeam-clipboard::message::MAX_CLIP`, pinned by a unit test) — a **frozen
  wire constant** on the same terms as Chat's `MAX_BODY`: raising it is a
  breaking change for any peer still on the old cap (that peer's decoder
  refuses the over-cap frame as `ClipboardError::TooLarge`, closing the
  Clipboard channel per §6), so it requires capability negotiation, not a
  silent bump. It is four times Chat's cap because a clipboard routinely holds
  a whole file's worth of code, and it is bounded at all because nothing here
  is a deliberate send: the watcher pushes whatever was copied, to every
  trusted device, with no button press.
  **Two validations are binding and symmetric on encode and decode** (§7), and
  both **reject rather than repair**: an over-cap clip is **skipped, never
  truncated** (a truncated clipboard silently corrupts what the user believes
  they copied, which is worse than not syncing), and an **empty** `text` is
  refused outright (applying one would *erase* the peer's clipboard, a
  destructive act with no user intent behind it). A non-UTF-8 payload is
  likewise rejected rather than lossily replaced.
  Sending is additionally gated locally, and neither gate is on the wire: the
  sender must have opted in (default **off**), and the peer must be
  **trusted**. The clipboard is the single most sensitive buffer on a desktop
  and nothing can tell which clips are secrets, so it goes to the user's own
  pinned devices or nowhere. See `peerbeam_clipboard::gate::may_share_clip`,
  which is the single place all three conditions are decided.
  Receiving is **ungated**, which is what lets a phone take part: Android 10+
  forbids background clipboard reads, so a phone can never auto-send, yet it
  advertises `CLIPBOARD_FEAT_CLIP` truthfully and applies an incoming clip in
  full. Desktop sends, every platform receives.
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
- **Pipe (0x0107):** a **stream** channel, like Transfer (0x0100) and unlike
  every other entry in this section — so it defines **no MessageTypes of its
  own, and never will while it stays a stream channel.** A stream channel's
  frames are not `SessionFrame`s at all: the session hands the sealed link to
  the caller (`open_stream_channel` / the incoming-streams receiver) and the
  caller runs a protocol directly over it, so there is no `message_type` field
  on the wire to allocate from. Its MessageType namespace is therefore
  **reserved in full**; a future need for typed messages alongside the bytes
  would be a new channel, not a retrofit of this one.
  What rides the link is the *transfer* framing, reused unchanged (I2): a run
  of `Chunk` frames carrying raw bytes, terminated by `Control::Complete
  { checksum }` and answered with `Control::Verify { ok }` — i.e. exactly
  [TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md)'s frames minus the ones a pipe
  has no meaning for. There is **no `Meta`**, because a pipe has no name and no
  length (that absence *is* the feature — `tar cz . | peerbeam pipe --to x`
  knows neither), and **no `ResumeAck`**, because stdin is not seekable and a
  half-consumed pipe cannot be replayed.
  The terminator is load-bearing and is not merely "the link closed": a
  receiver that treated a dropped connection as end-of-stream would exit `0`
  on a truncated file, which for `peerbeam pipe --listen > project.tgz` is
  silent corruption. So the stream ends **only** on an explicit `Complete`, and
  the SHA-256 it carries is verified against the bytes actually written; a
  link that closes first is an error, and a mismatch is an error, in both cases
  after the bytes are already out (a pipe cannot un-write stdout — the exit
  code is the report).
  Sent on a channel gated by the capability's first feature bit,
  `PIPE_FEAT_STREAM = 1 << 0`
  (`peerbeam_domain::session::PIPE_FEAT_STREAM`): a peer that does not
  advertise it is refused **before stdin is read**, with a message naming the
  peer and the reason, rather than being sent bytes it would drop.
  Acceptance is gated separately and locally, and neither gate is on the wire.
  Unlike Clipboard and Presence, the first gate is **not a setting**: an
  inbound pipe is accepted only by a process the user explicitly started as
  `peerbeam pipe --listen`, one stream and then exit. A running
  `receive`/`daemon`/`serve`, and the Flutter GUI, all advertise the capability
  and all **refuse** every pipe offered to them. The second gate is the
  familiar one — trusted peers only, not configurable — optionally narrowed to
  a single named device with `--from`. All of it is decided in one place,
  `peerbeam_transfer::may_accept_pipe`, and the reasoning for why this consent
  model differs from file transfer's approval prompt is in
  [SECURITY.md](SECURITY.md).

A capability may add MessageTypes to its own namespace at will; that is a
backward-compatible (minor) change (§6).

### Sync (`0x0104`)

`ManifestRequest = 1` / `Manifest = 2` / `FileRequest = 3` (implemented) — what
a peer has under a path, and a request for one file of it. **Bytes travel over
Transfer**, exactly as this table always said: a second bulk path would mean a
second set of resume, checksum and progress semantics to keep in step.

**A one-way pull mirror, not bidirectional continuous sync.** Two devices
editing the same file while apart is a conflict problem with no good automatic
answer, and pretending otherwise is how sync tools lose work.

Three rules make a pull safe, and each is tested:

* **Nothing is ever deleted.** A pull that removed local files because a peer no
  longer has them turns a mirror into a weapon — one misconfigured share and a
  folder empties. Removing is a decision the user makes with their own tools.
* **Newer local work is never overwritten**, even when the peer's copy differs
  in size. Silently replacing something edited here would lose it with no
  warning and no undo.
* **Symlinks are skipped, not followed.** Following one could walk out of the
  share or in a circle — and `Shares::resolve` would refuse to serve the file
  anyway, so the manifest would list entries that can never be fetched.

Entries carry size and mtime, **no checksum**: hashing a shared folder on every
manifest would read every byte of it to answer a question about what changed.
The weakness — a file edited in place, same length, same second — is stated
rather than papered over.

**Two permissions, because these are two questions.** `Permission::Browse`
answers a `ManifestRequest` (may you see what exists); `Permission::Files` is
required *as well* before any byte moves for a `FileRequest`. A device allowed
to read a folder listing has not thereby been allowed to pull every file out of
it. Containment is `Shares::resolve` — the same canonicalise-then-compare
browsing uses, so a file request cannot climb out by any route browsing already
refuses.

### Browse (`0x0108`)

`ListRequest = 1` / `ListResponse = 2` (implemented) — "what is in this
folder?" and its answer. Allocated from the reserved first-party range rather
than folded into Sync (`0x0104`) or Command (`0x0106`): Sync reconciles folders
in both directions and Command is for consented *actions*, while browsing is a
read that changes nothing.

Paths are **share-relative** — `photos/2026`, never `/home/someone/photos`. A
device's filesystem layout is not the asker's business, and answering with
absolute paths would leak a home directory's name to anyone allowed to browse.

**Two independent gates, and the default of each is closed.**
`Permission::Browse` decides *who* may look; `device.shared_directories`
decides *what there is to look at*, and it is **empty by default**. A device
that grants the permission and shares nothing still answers with nothing.
Sharing a folder is a deliberate act, never a consequence of trusting someone.

**Every reason for "nothing" is the same answer.** A peer that may not browse,
a path outside every share, a path that does not exist, and a file where a
directory was expected all receive an empty `denied` response carrying no
explanation. Distinguishing them would let an asker map a filesystem it may
never see, one refused request at a time.

Containment is `Shares::resolve`, which **canonicalises before comparing**. A
textual check against `..` is not enough: a symlink inside a share pointing at
`/` passes every string test ever written. Both escapes are covered by tests
that fail if the order is reversed.

Responses are capped at `MAX_ENTRIES` and say when they were truncated — a
directory can hold a million files, and silently answering with the first 500
reads as "that is all there is".

### Presence — `Ring = 2` (implemented)

"Make yourself findable", behind *find my device*. Carries a duration and
nothing else: the receiving device decides **how** to be noticeable — a sound, a
notification, a banner — and a sender dictating the method would be deciding
about hardware it cannot see. A CLI, having neither speaker nor tray, prints who
asked rather than pretending to have rung.

The duration is **clamped on receipt, not refused**. A peer asking for an hour is
unreasonable rather than hostile, and refusing outright leaves someone standing
next to a silent phone; `MAX_RING_SECONDS` answers the question they meant.

Carries `PRESENCE_FEAT_RING = 1 << 1`. Gated on `Permission::Presence`: a device
already allowed to see this machine's battery and network is one the user has
decided may locate it — ringing adds noise to that relationship, not knowledge.

**A refused ring is silent, and a successful one is not acknowledged.** Telling
an ungranted peer "you may not ring this device" confirms the device exists and
is listening, which is precisely what a stranger probing for hardware wants to
learn; and a sender told "it rang" could map which devices are awake. The sender
learns only that its request went out.

Deliberately independent of the presence *sharing* opt-in. That setting governs
what this device reveals about itself; ringing reveals nothing here, so someone
who shares no status at all can still find their own phone.

### Notes (`0x0105`)

`NoteBatch = 1` (implemented) — a slice of this device's note set, offered to a
peer. **Tombstones are included**: a deletion is a fact about the set exactly as
an edit is, and a batch of only live notes could never tell a peer something was
deleted — the set would simply look older and the peer would offer the note
back.

Bounded twice, by encoded bytes (`MAX_BATCH_BYTES`) and by count
(`MAX_BATCH_NOTES`), and **both are re-checked on decode**: a peer's claim about
how much it is sending is not trusted, because every note in a batch costs the
receiver a store write. A set larger than one batch is sent as several, and
`more` marks all but the last.

`reply` makes an exchange terminate. The receiver answers with its own set only
after a batch with `more: false`, and **never answers a reply** — without that,
two devices volley forever, each answering the other's answer. A sync is
therefore exactly two passes.

Conflicts are **last-writer-wins by `updated_at`, with deletion breaking a
tie**. Two devices that edited while apart cannot both be right and there is
nobody to ask; surfacing every divergence as a conflict would turn a notepad
into a merge tool. The tie goes to deletion so a note deleted on one device does
not come back because the clocks agreed to the second.

Carries `NOTES_FEAT_SYNC = 1 << 0`. Like every feature bit it asserts
comprehension, not consent: **whether notes are exchanged with a given device is
`Permission::Notes`**, checked before sending, again against the authenticated
identity after the handshake, and per inbound batch. The permission is also what
makes "your own devices" concrete — PeerBeam has no notion of owning a device,
only of trusting one, so a device receives your notes because you said it may.

Unlike chat, the inbound side is gated too. A chat message that has arrived is
in hand and refusing to persist it would lose the user's data; a note batch is
someone else's data being written into this device's store, so an ungranted peer
writes nothing here and learns nothing about what is here.

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
