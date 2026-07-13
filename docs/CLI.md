# CLI

`peerbeam` — a Rust frontend over `peerbeam-engine`, sibling to the Flutter
client. Crate: `bins/peerbeam-cli` (lib + `peerbeam` bin).

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
  after one transfer; `--port 0` picks an OS port (printed on start).
- `daemon start [--foreground]` — run the receive loop until interrupted.
  (`daemon stop|status` need the IPC layer — not built yet, exit code 8.)

Transfers are end-to-end encrypted: QUIC (TLS 1.3) for the pipe, plus an
application-layer X25519 mutual-auth handshake with TOFU trust pinning and
per-frame replay protection ([Security](SECURITY.md)).

Still gated (exit code 8): `clipboard`, `history`, and `daemon stop|status`.

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

`clipboard` and `history` execution, and the `daemon stop|status` IPC, are still
gated (exit code 8). Folder send (`send <dir>`) is not wired yet — send files
for now.

## Engine daemon vs CLI

The CLI `daemon` command runs a foreground receive loop. The embeddable engine
also exposes daemon control over FFI (`pb_daemon_start/stop/restart/status`) for
the Flutter app — see [FFI](FFI.md).
