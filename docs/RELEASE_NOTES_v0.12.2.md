# PeerBeam v0.12.2 — Beta

The app can download its own updates now: verified against the project's
signature, into a folder you choose, and never installed behind your back. And a
chat fix: a message sent just before a conversation closed could still go
missing on arrival.

If you use chat, upgrade.

## A message sent as a session closed could still be lost on arrival

v0.12.0 fixed the sending side of this: a closing session now delivers its last
message before it hangs up. The receiving side could still drop it. Closing
announces itself on one stream and the last message travels on another, and
nothing orders two streams against each other. The receiver stopped reading
everything the moment it saw the close, discarding whatever had arrived last.

It hit everything that connects, sends one thing and closes: `chat send`, group
invitations, clipboard pushes, `peerbeam identify` — and declining a file, where
it showed most: the decline went missing and the file read "failed". The
receiver now reads each channel to its end before closing, as the session's
documented state machine always said it should.

## Download updates from the app

In **Settings → Check for updates**, when a newer release exists, the tile now
offers **Download**:

- It asks where to save the file, starting at your Downloads folder, and fetches
  nothing unless you pick one.
- It fetches the right file for this machine, and checks it against the
  project's signature before keeping it. A file that does not verify is deleted,
  and there is no way to keep it anyway.
- Progress shows as a percentage. When it is done, **Show folder** opens the
  folder.

**PeerBeam never installs, opens or runs what it downloads.** Show folder opens
the *folder*, not the file, because your system would hand a `.deb`, `.rpm` or
`.dmg` straight to an installer. You install it exactly as you would one from
the website. Checking never starts a download by itself, and phones are offered
no download (Android updates through its own install).

**This release ships the button; the next one is the first it can fetch.** The
button only downloads releases newer than itself, so on 0.12.2 it has nothing to
download until 0.12.3.

If you are on **0.12.1 and use the CLI**, this is the first release
`peerbeam download-update` can fetch: 0.12.1 was the first signed release, and
0.12.2 is the first newer one.

## `peerbeam download-update` tells a script what happened

It used to exit `0` every time, even when it wrote nothing — so a script running
`peerbeam download-update && install …` went on to install after a refusal. Now:

| Exit | Meaning |
|---|---|
| `0` | a verified file was written, or there is nothing newer |
| `4` | the release list or GitHub could not be reached |
| `5` | refused: signature, digest, a redirect or the host failed a check |
| `8` | there is no file for this machine |
| `1` | the file could not be written |

Being offline is `4` and never `5`: one is a bad day, and the other may be an
attack. A machine that can never be served — a tarball or source build, Android,
an unknown architecture — is refused before anything is asked of the network,
and failures are reported on stderr. Details in
[CLI.md](CLI.md), under `download-update`.

## The download keeps its promises more closely

None of these was exploitable, because nothing that the project did not sign is
ever kept. Each made the download say or do less than it promises.

- **The hop to GitHub's file host no longer names where it came from.** The HTTP
  client added a `Referer` to every redirect it followed, telling
  `release-assets.githubusercontent.com` the release URL — and with it, the
  product.
- **A redirect off the allowed hosts now says so**, naming the host, instead of
  reading as a bad signature.
- **A "version" from the release list that is not one is refused** before it can
  become part of a URL or a file name.

## Upgrade note

- `peerbeam download-update` now exits non-zero when it did not write a verified
  file. A script that relied on it always exiting `0` will see the difference;
  that is the point.
- Android build 20.

## Downloads

Linux (`.deb`, `.rpm`, `.AppImage`, `.tar.gz`), Windows (portable `.zip`),
Android (`.apk`/`.aab`), macOS (universal `.dmg`) and the standalone CLI are
attached below. Desktop and CLI artifacts remain **unsigned** in the
code-signing sense; the checksum list is signed, so you can check a download is
the project's:

```bash
minisign -Vm SHA256SUMS -P RWTGMEdNd5xF/JoeUsh7Y8S9ItJUd/MOzzDziVa53xMzW0EJuZIveB0D
sha256sum -c --ignore-missing SHA256SUMS
```

Full detail in [CHANGELOG.md](../CHANGELOG.md).
