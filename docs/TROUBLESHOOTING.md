# Troubleshooting

Start with the built-in environment check:

```bash
peerbeam doctor          # add --json for machine output
```

`doctor` reports whether the config and save directories are writable, a UDP
socket is bindable, the mDNS daemon is available, the Tailscale CLI is present,
and crypto works. Anything marked ✗ points straight at the problem below.

## Discovery

**No devices appear.**
- Both devices must be on and running PeerBeam, and reachable by at least one
  provider (same LAN/subnet, same mDNS domain, or the same tailnet).
- Run `peerbeam discover --watch` on both to see live results (NDJSON with
  `--json`).
- **Firewall.** LAN discovery uses UDP broadcast; mDNS uses multicast. Allow
  PeerBeam (and UDP on the discovery port) through the OS firewall.
- **Wi-Fi client isolation / guest networks** block peer-to-peer traffic —
  common on public and some home APs. Use a trusted network or Tailscale.
- **VLANs / subnets.** Broadcast and multicast don't cross subnets. Devices on
  different subnets won't see each other via LAN/mDNS — use Tailscale.

**Only some providers find a device.** That's expected and fine — results are
merged, so a device found by any provider shows once. See [Devices](DEVICES.md).

**Tailscale peers missing.**
- `peerbeam doctor` should show the Tailscale CLI as present; if not, install
  Tailscale and ensure `tailscale` is on `PATH`.
- Confirm the node is up: `tailscale status`.
- MagicDNS names require MagicDNS enabled in your tailnet.
- On headless/daemon setups the LocalAPI socket must be accessible to the user
  running PeerBeam.

## Transfers

**`send` / `receive` / `daemon` say "unavailable" (exit code 8).**
This is expected in the current build: the network transport
(`TransferProvider`, planned QUIC) isn't implemented yet, so these commands are
gated. They parse and resolve peers but stop before moving bytes. Track this in
[Migration](MIGRATION.md). The transfer pipeline itself works and can be
exercised with `peerbeam benchmark loopback`.

**A transfer was interrupted.**
- Transfers are resumable. Partly-received data is kept in a `<name>.part` file;
  re-running continues from the receiver's offset rather than restarting.
- On completion the file is verified with a whole-file SHA-256 and atomically
  promoted to its final name. A `.part` left behind means the transfer didn't
  finish — it's safe to resume.

**"checksum mismatch" / integrity error (exit code 5).**
The received data didn't match the sender's whole-file hash (corruption or
tampering). The final file is *not* created; the `.part` remains so you can
retry. Retry the transfer; if it persists, suspect the network path or storage.

**Received file has a `(1)` suffix.** PeerBeam never overwrites: if the
destination name exists, a non-colliding name is chosen. See [Security](SECURITY.md).

## Configuration

**Where is my config?**
```bash
peerbeam config path
peerbeam config show          # full config as JSON
peerbeam config get transfer.chunk_size
peerbeam config set device.name "My Laptop"
```
Keys are dotted paths into the config tree. Setting an unknown key fails with a
usage error (exit code 2); reading an unknown key is not-found (exit code 3).

**Config won't load.** A malformed file, or one missing a required section,
fails with a parse error — PeerBeam won't silently half-load it. Fix the JSON
or delete the file to fall back to defaults. Unknown *extra* keys are ignored
(forward-compatible), so a newer file still loads on an older build.

**Change the download location.**
```bash
peerbeam config set storage.save_directory "/path/to/downloads"
```

## CLI behaviour

**No colour / no progress bar / no prompts.** PeerBeam auto-disables these when
output isn't a terminal (pipes, SSH without a TTY), when `NO_COLOR` is set, when
`TERM=dumb`, or with `--json` / `--quiet`. This is intentional for scripting and
SSH. Force plain output with `--no-color`; skip prompts with `-y/--yes`.

**Scripting.** Use `--json` for machine-readable output (NDJSON for streaming
commands like `discover --watch`) and branch on the exit code. Full list of exit
codes in [CLI](CLI.md).

## Android

- Grant notification and (on Android 13+) nearby-devices/network permissions.
- Background transfers rely on a foreground service; if the OS kills it, exempt
  PeerBeam from battery optimization when prompted. See [Android](ANDROID.md).

## Performance

If throughput seems low, see [Benchmarks](BENCHMARKS.md) — it documents measured
baselines and what bounds the pipeline (hardware-accelerated hashing + memory
bandwidth on the local path). Real network numbers await the QUIC transport.

## Still stuck?

- Increase log verbosity: `-v` or `-vv` (structured `tracing` logs).
- Re-run `peerbeam doctor --json` and include the output when filing an issue.
- Check the component docs linked from the [README](../README.md).
