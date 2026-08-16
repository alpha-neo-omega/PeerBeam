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
- `history [--limit N] [--clear]` — persisted transfer history (sends and
  receives, success or failure), `<data_dir>/history.json`, same schema as the
  app engine's history, bounded to the 500 most recent.

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
prompt + config round-trip); an **end-to-end test spawns two `peerbeam`
processes** and transfers a file over QUIC (`tests/transfer_e2e.rs`). Binary
smoke-tested incl. `send`/`receive` over both discovery and `--addr`.

## Not yet

The `daemon stop|status` IPC is still gated (exit code 8).

## Engine daemon vs CLI

The CLI `daemon` command runs a foreground receive loop. The embeddable engine
also exposes daemon control over FFI (`pb_daemon_start/stop/restart/status`) for
the Flutter app — see [FFI](FFI.md).
