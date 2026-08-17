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
src/trust.rs      trust list/approve/revoke (pinned vs approved)
```

## Commands

Working now:

- `config show|get <key>|set <key> <val>|path` — reads/writes `EngineConfig`
  JSON; dotted keys (`transfer.chunk_size`).
- `doctor [--json]` — environment checks (config/save dirs writable, UDP
  bindable, mDNS daemon, Tailscale CLI, crypto) with ✓/!/✗; non-zero exit if
  any fail.
- `benchmark crypto|hash|loopback|quic [--size N] [--chunk KiB]` — AES-256-GCM
  seal/open and SHA-256 throughput (MiB/s); `loopback` = end-to-end transfer
  over an in-process link; `quic` = end-to-end over a **real QUIC connection**
  (loopback) reporting throughput + connect latency. Live progress bar;
  `--chunk` tunes framing.
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
  re-run.
- `chat history <peer>` — print a conversation's stored history. Accepts a device
  id, or a name resolved via discovery. Messages are encrypted at rest. A file
  share's row shows its name, size, and status instead of message text.
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
- `trust list` / `trust approve <device>` / `trust revoke <device>` — the
  devices this machine trusts, and **which of them the user actually chose**.

  ```
  STATUS    DEVICE           NAME          FINGERPRINT          PINNED
  pinned    pb-91ab33cd1122  Unknown Peer  77b2ccddeeff0011…    2026-08-18 02:11
  approved  pb-f4e4d56fce98  laptop        3f9a1b2c4d5e6f70…    2026-08-17 10:30
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

  `revoke` removes the **whole record**, not just the approval, so the next
  connection is a fresh first contact: re-pinned, and unapproved until someone
  says otherwise. That is what the app's Trusted Devices revoke does too.
  Revoking a device that is not pinned is exit `3`. There is deliberately no
  confirmation on `revoke` — it only ever removes standing.

  Under `--json`, `list` emits one object per line
  (`{"id","name","fingerprint","trusted_at","approved"}`, `approved` an
  explicit bool and the fingerprint in full); `approve` and `revoke` emit one
  `trust_approved` / `trust_revoked` event.

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
peerbeam trust list                                # who is approved, who is only pinned
peerbeam trust approve pb-f4e4d56fce98 --yes       # scripted: no prompt
peerbeam trust revoke laptop                       # forget it entirely

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
  message carrying `status` (`staging`/`pending`/`sent`/…) and, for a file,
  `kind:"file"` plus `{"name","size","local_path"}`.
- `trust list --json` → one object per line:
  `{"id","name","fingerprint","trusted_at","approved"}`. **`approved` is a
  bool**, and it is the field to filter on — the presence of a row means only
  that the device's key was pinned when it connected.
  `trust approve --json` → one `{"event":"trust_approved",…,"changed":bool}`
  (`changed:false` when it was already approved, still exit `0`);
  `trust revoke --json` → one `{"event":"trust_revoked",…,"removed":true}`.

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
