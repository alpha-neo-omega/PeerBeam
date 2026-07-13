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

Working now (no network transport needed):

- `config show|get <key>|set <key> <val>|path` — reads/writes `EngineConfig`
  JSON; dotted keys (`transfer.chunk_size`).
- `doctor [--json]` — environment checks (config/save dirs writable, UDP
  bindable, mDNS daemon, Tailscale CLI, crypto) with ✓/!/✗; non-zero exit if
  any fail.
- `benchmark crypto|hash|loopback [--size N] [--chunk KiB]` — AES-256-GCM
  seal/open and SHA-256 throughput (MiB/s); end-to-end transfer over an
  in-process link with a live progress bar (`--chunk` tunes framing).
- `discover [--timeout N] [--watch]` — scans via all providers; table or live
  NDJSON stream (Ctrl-C to stop).
- `list [--online]`, `status` — device snapshot / identity + providers.
- `completions <shell>`.

Present but gated until the QUIC transport lands (exit code 8, clear message):
`send` (validates paths + resolves/──selects peer + confirms first), `receive`,
`clipboard`, `history`, `daemon`.

## Global flags

`--json` · `-v/-vv` · `-q/--quiet` · `--no-color` · `-y/--yes` · `--config <path>`.

## Exit codes

`0` ok · `2` usage · `3` not-found · `4` connection · `5` integrity ·
`6` cancelled · `7` daemon-unavailable · `8` unavailable · `1` other.

## Verification

`cargo clippy -D warnings` clean; `cargo test` green (parse + resolver +
prompt); binary smoke-tested: `--version`, `doctor --json`, `benchmark crypto`
(~5 GiB/s here), `config get/show`, `completions bash`, `status --json`,
`send /missing --to x` → exit 3.

## Not yet

`send`/`receive`/`clipboard`/`history`/`daemon` execution and the daemon IPC
land with the transport bridge; they parse and resolve today but stop at a
gated message.
