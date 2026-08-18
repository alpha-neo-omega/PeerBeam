# PeerBeam v0.8.2 — Beta

Linux on ARM. A packaging release; no application code changed.

## The CLI runs natively on arm64

`peerbeam-linux-arm64` is built on a native arm64 runner — a Raspberry Pi 4 or 5
on 64-bit Raspberry Pi OS, an Ampere or Graviton server, or Asahi Linux on Apple
silicon. Install it exactly like the x86_64 one:

```bash
install -Dm755 peerbeam-linux-arm64 ~/.local/bin/peerbeam
peerbeam --version
```

Check which you need with `uname -m`: `aarch64` takes the arm64 build, `x86_64`
the other.

## The desktop app is still x86_64 only, and here is why

Google publishes **no Linux arm64 Flutter SDK**. Every one of the 730 Linux
releases in Flutter's manifest is `x64` or unspecified, so an arm64 build fails
at SDK setup before a line of PeerBeam is compiled. Building the SDK and its
engine from source on arm64 is hours of unsupported work, so the arm64 GUI job
was removed rather than left failing every release.

The CLI has no such constraint — it is plain Rust — which is why it builds
natively for both. On a Pi or a headless server the CLI is usually what you
want anyway. The packaging script already derives the architecture, so the day
an arm64 Linux SDK exists this becomes a one-line change.

32-bit ARM (`armv7l`) is not published.

## Also fixed

**Linux packaging derives the architecture rather than hardcoding it.** Four
ecosystems spell it four ways — Flutter `x64`/`arm64`, dpkg `amd64`/`arm64`, rpm
and AppImage `x86_64`/`aarch64`. It is mapped once now, so filenames, the deb's
`Architecture`, the spec's `BuildArch` and appimagetool's `ARCH` all follow.
x86_64 output is unchanged, and an unrecognised architecture fails loudly
instead of producing a mislabelled package.

**A templated artifact name no longer breaks the entire workflow.**
`with: { name: ${{ matrix.artifact }}, … }` is invalid YAML — the expression's
braces close the flow mapping early — so the release workflow silently stopped
parsing and GitHub stopped seeing its triggers. Nothing reports that clearly; it
presents as the workflow simply not running. Caught by test-triggering a build
before tagging.

## Downloads

Linux ships `.deb`, `.rpm`, `.AppImage` and `.tar.gz` for **x86_64**, plus the
**arm64 CLI**. Windows (portable `.zip`), macOS (universal `.dmg`), Android
(`.apk`/`.aab`) and the CLI for Linux, macOS and Windows are attached as before.
Everything is **unsigned** — see [GUIDE.md](GUIDE.md) for the per-platform
steps.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
