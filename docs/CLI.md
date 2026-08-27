# CLI

`peerbeam` — a Rust frontend over `peerbeam-engine`, sibling to the Flutter
client. Crate: `bins/peerbeam-cli` (lib + `peerbeam` bin).

## Install

**Prebuilt binaries** (attached to each [GitHub release](https://github.com/alpha-neo-omega/PeerBeam/releases)):

```bash
# Linux
curl -LO https://github.com/alpha-neo-omega/PeerBeam/releases/latest/download/peerbeam-linux-x64
chmod +x peerbeam-linux-x64 && sudo mv peerbeam-linux-x64 /usr/local/bin/peerbeam

# macOS (arm64) — unsigned, so clear the download quarantine first
chmod +x peerbeam-macos-arm64
xattr -d com.apple.quarantine peerbeam-macos-arm64 2>/dev/null || true
sudo mv peerbeam-macos-arm64 /usr/local/bin/peerbeam
```

Windows: download `peerbeam-windows-x64.exe` (SmartScreen may warn on an
unsigned download — "More info" → "Run anyway"), put it on your `PATH`.

Shell completions ship alongside the Linux binary (`peerbeam.bash`, `_peerbeam`,
`peerbeam.fish`), or generate them anytime:
`peerbeam completions <bash|zsh|fish|powershell>`.

**From source:** `cargo build --release -p peerbeam-cli`
(→ `rust/target/release/peerbeam`).

## The seven qualities

| Quality | How |
|---|---|
| **Interactive** | `prompt::confirm`/`select` — used by `send` to pick a device / confirm; no-ops when not a TTY |
| **JSON output** | `--json` on any command → machine output (NDJSON for streams like `discover --watch`) |
| **Colored output** | ANSI via `Ctx`, auto-on for TTYs, auto-off otherwise |
| **Progress bars** | `Ctx::bar` (stderr), shown in `benchmark loopback`; suppressed when not a terminal / in `--json` |
| **Shell completion** | `peerbeam completions <bash\|zsh\|fish\|powershell>` via `clap_complete` |
| **SSH friendly** | non-TTY (pipe/SSH-without-tty), `NO_COLOR`, `TERM=dumb`, `--json`, `--quiet` all disable colour/prompts/progress automatically |
| **Tests** | parse tests, pure resolver/prompt unit tests, running the binary |

## Layout

```
src/main.rs       thin: parse → Ctx → dispatch → exit code
src/lib.rs        module surface (so tests import it)
src/cli.rs        clap derive (commands + global flags)
src/output.rs     Ctx: colour/json/tty/progress decisions + table + Bar
src/prompt.rs     confirm / select (no-op off-TTY)
src/resolve.rs    pure peer resolution (id → name → prefix)
src/engine.rs     build the engine with all discovery providers
src/exit.rs       typed CliError → stable exit codes
src/commands.rs   one fn per command + dispatch
src/trust.rs      trust list/approve/revoke/permit (pinned vs approved vs permitted)
src/rules.rs      rules list/add/remove (where a received file lands)
```

## Commands

Working now:

- `config show|get <key>|set <key> <val>|path` — reads/writes `EngineConfig`
  JSON; dotted keys (`transfer.chunk_size`).
- `doctor [--json]` — environment checks (the config file itself, config/save
  dirs writable, UDP bindable, identity, mDNS daemon, Tailscale CLI, crypto)
  with ✓/!/✗; non-zero exit if any fail. The `Config` row is the first one: a
  `config.json` that cannot be parsed fails the run and names the parse error,
  rather than being quietly replaced by the defaults every check below it would
  then answer for. A config file that does not exist yet is reported as such and
  passes — the defaults are the documented behaviour for a fresh install.
- `space list|create|rename|delete|add|remove|send` — named local sets of trusted
  devices. `space send <SPACE> <PATH…>` is **N ordinary sends**, one per member,
  each through the same permission gate a hand-typed send passes. Nothing about a
  Space reaches any peer, so **no member learns who else is in it**. A member this
  device no longer trusts is named and skipped, never silently dropped. Every
  command takes a Space by name or id. See [SPACES.md](SPACES.md).
- `trust mine <DEVICE> [--no]` and `trust my-devices` — mark which machines are
  yours, and list them. A label kept on this device: it grants nothing, widens no
  permission, and the device is never told.
- `wake set|forget|send` — start one of your own machines over the local network.
  **LAN only** — a magic packet is a broadcast and does not travel over Tailscale
  or a VPN. `wake send` reports what it sent; the protocol has no reply, so it
  never claims the device woke. Only approved devices may be woken (I6). See
  [WAKE.md](WAKE.md).
- `chat retention <PEER> [--after 30m | --off]` and `chat prune [PEER]` —
  disappearing messages, **on this device only**. There is no frame telling the
  peer to delete its copy, and PeerBeam does not imply one: the promise it can
  keep is that a message is readable here for at most the window, then deleted
  from here. Off by default and off for every existing conversation. Reading a
  conversation already hides what has aged out; `prune` is what removes the bytes
  without waiting for someone to open it. Received files are left on disk — only
  the conversation row goes.
- `check-updates [--json]` — asks whether a newer release exists, **once, because
  you ran it**. This is the only request PeerBeam makes to anything that is not a
  peer. It sends no device id, no install id, and nothing identifying; it
  downloads nothing and installs nothing; and there is no automatic check on
  launch or on a timer. Being unable to reach the feed exits **0** with
  `reachable: false` — a machine with no route out is not a machine with a
  problem, and a script running this must not fail because of it. Permitted by
  amendment A1 in [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md#amendments),
  which lists the conditions it is allowed on.
- `benchmark crypto|hash|loopback [--size N] [--chunk KiB]` — AES-256-GCM
  seal/open and SHA-256 throughput (MiB/s); `loopback` = end-to-end transfer
  over an in-process link. Live progress bar; `--chunk` tunes framing. There is
  no `benchmark quic`: that micro-benchmark measured the legacy direct-transport
  link and was retired with it, and real QUIC is covered end to end by the
  two-process tests instead ([Benchmarks](BENCHMARKS.md)).
- `discover [--timeout N] [--watch]` — scans via all providers; table or live
  NDJSON stream (Ctrl-C to stop).
- `list [--online]`, `status` — device snapshot / identity + providers.

  `status` also reports **this device's own presence** — the same reading a
  heartbeat would carry, so it shows exactly what the opt-in would reveal
  before you turn it on. Computing it is purely local; nothing is put on the
  wire:

  ```
  Status: battery 21% (charging) · 4.0 GB free · v0.4.1
  Sharing: off — device status is shared with nobody
  Peers:    no shared status (presence is live; a one-shot command holds no session)
  ```

  Battery is read on Linux (sysfs) and Android; Windows and macOS report none
  by design, and an absent reading is simply omitted rather than shown as `0%`.
  Free space is measured on the volume holding the save directory.

  The empty `Peers` line is not a failure: presence is **live** state carried
  on open sessions and nothing is persisted, so a one-shot command that holds
  no session has nothing to show. It populates for a long-running command
  (`receive`, `daemon`, `chat watch`) that keeps sessions open.

  Under `--json` the same data appears as `presence` (this device),
  `share_presence` (the opt-in, default `false`), and `peers` (an array, one
  row per peer that has shared). Additive — every pre-existing key is
  unchanged.

  Sharing is off by default, and status is only ever sent to **approved**
  devices (`peerbeam trust approve`); that second gate is not configurable. Set
  `device.share_presence` in the config to opt in.

  **Clipboard auto-sync is a GUI feature and the CLI does not gain it.**
  `send --clipboard` is unchanged and remains the CLI's manual path. This is a
  deliberate boundary, not a gap: auto-sync needs a *watcher*, watching needs
  to read the system clipboard, and there is no system-clipboard adapter in
  the Rust workspace — only an in-memory double for tests and headless
  servers. Flutter's clipboard API already works on every desktop target, so
  the watcher lives there (`docs/UI.md`) and adding a Rust one would mean a
  new dependency for a feature the desktop app already has. A headless server
  has no clipboard to sync in the first place.

  The CLI does take part on the **receiving** side, because it advertises
  `CLIPBOARD_FEAT_CLIP` exactly as the Flutter frontend does — a peer must not
  behave differently depending on which of PeerBeam's two frontends it
  reached, and advertising a bit whose frames were then dropped on the floor
  would make the advertisement a lie. A clip arriving during a long-running
  command (`receive`, `daemon`, `chat watch`) prints one line on **stderr**:

  ```
  clipboard received from pb-alice (57 bytes) — the CLI does not apply it to a system clipboard
  ```

  The sender and the size, and **never the contents**. A clip is not a chat
  message: it is whatever the user last copied, captured automatically, and
  nothing can tell a shopping list from a password — so printing it would
  write secrets into terminal scrollback, `script` captures and CI logs. It
  goes to stderr so it can never interleave into `--json` output on stdout.
- `completions <shell>`.
- `send <PATH>… [--to <device>] [--addr IP:PORT]` — send file(s) over QUIC with
  mutual authentication. `--to` resolves a peer via discovery (id / name /
  prefix, or interactive pick); `--addr` dials directly, skipping discovery
  (headless/testing). Live progress bar; whole-file SHA-256 verified.
- `config set transfer.max_send_bytes_per_sec <BYTES>` — an outbound ceiling on
  what this device sends, in bytes per second; `0`, the default, is unlimited.
  It reaches `send <FILE>`, `send <DIR>/` and a chat attachment (`chat send
  --file`), and through them `space send`, `watch` and `transfers resume`, each
  of which is one of those sends under another name.

  **Sending only.** A receiver cannot slow a sender that ignores it, so a
  download limit would be a promise this side cannot keep; the honest control is
  over what this device puts on the wire.

  **`pipe` is not metered.** It is a raw byte stream with no transfer control at
  all, so the ceiling does not reach it. Everything that goes through the file
  or folder send loops does.

  The limit is a token bucket, so the figure is an average rather than a
  cadence: a transfer that has been idle banks up to one second's worth of
  credit and spends it in a burst instead of stuttering when it resumes. It is
  applied per transfer, which here is also per command, because the CLI sends
  one file to one peer at a time — a `space send` fan-out included.
- `receive [--dir DIR] [--port N] [--once]` — serve QUIC, authenticate each
  peer, stream incoming files to `DIR` (default: config `save_directory`).
  Advertises presence via discovery so `send --to` can find it. `--once` exits
  after one transfer; `--port 0` picks an OS port (printed on start). On first
  contact from a new peer, the CLI prints the peer's pairing code (a 128-bit
  safety number); if `device.require_pairing_confirmation` is enabled, the user
  is prompted to confirm the code matches the sender's (a decline un-pins the
  peer and aborts the transfer).
- `daemon start [--foreground]` — run the receive loop until interrupted.
  (`daemon stop|status` need the IPC layer — not built yet, exit code 8.)
- `clipboard send [--to NAME | --addr IP:PORT] [TEXT]` — send text using the
  clipboard wire convention (receiving apps offer one-tap Copy). Text source
  priority: argument → piped stdin → the system clipboard. Headless-friendly:
  `echo hi | peerbeam clipboard send --addr host:49600`.
- `clipboard get` — print the newest received clipboard text raw to stdout
  (pipes cleanly, e.g. `peerbeam clipboard get | wl-copy`).
- `chat send [--to <peer>|--addr IP:PORT] (--file <path> | <text>)` — send a
  text/markdown message, or share a file, in a conversation with a peer.
  `--to` resolves a peer via discovery (id / name / prefix, or interactive
  pick); `--addr` dials directly, skipping discovery. Exactly one of `<text>`
  or `--file <path>` must be given — clap rejects both and requires one
  (`chat send` with neither fails with "the following required arguments were
  not provided: `<TEXT>`"; `--file x hello` fails with "the argument
  `--file <PATH>` cannot be used with `[TEXT]`"). Without `--file`: the
  message is durably queued locally and a bounded best-effort delivery
  attempt is made before the command returns (does not block indefinitely or
  error on failure). Queued messages are stored encrypted locally
  (per-conversation, key derived from the device identity). For `--to` sends,
  messages are retried indefinitely when a running host (the app, or
  `peerbeam daemon start` / `peerbeam chat watch`) next reaches the peer.
  Note: `--addr` sends are queued under a routing placeholder; if the initial
  delivery attempt fails, the message stays queued (visible via `chat
  history`) but is not picked up by later drain or flush-on-connect — there
  is currently no way to auto-deliver a queued `--addr` message; re-send the
  same text via `--to` once the peer is discoverable. `Sent` status means the
  message was handed to a live session (delivery attempted on the wire); it
  is not a read receipt and does not confirm the peer's user has seen it.
  With `--file <path>`: shares one file in the conversation instead of text —
  the bytes ride the same QUIC transfer path as plain `send`, while a small
  reference (name, size) rides the chat channel so the file gets a row in the
  conversation, correlated with the transfer by one shared id. A folder is
  refused up front, before any network work (`error: folders aren't supported
  in chat yet — use Send folder`; use plain `send` for a folder), as is a
  missing path. If the peer's build predates file sharing in chat (it never
  negotiated the `FileRef` feature), the send is refused rather than falling
  back to a plain, chat-invisible transfer (`error: <peer> cannot receive
  chat attachments — its build predates file sharing in chat. Send <file> as
  a plain transfer instead.`) — that stays a **hard error even now that an
  unreachable peer queues**, because an unreachable peer is merely away and
  waiting helps, while a peer that can never receive a chat attachment would
  be a promise nothing keeps.
  **Every file send stages first.** Before any network work the file is
  stream-copied into the outbox's own storage
  (`<storage.data_directory>/outbox-blobs`), and it is that copy — never the
  path — that is offered and sent, so deleting, moving or rewriting the
  original afterwards cannot change what the peer receives. Unlike the chat log
  and queued text, a staged blob is **plaintext** on disk (owner-only, `0600`),
  for the lifetime of the queue entry that owns it. Two consequences, both new:
  **disk I/O is doubled** (a 5 GiB share writes 5 GiB before a byte moves), and
  a send can be **refused for space** where earlier builds streamed it straight
  from the source. The two bounds are ordinary config
  keys — `device.max_queued_file_bytes` (default 16 GiB / `17179869184`, a
  backstop against the absurd rather than a product limit) and
  `device.min_free_bytes` (default 512 MiB / `536870912`, which staging
  refuses to eat into) — so `peerbeam config set device.min_free_bytes
  1073741824` moves the floor. A refusal names the reason, its numbers and the
  key behind the bound, copies nothing and touches no network — e.g. with the
  cap lowered to 65536: `error: cannot stage movie.mkv: 200000 bytes is over
  the 65536-byte limit for a chat attachment. Both staging bounds are
  configurable: device.max_queued_file_bytes and device.min_free_bytes.` The
  free-space refusal has the same shape and names the floor it would have
  breached. Both exit `1`.
  While the copy runs the row reads `staging` in `chat history`, a progress bar
  shows on stderr, and `--json` emits `chat_staging` lines (both throttled to
  one report per percent).
  If that conversation is **deleted while the copy is running** — by a PeerBeam
  app or a second CLI run sharing the same data directory — the send is
  abandoned rather than half-completed: nothing is queued, the staged copy is
  deleted, no offer reaches the peer, and the command exits `1` saying so (*the
  conversation with `<peer>` was deleted while `<file>` was being staged, so
  nothing was queued — send it again to retry*). An ordinary delete does **not**
  do this: a file still being staged is kept, exactly like one already queued.
  This is the residual race, and it exits cleanly rather than offering the peer
  a file that would then be dropped.
  **An unreachable peer is queued, not an error** — the row stays `pending`,
  the queue entry and the staged copy stay on disk, and the command exits `0`
  after printing how many bytes it staged and what will deliver them: *a
  running PeerBeam app sharing this data directory*, naming
  `daemon`/`receive`/`chat watch` as draining queued text and declines, **not**
  files. Take that literally: **this CLI does not deliver queued files.**
  `daemon start`, `receive` and `chat watch` drain queued **text and declines
  only** — their drain skips file entries by design, because the bytes need a
  transfer engine the chat crate does not own. A file queued here is delivered
  by a running PeerBeam **app** pointed at the same `storage.data_directory`
  (same appstore under the same identity-derived key, same `outbox-blobs`), or
  it is dropped with `chat cancel`. File queueing and text queueing are **not**
  the same behaviour on this surface. The `--addr` caveat above applies to files
  too, and harder: such a send is queued under the same routing placeholder,
  which discovery can never resolve, so neither this CLI nor a running app will
  ever pick it up — re-send via `--to` once the peer is discoverable, and `chat
  cancel` the placeholder copy. Note also that every attempt mints a new id and
  stages its own copy, so three tries at the same 5 GiB file against an offline
  peer hold 15 GiB until they are sent or cancelled.
  Independently of all that, a peer running only `peerbeam chat watch` cannot
  receive the bytes of an incoming chat file: `watch`'s accept loop discards
  every incoming transfer stream unread (it only dispatches Chat-channel
  frames). A headless receiver needs `peerbeam receive` or `peerbeam daemon
  start` running to accept an incoming chat file; `chat watch` alone will only
  show that a file was offered, not receive it.
- `chat send --reply-to <MSG-ID> [--to <peer>] <text>` — answer an earlier
  message. `<MSG-ID>` is an id from `chat history --json`, and the reference
  travels with the message, so the peer's copy answers the same one.

  **Text only.** `--reply-to` and `--file` are refused together, because the
  reference rides inside the text message and nothing offers replying *with* a
  file yet. The id is **not** checked against this device's history: an id
  naming nothing is not an error, it is sent and rendered as a reply whose
  original is no longer here — which is also what a parent that was deleted, or
  whose retention window has closed, has to look like. What is refused, before
  anything is stored or queued, is an empty id or one over 128 characters.
  Nothing is negotiated for any of it: the reference is an additive field inside
  the ordinary text message, so a message that is not a reply is byte-for-byte
  what earlier builds sent, and a peer predating replies ignores the field and
  shows a reply as an ordinary message.

  `chat history` prints a quote line above a reply — `┌ replying to in:` (or
  `out:`, naming which side sent the parent) and the first 80 characters of
  what is stored, a file row quoting its name — or `┌ replying to <id> —
  original message no longer here` when the parent has gone. Quotes are
  resolved against the rows being printed, so a message the retention window
  already hides cannot be quoted back into view, and `chat history` is where a
  reply is marked: the `chat_received` event a live `chat watch` prints carries
  the body and no reply reference. Under `--json` every message carries
  `in_reply_to` — `null` for an ordinary message, and the id it named even when
  that message is gone.
- `chat cancel <peer> <id>` — call off a file *we* are sharing: drop it from
  the queue, delete the copy the outbox made of it, and settle the row
  `failed`. `<peer>` is a device id or a discoverable name (same resolution as
  `chat history`); `<id>` is the share's message id, as printed by `chat
  history --json` or by the queued-file notice above. Only an outgoing file row
  that has not already been sent or declined can be cancelled — anything else
  (a text row, a file the peer is offering *us*, a completed share, an unknown
  id) is `not found`, **exit 3**, and rewrites nothing. It cannot stop a copy or
  a transfer running *right now* in another `chat send --file` process (there is
  no CLI-to-CLI IPC); it stops everything that outlives that process, including
  a row stranded by a Ctrl-C mid-stage. Cancelling a row whose queue entry has
  already gone still succeeds (`— nothing was still queued`), so it is safe to
  re-run. It is **one share**, named by its message id — to forget the whole
  thread, see `chat delete`.
- `chat delete <peer>` — forget a whole conversation on **this device**: every
  message in it, text rows and file rows alike. `<peer>` is a device id or a
  discoverable name (same resolution as `chat history`).

  **Not `chat cancel`, in either direction.** That calls off one file we are
  still sharing and leaves the conversation alone; this erases the conversation
  and leaves the queue alone. Cancelling does not forget a thread, and deleting
  does not stop a send.

  **Local only, and not an unsend.** Nothing goes on the wire and the other
  device keeps its own copy of every message. Every other messenger's "delete
  conversation" reaches both screens; someone who assumes that here would be
  erasing only their own record of a thread the peer still holds in full.

  A row still backing a **queued** outbound message survives, along with the
  file it owns. The drain reads a missing record as "nothing will ever settle
  this" and releases the staged bytes with it, so dropping the row would make
  the file vanish without ever being sent. The receipt names how many were kept
  for that reason; `chat cancel` is what actually lets those go.

  That rule bites hardest on a thread with an **offline** peer, and it is not a
  bug: `chat send` to an unreachable device enqueues, so a conversation whose
  messages have not gone out yet deletes *nothing* and reports every row as
  kept (`nothing deleted … all N message(s) are still waiting to send`). Let
  them drain — or cancel them — and the delete then takes the thread.

  **It asks first, and silence is not consent.** The delete cannot be undone and
  nothing else on the device holds a second copy, so a non-interactive run
  (`--json`, no TTY, redirected stdin) with no `-y/--yes` is refused — **exit
  2**, naming the flag that would have worked — rather than proceeding the way
  `trust approve` does under `--json`. That command *grants* standing and can
  safely assume a machine that cannot answer; this one destroys history.
  Answering "no" at the prompt is **exit 6**. Deleting a conversation that is
  not there removes nothing and exits `0`, so a re-run converges instead of
  failing the second time.

  The engine call is the same one the desktop app's "Delete conversation"
  reaches (`ChatStore::delete_conversation` via `pb_chat_delete`), not a second
  implementation of it, so the keep rule cannot drift between the two surfaces.
  Under `--json` it emits one `chat_deleted` event carrying
  `{"peer","removed","kept"}`.
- `chat history <peer>` — print a conversation's stored history. Accepts a device
  id, or a name resolved via discovery. Messages are encrypted at rest. A file
  share's row shows its name, size, and status instead of message text.
- `logs [--limit N] [--export PATH]` — recent engine log lines, read from the
  **log file** rather than this process's memory: a one-shot command has its own
  empty buffer, so answering from it would print nothing while looking like it
  worked. Needs `log.to_file` (on by default).
- `pair <peer> [--pin 123456 | --show]` — PIN-pair with a device. One side runs
  `--show` and reads six digits aloud; the other types them. **The PIN is never
  sent** — only a proof over this handshake's transcript, which is worthless to
  anyone relaying between two connections. Three wrong guesses ends the pairing;
  a fresh PIN is needed rather than another try.

  Turn `encryption.require_pin_pairing` on to require this before any new device
  can be approved. It is **off by default**: on, nothing pairs without a person
  at both ends.
- `snippet --to <peer> [--title T]` — send piped text as a message:
  `cargo test 2>&1 | peerbeam snippet --to laptop`. Oversized input is
  truncated rather than refused — a long log piped by accident should not fail
  after the command that produced it has finished, and the message says it was
  cut.
- `send … --at <when>` — wait until `HH:MM` (next occurrence) or
  `YYYY-MM-DDTHH:MM:SS`, then send. **A delay, not a scheduler**: the process
  must stay running, so use cron or a systemd timer for anything that has to
  survive a reboot.
- `sync <peer> <path> <into>` — sync a folder with a device, **both ways**.
  Files only they changed are fetched, files only you changed are pushed, and
  their deletions are applied when they descend from your copy. When **both**
  changed a file, their copy arrives as `name.sync-conflict-<peer>.ext` and
  yours is left untouched — each conflict is named, because it is a decision you
  now have to make. Needs both `browse` and `files` permissions from the peer,
  and incoming files arrive as ordinary transfers, so `peerbeam receive` or the
  daemon must be running to accept them.

  Only the **changed parts** of a file cross the wire, reused from wherever you
  already hold them. A file that was merely **moved or renamed** is moved
  locally rather than fetched again. Add `--watch <seconds>` to keep syncing;
  a file is acted on only once it has stopped changing, so saving a large file
  mid-poll never syncs a half-written copy.
- `browse <peer> [path]` — list what a device shares, read-only. Paths are
  share-relative (`photos/2026`). An empty listing is the same answer whether
  the device shares nothing, has not granted this machine the `browse`
  permission, or the path does not exist — it deliberately does not say which,
  because a caller able to tell would be able to map a filesystem it may not
  see. Share folders with `device.shared_directories` (empty by default) and
  grant access with `peerbeam trust permit <device> browse`.
- `watch <dir> --to <peer> [--interval N] [--existing]` — send whatever lands in
  a folder. Polls rather than using a filesystem watcher, so it behaves the same
  on every OS and works on the network shares people actually drop files onto.
  **A file is sent only once it has stopped growing**, so a copy still in
  progress is never delivered half-written. Files already present when the watch
  starts are left alone unless `--existing` is given.
- `timeline [--limit N]` — one chronological view of this device's activity:
  transfers, conversations, and clips when clipboard history is on. Carries no
  message bodies and no clip text.
- `clipboard history [--clear]` — show or erase what this device remembers
  copying. Empty unless `device.clipboard_history` is on (default off, and
  separate from clipboard sync). The listing abbreviates each clip to one
  capped line — it prints into terminal scrollback, and dumping fifty
  remembered clips there would defeat the point of bounding the log.
- `ring <peer> [--seconds N]` — *find my device*: ask one of your devices to
  make itself findable. It rings only if it has granted this machine the
  `presence` permission, and never reports back either way.
- `notes list` — every note, newest edit first. Deleted notes are not shown.
- `notes add [BODY] [--title T]` — write a note. With no `BODY` the text is read
  from stdin, so `pbpaste | peerbeam notes add` and `notes add < draft.md` work
  over SSH.
- `notes edit <id> [BODY] [--title T]` — replace a note's text. Refuses a
  deleted note rather than resurrecting it.
- `notes remove <id>` — delete a note, leaving a tombstone so the deletion can
  reach other devices.
- `notes sync <peer>` — exchange notes with a device you have granted the
  `notes` permission (`peerbeam trust permit <device> notes`). Sends this
  device's whole set and merges what comes back. Notes also sync automatically
  whenever a permitted device connects.
- `chat history <peer> [--mark-read]` — `--mark-read` also tells the peer you
  have read it. Opt-in on top of an opt-in: printing a conversation is never
  consent to report having read it, and nothing is sent unless
  `device.share_read_receipts` is on.
- `chat react <peer> <id> <emoji> [--remove]` — react to a message, or withdraw
  a reaction. Applies to this device's history whether or not the peer can be
  reached, and reports both answers: `--json` returns `applied` and `delivered`,
  and the human line says "not delivered (peer offline, or too old for
  reactions)" rather than implying it was seen. Reactions are not queued.
- `chat search <query> [--limit N]` — find messages in this device's **own**
  stored conversations. A local read: no peer is resolved, no discovery window
  is opened, nothing is dialled, so it works on a headless box with no network
  at all and a thread whose device is long gone is searchable exactly like one
  that is online. A conversation that was deleted is not searchable — its rows
  are gone.

  Matches a **case-insensitive substring** of a message's text or a shared
  file's *name*. It is not a regular expression, and a file's `local_path` is
  never searched: that is where the file happens to sit on this disk, not
  anything anyone said, so matching it would surface a thread for the name of a
  folder. Case-insensitivity is Unicode lowercase *mapping*, not full case
  folding — `ПРИВЕТ` finds `привет`, but `ß` does not match `ss`.

  Results are newest first (ties broken by peer id then message id, so the
  order is stable) and bounded by `--limit` (default 50, max 500). **When there
  were more matches than fitted, the command says so** on its own line — a
  bounded search whose bound is invisible reads as "that is all there is".
  Finding nothing exits `0`: an empty result set is a successful search, not a
  failed lookup.
- `chat watch [--port N]` — listen for and print incoming chat messages in
  real-time. Must be running to receive messages. `--port` specifies the QUIC
  port to listen on (default: from config `transfer.port`). A file share
  still appears here (with a note that `watch` cannot receive its bytes — see
  `chat send --file` above); run `peerbeam receive`/`daemon start` instead to
  actually accept the file.
- `pipe --to <device> | --addr IP:PORT` / `pipe --listen [--from <device>]
  [--port N]` — an **encrypted byte pipe** between two devices: stdin on one
  side, stdout on the other.

  ```bash
  tar cz ./project | peerbeam pipe --to laptop     # on the sending machine
  peerbeam pipe --listen > project.tgz             # on the receiving one
  ```

  Exactly one direction is required (`--to`, `--addr` or `--listen`); giving
  none, or two, is a usage error (exit `2`). `--to` resolves a peer through
  discovery, `--addr` dials directly.

  **Binary-safe and unbounded.** The stream has no length and no filename —
  that absence is the point — and nothing inspects, decodes or line-buffers
  the bytes, so `tar`, `gzip`, `dd` and arbitrary binary survive intact.
  Neither end ever holds more than one chunk, so a 40 GB pipe runs at flat
  memory. **EOF is the terminator**: the sender closing stdin ends the
  stream, and the receiver flushes stdout and exits `0`.

  **`stdout` carries piped bytes and nothing else.** Every human-facing line
  — the listening address, the peer's name, refusals, and `--json` events —
  goes to **stderr**, in *both* directions. That is what makes
  `peerbeam pipe --listen > project.tgz` produce a correct archive, and it
  means a script reads pipe events from stderr and the payload from stdout:

  ```bash
  peerbeam --json pipe --listen --port 0 > out.bin 2> events.ndjson
  ```

  emits `{"event":"pipe_listening","addr","port","from"}` on start and
  `{"event":"piped","direction","bytes","chunks","peer"}` at the end, both on
  stderr. There is deliberately **no progress bar**: a pipe has no total to
  measure against.

  **The consent model is not file transfer's, and the difference is the
  point.** Two gates, neither optional:

  1. **Only a `peerbeam pipe --listen` accepts a pipe.** A running `receive`,
     `daemon start` or `chat watch` refuses every one, as does the PeerBeam
     desktop app — all of them advertise the capability and none of them
     accepts. **Running the command is the approval**, which is why there is
     no prompt and must not be one: a prompt reads stdin, stdin is the
     payload on the sending side, and a prompt would break the scripted,
     headless use this exists for.
  2. **Approved devices only**, not configurable, narrowed to a single device
     with `--from <device>` (a `pb-…` id, or a name resolved through
     discovery — the match is always against the authenticated id, never
     against the name a peer presents). *Approved*, not merely pinned: a peer
     is pinned by the handshake as it connects, so a listener that accepted
     every pinned device would accept every first-contact stranger. Run
     `peerbeam trust approve <device>` on the listening machine first. See
     [Security](SECURITY.md) for the reasoning and the limits.

  **One stream, then exit.** A listener takes one stream and stops; there is
  no `--keep-open`. A *refused* attempt is not that stream — the listener
  keeps waiting, so a stranger cannot end it with a single dial.

  Exit codes: `0` piped; `2` no direction or two; `3` peer/route not found;
  `4` the stream failed or was refused (the message names `pipe --listen`, the
  only thing that accepts one, since a refusal and an unreachable peer look
  identical from the sending side); `5` the receiver's bytes did not match the
  sender's checksum — **the bytes are already on stdout by then**, so this
  exit code is the only report a truncated or corrupt stream gets, and a
  script that ignores it will trust a bad file; `8` the peer's build predates
  `peerbeam pipe` (nothing is read from stdin in that case).
- `history [--limit N] [--clear]` — persisted transfer history (sends and
  receives, success or failure), `<data_dir>/history.json`, same schema as the
  app engine's history, bounded to the 500 most recent.
- `trust list` / `trust approve <device> [--for DURATION] [--no-share]` /
  `trust auto-accept <device> [--no]` / `trust revoke <device>` /
  `trust permit <device> <permission>…` /
  `trust revoke-permission <device> <permission>…` — the devices this machine
  trusts, **which of them the user actually chose**, **for how long**,
  **what each may do**, and **which of them stop asking**.

  ```
  STATUS    DEVICE           NAME          FINGERPRINT          PINNED            EXPIRES   PERMISSIONS
  pinned    pb-91ab33cd1122  Unknown Peer  77b2ccddeeff0011…    2026-08-18 02:11  never     none
  approved  pb-f4e4d56fce98  laptop        3f9a1b2c4d5e6f70…    2026-08-17 10:30  never     files,chat,presence,pipe
  approved  pb-0d19aa73be40  Loaner        b1c2d3e4f5a60718…    2026-08-19 09:02  in 24m    files,chat,clipboard,presence,pipe
  expired   pb-77c410ee9a35  Guest         5e6f7081a2b3c4d5…    2026-08-19 08:15  12m ago   none
  ```

  Two states, and the difference is the point. A device is **pinned** by the
  authenticated handshake the first time it connects — that records its key so
  a later change is detectable as a possible MITM, and nothing more, so every
  stranger that has ever reached this machine is pinned. A device is
  **approved** only when a person says so, and approval is what lets it receive
  this machine's **presence status, clipboard, and a `pipe --listen`**. Those
  three gates are not configurable and ask for approval, not for a pin; see
  [Security](SECURITY.md#pinned-is-not-approved).

  Until this command existed, approving was reachable only from the desktop
  app's accept-and-trust prompt, so a headless server or a container could not
  use any of the three at all. Approving needs no daemon, no network and no
  peer online — it edits this machine's own store:

  ```bash
  peerbeam trust list --json | jq -r 'select(.approved | not) | .id'
  peerbeam trust approve pb-f4e4d56fce98 --yes    # scripted: no prompt
  ```

  **What `approve` grants is a fixed five, not everything.** It writes `files`,
  `chat`, `clipboard`, `presence` and `pipe` — the set that existed when
  permissions were introduced, frozen so that a later release cannot widen what
  an unreviewed device may do. `notes` and `browse` were added afterwards and
  stay opt-in, which is why an approved device still cannot list this machine's
  shared folders until you say so:

  ```bash
  peerbeam trust permit pb-f4e4d56fce98 browse
  ```

  **`--no-share` approves and grants nothing.** The key is vouched for — the
  device stops counting as a stranger and stops re-prompting as first contact —
  and every capability is left to be granted one at a time, which is what
  invariant I6 asks for:

  ```bash
  peerbeam trust approve pb-f4e4d56fce98 --no-share --yes
  peerbeam trust permit pb-f4e4d56fce98 chat   # …and only chat
  ```

  It is ignored for a device that is **already** approved: the permission set is
  written only on the transition, so this can never be used to strip what a
  working device has been using.

  **`trust auto-accept <device>` stops the approval prompt for one device.**
  The global `device.auto_accept_trusted` setting is all-or-nothing, so
  silencing the phone you sync with hourly used to mean silencing every approved
  device as well:

  ```bash
  peerbeam trust auto-accept my-phone        # its files arrive without asking
  peerbeam trust auto-accept my-phone --no   # ask again
  ```

  It is a **prompt** setting, not a permission. The admission gate consults it
  only after `files` has already admitted the transfer, so it can never accept
  what would otherwise be refused, and setting it on a device that is
  unapproved, expired, or narrowed to withhold `files` does nothing at all —
  the command says so rather than reporting a setting that is not in force.

  `<device>` resolves exactly as `send --to` does — exact id, exact name, then
  unique name prefix — and an ambiguous prefix is an error listing the
  candidates (exit `2`), never a guess. A device that matches nothing is exit
  `3`.

  **`approve` shows the fingerprint it is approving and asks.** On a terminal
  the prompt carries the full 64-character fingerprint and says what approval
  grants, so it cannot be answered blind; `--yes` (or `--json`, which is
  non-interactive by definition) proceeds without prompting, which is what
  makes it scriptable. Without either, and with no terminal to ask at, it
  refuses (exit `6`) rather than approving unasked. Approving an
  already-approved device is a no-op that says so and exits `0`, so a
  provisioning script is safe to re-run.

  **`--for DURATION` approves for a while.** `45s`, `30m`, `2h`, `7d` — one
  number and one unit; a bare `30` is exit `2` rather than a guess between half
  a minute and half an hour, and so is `0m` (the error names `--for`'s omission
  and `trust revoke` as the two things that were probably meant). The deadline is
  stored as an **absolute instant**, so a machine asleep through the whole window
  wakes with it shut.

  ```bash
  peerbeam trust approve guest-laptop --for 30m --yes
  peerbeam trust list                      # EXPIRES: in 29m
  ```

  When the window closes the device is back to being merely **pinned**: it may
  nothing, `is_trusted`/`is_approved`/`may` all answer false, and `list` shows it
  as `expired` with when it lapsed. **Nothing has to run for that to happen** —
  no daemon, no sweeper, no reconnect. Every gate re-reads the store per
  operation, so the verdict is recomputed from the clock each time somebody asks,
  and a fresh `peerbeam` process reaches the same one.

  The **pin survives**: its key is still remembered, so a key change is still
  caught. `revoke` is what forgets a device. Approving an expired device renews
  it and gives back the permissions it was actually left, not the five it started
  with; a plain `approve` (no `--for`) means indefinitely, and lifts a window set
  earlier. Re-running `--for` on a device that still holds standing just rewrites
  the window and asks nothing — it is not a new grant. With
  `encryption.require_pin_pairing` on, renewing an *expired* device goes through
  `peerbeam pair`, exactly as a first approval does.

  `revoke` removes the **whole record**, not just the approval, so the next
  connection is a fresh first contact: re-pinned, and unapproved until someone
  says otherwise. That is what the app's Trusted Devices revoke does too.
  Revoking a device that is not pinned is exit `3`. There is deliberately no
  confirmation on `revoke` — it only ever removes standing.

  **`permit` and `revoke-permission` narrow what an approved device may do**,
  without un-approving it. The permissions are `files`, `chat`, `clipboard`,
  `presence` and `pipe`; approving grants all of them, which is exactly what
  approval always meant. This is how "this laptop may sync files but must never
  read my clipboard" is expressed:

  ```bash
  peerbeam trust revoke-permission laptop clipboard presence
  peerbeam trust permit laptop clipboard          # and back again
  peerbeam trust list --json | jq -r 'select(.permissions | index("clipboard") | not) | .id'
  ```

  A change applies to that device's **next operation, not its next connection** —
  every gate re-reads the store per message, clip, heartbeat and accept. Several
  permissions may be given in one invocation, and **every name is parsed before
  anything is written**, so a typo applies nothing (exit `2`, listing the valid
  names) rather than leaving a half-applied change. Both directions are
  idempotent and exit `0` when nothing changed, so a provisioning script is safe
  to re-run, and neither prompts: revoking only removes standing, and permitting
  can only restore what `approve` already granted once. Permitting a
  pinned-but-*unapproved* device is exit `2` naming the next step — permissions
  narrow a standing and never create one, so there would be nothing to narrow.

  A `trust.json` written before permissions existed keeps all five for every
  device it had approved; a permission added in a later release is denied by
  default until explicitly permitted. See
  [Security](SECURITY.md#the-upgrade-rule).

  Under `--json`, `list` emits one object per line
  (`{"id","name","fingerprint","trusted_at","approved","expires_at","expired","permissions"}`,
  `approved` an explicit bool, `permissions` an explicit array — empty included —
  and the fingerprint in full). `approved` is the **effective** answer, so
  `select(.approved | not)` catches a device whose window has closed instead of
  skipping it; `expired` tells that device apart from a stranger nobody ever
  approved, and `expires_at` is the absolute instant or `null`.

  `approve` and `revoke` emit one `trust_approved` / `trust_revoked` event; `permit` and `revoke-permission`
  emit one `trust_permissions_changed` carrying `granted`, `requested`,
  `changed` (what actually moved) and the resulting `permissions`.
- `rules list` / `rules add <DIRECTORY> [criteria]` / `rules remove <INDEX>` —
  **where** a received file is saved.

  ```
  #  DEVICE           EXT   SIZE       DESTINATION
  0  pb-f4e4d56fce98  any   ≥ 1.1 GB   /mnt/big
  1  any              pdf   any        /srv/papers
  2  any              any   any        /srv/inbox
  ```

  **A rule decides where a file is saved. A rule never decides whether it is
  accepted.** Rules are read *after* a transfer has been accepted and is on its
  way to disk; they cannot approve anything, they have no field that influences
  approval, and the separate `device.auto_accept_trusted` setting is untouched
  by all of this. If you want to change what gets accepted, that is the
  approval prompt and `auto_accept_trusted`, not this command.

  A rule is a **match** plus a **destination**. Every criterion is optional and
  an omitted one matches everything, so a rule with none is a catch-all:

  | flag | matches |
  |------|---------|
  | `--from <device>` | files from that device, by its **authenticated id** |
  | `--ext <EXT>` | that file extension, case-insensitively (`pdf` or `.pdf`) |
  | `--min-bytes N` | files of at least N bytes (inclusive) |
  | `--max-bytes N` | files of at most N bytes (inclusive) |

  **The first rule that matches wins**, and the `#` column is that order. There
  is no specificity score: a list you can reorder is a list whose outcome you
  can predict. `--at <INDEX>` inserts rather than appends, which is how you
  change the tie-break without rewriting the list. A file that matches no rule
  goes to `storage.save_directory`, exactly as every file did before rules
  existed — an empty list changes nothing.

  ```bash
  peerbeam rules add /srv/papers --ext pdf
  peerbeam rules add /mnt/big --from laptop --min-bytes 1073741824 --at 0
  peerbeam rules list --json | jq -r '"\(.index)\t\(.directory)"'
  peerbeam rules remove 1
  ```

  `--from` resolves exactly as `send --to` and `trust approve <device>` do —
  exact id, exact name, then unique name prefix — and what is **stored** is the
  resolved `pb-…` id, never the name that was typed. A name is peer-supplied
  and any peer may present any name it likes, so a rule matching on one would
  hand a stranger calling itself "laptop" the laptop's destination. An
  ambiguous prefix is exit `2` listing the candidates; an unknown *name* is
  exit `3`; an unknown but well-formed `pb-…` id is accepted verbatim, so a
  rule can be provisioned before that machine first connects.

  **A rule is validated when it is added**, not when a file arrives: the
  destination must be absolute, must contain no `..` component, and its parent
  must already exist (exit `2`, with the reason). Requiring the parent rather
  than the leaf is deliberate — creating one missing directory on first use is
  a convenience, while creating a missing *tree* would cheerfully manufacture
  `/mnt/nas/videos` on the local root the day the NAS is not mounted.

  **A destination that fails anyway does not lose the file.** If the chosen
  directory cannot be written to when a file arrives — it vanished, the mount
  went away, permissions changed — the file goes to `storage.save_directory`
  and the receiver says so:

  ```
  rule destination /mnt/big is unusable (Not a directory (os error 20)); saving to /srv/incoming instead
  ```

  Under `--json` that is a `rule_fallback` event on the same stream as every
  other thing that goes wrong with a receive. A file quietly landing somewhere
  other than where the rules claimed is worse than having no rules.

  **`receive --dir DIR` turns rules off for that run.** A directory typed on
  the command line is the more specific instruction of the two, so it wins
  outright — which also makes it the way to say "ignore my rules just this
  once" without editing them. `daemon start` always uses the rules.

  Rules live in this machine's config under `storage.rules`, so
  `peerbeam config show` prints them and the daemon reads them at startup. The
  desktop app keeps its own list in its own settings document, exactly as it
  keeps its own save directory and device name.

  Under `--json`, `list` emits one object per line
  (`{"index","device","extension","min_bytes","max_bytes","directory"}`, with
  an unset criterion as `null` rather than `""` or `0` — `0` is a legitimate
  `min_bytes`); `add` and `remove` emit one `rule_added` / `rule_removed`
  event carrying the same fields.

- `transfers [list|resume <ID>|discard <ID>]` — what is moving, and what
  stopped moving.

  A transfer that ends because the link dropped or the process died leaves a
  **checkpoint** in `<data_directory>/checkpoints`: the peer, the file, its size
  and how far it got. That directory belongs to the *machine*, not to a
  frontend, so `transfers list` on a headless box shows what the desktop app and
  the daemon left behind, and `resume` can pick one up over SSH.

  ```text
    ID          DIR   PEER              FILE          PROGRESS         AGE
    tx-4131-0   out   pb-f4e4d56fce98   movie.mkv     1.2 GB / 4.0 GB  2h
    fileref-7   in    pb-9a10c2b40f21   photos.zip    88 MB / 210 MB   1d
  ```

  `peerbeam transfers` with no subcommand keeps printing the live
  session/transport snapshot it always did and adds an `interrupted` array
  beside it — additive, so a script reading `sessions`/`transport` is untouched.

  **`out` can be resumed; `in` resumes itself.** The transfer protocol is
  sender-driven, and resume is that protocol's own mechanism rather than a new
  message, so this side cannot ask a peer to start sending again. An incoming
  checkpoint keeps its partial file, its progress and its consent, and continues
  the moment its sender offers it again; `resume` on one exits `2` and says so.

  `resume <ID>` re-dials the peer — using **discovery**, never an address from
  the checkpoint, because where a device can be reached is exactly what changes
  while a transfer sits interrupted — and continues from the bytes the receiver
  already has. Before anything moves it checks that the checkpoint still binds
  to its transfer: direction, the persisted consent flag, the peer id, the file
  name, and the total size *as the source file is on disk now*. A source that
  has been replaced or resized exits `2`: appending its bytes to a receiver's
  prefix of the old contents would build a file that never existed anywhere.
  The end-of-transfer checksum still runs, so a resumed file that does not
  verify fails (exit `5`) rather than landing.

  `discard <ID>` drops the record and, for an incoming transfer, the `.part`
  file it was holding — leaving that behind would let a transfer the user threw
  away seed the next one of the same name. It never touches the **source** of a
  send: giving up on a send is not permission to delete your file.

  Checkpoints are also swept by age. The desktop engine reclaims anything older
  than 14 days (and its partial file) at startup; a resume refreshes the clock,
  so a transfer you keep retrying never ages out.

  **`peerbeam send` writes no checkpoints of its own**, deliberately. It is a
  foreground command, and re-running it already resumes — the receiver's partial
  file is what negotiates the offset, not a record on this side.

  Under `--json`, `list` emits one object per line
  (`{"id","direction","peer","file","path","transferred_bytes","total_bytes","started_at","resumable"}`);
  `discard` emits one `discarded` event with `partial_removed`.

- `session list|show <ID>|stats|watch`, `channels [--session <ID>] [--watch]`,
  `migration`, `recovery`, `diagnostics` — the PeerSession diagnostics view,
  printed as JSON (indented on a terminal, one line under `--json`).

  `session list` gives
  `{"count","sessions":[{"id","peer","state","version","capabilities"}]}`;
  `session show` one `{"session"}` by its 32-hex id, `null` (and exit `0`) for
  an id that does not parse or is not open; `session stats` the session count
  beside the transport summary; `channels` a live per-channel snapshot with
  frame and byte counters, for one session or all of them; `recovery` the
  sessions currently reconnecting; and `diagnostics` sessions, transport and
  recovery in one object. `migration` is the transport summary
  (`{"transport":"peersession","active_sessions","recovering"}`) under the name
  the command and its FFI symbol have always had, kept so anything scripted
  against either keeps working.

  **A one-shot command holds no sessions, and cannot see another process's.**
  Each invocation builds a fresh diagnostics view over the registry a live
  `PeerSession` registers into, and there is no IPC to a running receiver — the
  same gap `daemon stop|status` is gated on. So from a bare shell these print
  well-formed *empty* snapshots, which is also why `session watch` and
  `channels --watch` print one snapshot rather than streaming: there is nothing
  to attach to. They are here because the app reads exactly this view
  in-process over the FFI (`pb_sessions_json`, `pb_diagnostics_json`, …), and a
  capability that exists only in the GUI is the split I7 forbids.

Transfers are end-to-end encrypted: QUIC (TLS 1.3) for the pipe, plus an
application-layer X25519 mutual-auth handshake with TOFU trust pinning and
per-frame replay protection ([Security](SECURITY.md)).

Still gated (exit code 8): `daemon stop|status`.

## Global flags

`--json` · `-v/-vv` · `-q/--quiet` · `--no-color` · `-y/--yes` · `--config <path>`.

## Exit codes

`0` ok · `2` usage · `3` not-found · `4` connection · `5` integrity ·
`6` cancelled · `7` daemon-unavailable · `8` unavailable · `1` other.

## Examples

```bash
# Human use
peerbeam doctor
peerbeam discover --timeout 5
peerbeam list --online
peerbeam config set device.name "My Laptop"

# Transfer: receive on one machine, send from another
peerbeam receive                          # serve + advertise (Ctrl-C to stop)
peerbeam send movie.mkv --to "My Laptop"  # discover peer by name and send
peerbeam send movie.mkv --addr 192.168.1.5:49600   # or dial directly

# Scripting (machine-readable, no colour/prompts, branch on exit code)
peerbeam discover --timeout 3 --json | jq '.[].name'
name=$(peerbeam config get device.name)
if ! peerbeam config get transfer.chunk_size >/dev/null; then
  echo "key missing"        # exit code 3
fi

# Live stream of discovery changes (NDJSON, Ctrl-C to stop)
peerbeam discover --watch --json

# Encrypted pipe: a directory across the network without a temp file
peerbeam pipe --listen > project.tgz          # on the receiving machine
tar cz ./project | peerbeam pipe --to laptop  # on the sending one

# ...or straight into a command, since stdout is only ever the bytes
peerbeam pipe --listen --from pb-a1b2c3 | tar xz
# Check the exit code: the bytes are already out when a truncated or corrupt
# stream is detected, so this is the only thing that says the file is sound.
peerbeam pipe --listen > backup.img || echo "incomplete — do not use it"

# Approve a device on a headless box — required before it can be sent this
# machine's presence status or clipboard, or pipe into a `pipe --listen`.
peerbeam trust list                                # who is approved, and what each may do
peerbeam trust approve pb-f4e4d56fce98 --yes       # scripted: no prompt
peerbeam trust revoke-permission laptop clipboard  # keep it, take one power away
peerbeam trust revoke laptop                       # forget it entirely

# Sort what arrives, without changing what is accepted. First match wins, so
# the order is the tie-break — and a file matching nothing goes where it
# always did.
peerbeam rules add /srv/papers --ext pdf
peerbeam rules add /mnt/big --from laptop --min-bytes 1073741824 --at 0
peerbeam rules list
peerbeam rules remove 1

# Shell completion (bash; also zsh/fish/powershell)
peerbeam completions bash > /etc/bash_completion.d/peerbeam
```

Over SSH without a TTY, or into a pipe, colour/progress/prompts disable
automatically — no flags needed. Force non-interactive with `-y`, plain output
with `--no-color` or `--json`.

### JSON output (scripting)

With `--json`, human text and progress bars are suppressed and each command
emits machine-readable JSON (NDJSON for streaming/long-running commands):

- `send --json` → one object per file: `{"event":"sent","file","bytes","peer","newly_trusted"}`.
- `receive --json` / `daemon` → a `{"event":"listening","addr","port","dir"}`
  line on start, then `{"event":"received","file","bytes","peer","newly_trusted"}`
  per transfer (or `{"event":"error","message"}`).
- `status --json` → `{"device_name","platform","transfer_port","save_directory","data_directory","providers":[…],"listening":bool}`.
- `discover --json` → array (or NDJSON with `--watch`) of devices.
- `chat send --json` → `{"event":"chat_sent","id","peer","delivered":bool}` for
  text, `{"event":"chat_file_sent","id","peer","delivered":bool}` for `--file`.
  **`delivered:false` means queued, not failed** — the command still exits `0`
  — and on the file event that value is newly reachable (before offline
  queueing it was always `true`). A `--file` send also emits
  `{"event":"chat_staging","id","peer","done","total"}` while the file is being
  copied into the outbox, one line per percent.
- `chat cancel --json` → `{"event":"chat_cancelled","id","peer","cancelled":true,"dequeued":bool}`;
  `dequeued` distinguishes "stopped a queued delivery" from "settled a row whose
  queue entry had already gone".
- `chat watch --json` / chat traffic seen by `receive`/`daemon` →
  `{"event":"chat_received","id","peer","body","timestamp","kind"[,"file"]}` per
  message, and — `chat watch` only — `{"event":"chat_file_needs_receiver","id","peer","name","size"}`
  when a file is offered to a process that cannot accept its bytes.
- `chat history --json` is not a stream: one `{"messages":[…]}` object, each
  message carrying `status` (`staging`/`pending`/`sent`/…), `in_reply_to` (the
  id this message answers, `null` when it answers nothing, and still the id
  when that message has gone) and, for a file, `kind:"file"` plus
  `{"name","size","local_path"}`.
- `chat search --json` is not a stream either: one
  `{"hits":[{"peer_id","message_id","timestamp","direction","kind","snippet"}],"truncated":bool,"limit":N}`
  object. Deliberately one object rather than a line per hit — `truncated` has
  to sit somewhere a consumer cannot miss it, and a stream of hits with a
  marker at the end is exactly the shape a script reading the first N lines
  drops on the floor. `snippet` is a substring of what is stored (the body
  around the match, or the file's name), unmodified.
- `trust list --json` → one object per line:
  `{"id","name","fingerprint","trusted_at","approved","permissions"}`.
  **`approved` is a bool**, and it is the field to filter on — the presence of a
  row means only that the device's key was pinned when it connected.
  **`permissions` is an array of names**, emitted even when empty, so a script
  can test what a device may do rather than infer it.
  `trust approve --json` → one `{"event":"trust_approved",…,"changed":bool}`
  (`changed:false` when it was already approved, still exit `0`);
  `trust revoke --json` → one `{"event":"trust_revoked",…,"removed":true}`;
  `trust permit --json` / `trust revoke-permission --json` → one
  `{"event":"trust_permissions_changed","granted":bool,"requested":[…],"changed":[…],"permissions":[…]}`.

Branch on the exit code for success/failure; parse the JSON for details.

```bash
# One-shot receive; print each received file name as it lands
peerbeam --json receive --once --dir ./in | while read -r ev; do
  echo "$ev" | jq -r 'select(.event=="received") | .file'
done

# Is a receiver already up on this host?
peerbeam --json status | jq -e '.listening' >/dev/null && echo "listening"
```

## Verification

`cargo clippy -D warnings` clean; `cargo test` green (parse + resolver +
prompt + config round-trip + `trust` against a throwaway store,
`tests/trust_cli.rs`); **end-to-end tests spawn two `peerbeam` processes** and
transfer a file over QUIC (`tests/transfer_e2e.rs`) or a byte stream
(`tests/pipe_e2e.rs`) — the latter walking the real approval path: first
contact pins the sender and is refused, `peerbeam trust approve` grants it, and
only then does the pipe succeed. Binary smoke-tested incl. `send`/`receive`
over both discovery and `--addr`.

## Not yet

The `daemon stop|status` IPC is still gated (exit code 8).

## Engine daemon vs CLI

The CLI `daemon` command runs a foreground receive loop. The embeddable engine
also exposes daemon control over FFI (`pb_daemon_start/stop/restart/status`) for
the Flutter app — see [FFI](FFI.md).
