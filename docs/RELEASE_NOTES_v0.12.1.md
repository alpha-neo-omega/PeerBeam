# PeerBeam v0.12.1 — Beta

Five defects that only ever appeared on Windows and macOS, the CI gap that let
them ship, and the first half of a verified in-app update.

If you run PeerBeam on macOS, upgrade. One of these could leave the app running
with no way back to it.

## On macOS, closing the window could strand the app

Read this one even if you skip the rest.

The menu-bar icon was a white glyph handed to macOS as ordinary pixels rather
than a *template* image, so on a Light-appearance menu bar — the default — it
drew white on white and there was nothing to see. Separately, closing to the
menu bar hides the window with `orderOut:`, and the app never handled the Dock
icon being clicked afterwards.

Either alone is a nuisance. Together they meant: close the window, and you had a
running PeerBeam with no window, an invisible menu-bar icon, and a Dock icon
that did nothing. The only way out was killing the process.

Both are fixed, and independently — either fix alone closes the trap.

The same icon problem affected the Windows notification area on the Light theme,
for the same reason. Windows has no template-image concept at all, so it now
ships a second dark-glyph icon and picks between them by system theme,
re-picking when you switch.

## The rest of the macOS fixes

- **A left click on the menu-bar icon opens the menu**, as macOS expects,
  instead of yanking the window forward.
- **The app is called "PeerBeam" again** in the menu bar, the Dock, Finder and
  the About/Hide/Quit items. All of them read a lowercase `peerbeam` that
  disagreed with the window title, the website, and the other three platforms.
- **The window opens at 1280x720**, matching Windows and Linux. It opened at
  800x600, which is below the layout's medium breakpoint — so a first run on
  macOS landed in the collapsed icon-only rail and looked like a different
  application.

## Why five platform bugs shipped at once

CI built the app on Windows and macOS but only ever *tested* it on Linux. None
of the five is a compile error, so building them was never going to catch them.

`flutter analyze` and `flutter test` now run on Windows and macOS runners too.
That change immediately found a real Windows ordering bug that had never been
visible before, which is being fixed separately — the job doing what it was
added for.

## Checksums are now signed

`SHA256SUMS` has shipped with every release, and it has only ever proved a
download was not corrupted. It sits beside the files it describes, so whoever
could replace one could replace both.

From this release it is signed with a [minisign](https://jedisct1.github.io/minisign/)
key, and `SHA256SUMS.minisig` is attached alongside it. To verify a download
yourself:

```bash
minisign -Vm SHA256SUMS -P RWTGMEdNd5xF/JoeUsh7Y8S9ItJUd/MOzzDziVa53xMzW0EJuZIveB0D
sha256sum -c SHA256SUMS
```

## `peerbeam download-update`

A new CLI command fetches the release for your platform, checks it against those
signed checksums, and writes it to disk. It does **not** install it, run it, or
touch the running PeerBeam — you get the file, to install exactly as you would
one from the website.

A download it cannot verify is deleted rather than kept, and there is no flag to
skip the check. On Linux it refuses unless it can establish which package format
you installed, because handing you an `.rpm` when you installed a `.deb` is
worse than handing you nothing.

It is permitted by two amendments recorded in
[ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md) — PeerBeam does not
phone home, and any exception to that is written down before it is built.

**This release is the one that makes the next one fetchable.** v0.12.1 is the
first signed release, so there is nothing signed behind it yet for the command
to pull. From v0.12.2 onward it works.

## Upgrade note

Nothing behaves differently after upgrading, and no configuration changes.
Android users get build 19, so the upgrade installs over 0.12.0 normally.

## Downloads

Linux (`.deb`, `.rpm`, `.AppImage`, `.tar.gz`), Windows (portable `.zip`),
Android (`.apk`/`.aab`), macOS (universal `.dmg`) and the standalone CLI are
attached below. Desktop and CLI artifacts remain **unsigned** in the
code-signing sense — the new minisign signature proves the *release* is ours,
not that Windows or macOS will launch it without a warning.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
