# PeerBeam v0.9.0 — Beta

Folder sync grows up, pairing gets a person in the loop, and the logs the engine
has always kept are finally readable.

## Only what changed crosses the wire

Folder sync now splits files into content-defined chunks and transfers only the
pieces the other side lacks. Editing one line of a large file costs one line.

Measured, not claimed: a 64-byte edit to a 2 MB file transfers **117,518 bytes —
5.88%** — and rebuilds the file byte-exact. The figure comes from a test that
prints it, so it cannot quietly stop being true.

Boundaries are chosen by content rather than position, so inserting a byte at the
front disturbs one chunk instead of shifting every boundary after it. Chunks are
reused from anywhere you already hold them — an older version, a copy under
another name, even an unrelated file that happens to share content — because a
chunk's identity is its hash and where it came from does not matter.

**A moved or renamed file sends nothing at all.** A deletion and a creation
carrying the same content hash are recognised as one file that moved, and the
local copy is moved to match.

Every chunk is verified against its hash before it is written. A peer that sends
the wrong bytes under a right-looking name gets nothing into your file.

## Sync that keeps running

`peerbeam sync --watch <seconds>`, and a matching control in the app. A file is
only acted on once it has stopped changing, so saving a large file while a poll
is running never syncs a half-written copy.

## PIN pairing

Trust-on-first-use pins whatever key answers first. If someone is on the path
during that first handshake, their key gets pinned instead of your device's, and
every later connection looks perfectly consistent.

`peerbeam pair` closes that window. One device shows six digits, a person reads
them across, and the other proves it knows them.

**The PIN is never sent, and the six digits are not a shortened fingerprint.** A
short code derived from the two public keys would be forgeable — an attacker
generates keypairs until one produces the digits you expect, which takes seconds.
This PIN is a fresh random secret used to sign *this* handshake, so a proof is
worthless on any other connection, including the second leg of a
machine-in-the-middle. Three wrong guesses end the pairing.

Off by default. Turning `encryption.require_pin_pairing` on means nothing pairs
without a person at both ends.

## Logs you can read

The engine has captured structured logs for a long time. Nothing could reach
them — not the app, not the CLI.

`peerbeam logs [--limit N] [--export PATH]` now reads them, and the app can list,
export and stream them. Logs are written to `<data>/logs/peerbeam.jsonl` as well
as held in memory, because a per-process buffer is gone the moment the process
is — which is exactly when you want the logs. The file is bounded and rotated
once; `log.to_file = false` turns it off.

## Shared folders are a setting

Choosing what you share meant editing a config file. It is a setting now, applied
**immediately** — un-sharing a folder that stayed shared until the next restart
was the one direction this must never lag in. Still empty by default, and a peer
still needs the Browse permission.

## Fixed

**A completed send could reappear as interrupted.** The transfer cleared its
checkpoint while the progress pump was still draining a backlog, and a late write
put the checkpoint back — so a finished transfer showed up as interrupted, with a
Restart button, for a file that had fully arrived. It reproduced in 2 of 6 full
test runs and never once in isolation.

## Downloads

| Platform | GUI | CLI |
| --- | --- | --- |
| Linux x86_64 | AppImage / tar / deb | `peerbeam-linux-x64` |
| Linux arm64 | AppImage | `peerbeam-linux-arm64` |
| Windows x64 | portable zip | `peerbeam-windows-x64.exe` |
| Windows arm64 | portable zip | `peerbeam-windows-arm64.exe` |
| macOS (Intel + Apple Silicon) | universal DMG | `peerbeam-macos-x64`, `-arm64`, `-universal` |
| Android | APK | — |

Signed installers (notarized DMG, signed MSIX) still await their signing secrets.

**The arm64 GUI builds are not in this release.** Flutter publishes no arm64 SDK
archive for Linux or Windows — its release manifest lists `x64` only — so the
build could not start. The arm64 **CLI** for Linux and Windows did build and is
attached, as is the macOS x64 CLI. The GUI on arm64 is being worked on; the
release went out without it rather than being withheld.
