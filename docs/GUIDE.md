# PeerBeam — Install & CLI Guide

Everything needed to get PeerBeam running on a machine and drive it from a
terminal. Written against **v0.8.0**.

- Full CLI reference: [CLI.md](CLI.md)
- Platform notes: [ANDROID.md](ANDROID.md) · [DESKTOP.md](DESKTOP.md)

> **All artifacts are unsigned.** Signing secrets are not configured for this
> project, so macOS Gatekeeper and Windows SmartScreen will warn on first open.
> The steps below say exactly what to do about it. Nothing here asks you to
> disable a security feature system-wide.

---

## 1. Download

Everything is attached to the release:
**<https://github.com/alpha-neo-omega/PeerBeam/releases/latest>**

| You want | File |
|---|---|
| Linux app (Debian/Ubuntu family) | `peerbeam-<ver>-amd64.deb` |
| Linux app (Fedora/RHEL/openSUSE) | `peerbeam-<ver>-x86_64.rpm` |
| Linux app (any distribution) | `PeerBeam-<ver>-x86_64.AppImage` |
| Linux app (portable tarball) | `peerbeam-<ver>-linux-x64.tar.gz` |
| Windows app | `peerbeam-<ver>-windows-x64-portable.zip` |
| macOS app | `PeerBeam-<ver>.dmg` |
| Android app | `peerbeam-<ver>-android.apk` |
| CLI, Linux | `peerbeam-linux-x64` |
| CLI, macOS | `peerbeam-macos-arm64` |
| CLI, Windows | `peerbeam-windows-x64.exe` |
| Shell completions | `peerbeam.bash` · `peerbeam.fish` · `_peerbeam` |

The GUI and the CLI are two frontends over the same engine. Installing both on
one machine is normal and they share the same identity, trust store and history.

---

## 2. Install the app

### Linux

Pick the row for your distribution. All of them install the same build; only the
packaging differs.

| Distribution | Package | Command |
|---|---|---|
| Ubuntu · Debian · Mint · Pop!_OS · elementary · Zorin | `.deb` | `sudo apt install ./peerbeam-<ver>-amd64.deb` |
| Fedora · RHEL · Rocky · Alma · CentOS Stream | `.rpm` | `sudo dnf install ./peerbeam-<ver>-x86_64.rpm` |
| openSUSE (Leap · Tumbleweed) | `.rpm` | `sudo zypper install --allow-unsigned-rpm ./peerbeam-<ver>-x86_64.rpm` |
| Arch · Manjaro · EndeavourOS · Garuda | `PKGBUILD` | see below |
| Anything else (NixOS, Void, Gentoo, Alpine, Slackware…) | `.AppImage` or `.tar.gz` | see below |

**AppImage — works on any distribution, installs nothing:**
```bash
chmod +x PeerBeam-<ver>-x86_64.AppImage
./PeerBeam-<ver>-x86_64.AppImage
```
Needs FUSE. If your system lacks it (common on minimal or containerised
installs), run it without mounting:
```bash
./PeerBeam-<ver>-x86_64.AppImage --appimage-extract-and-run
```

**Portable tarball — no root required:**
```bash
tar xzf peerbeam-<ver>-linux-x64.tar.gz
./opt/peerbeam/peerbeam
```

System-wide from the same tarball:
```bash
sudo cp -r opt/peerbeam /opt/
sudo ln -sf /opt/peerbeam/peerbeam /usr/bin/peerbeam
sudo cp usr/share/applications/peerbeam.desktop /usr/share/applications/
sudo cp -r usr/share/icons/hicolor/* /usr/share/icons/hicolor/
sudo update-desktop-database /usr/share/applications 2>/dev/null || true
```

**Arch and derivatives** — build a native package from the release tarball:
```bash
git clone https://github.com/alpha-neo-omega/PeerBeam
cd PeerBeam/packaging/arch
makepkg -g >> PKGBUILD     # record the tarball's checksum
makepkg -si                # build and install
```

**Runtime requirement.** The GUI needs **GTK 3** at run time; the `.deb`, `.rpm`
and `PKGBUILD` all declare it, so your package manager pulls it in. The tarball
and AppImage do not check — if the app exits immediately, install GTK 3
(`libgtk-3-0` on Debian family, `gtk3` elsewhere). The **CLI has no GTK
dependency at all**, which is what makes it the right choice on a headless
server.

Nothing here is signed or in any distribution's repositories, so your package
manager may warn about an untrusted origin — that is expected, not a
misconfiguration on your side.

**Uninstall:** `sudo apt remove peerbeam` · `sudo dnf remove peerbeam` ·
`sudo pacman -R peerbeam` · or delete `/opt/peerbeam` and the desktop entry.
Your data is left alone (see §6); remove it deliberately if you want a clean
slate.

### Windows

Unzip `peerbeam-<ver>-windows-x64-portable.zip` anywhere and run `peerbeam.exe`.
It is portable — no installer, nothing written outside the folder and your user
profile.

SmartScreen will show *"Windows protected your PC"* because the binary is
unsigned. **More info → Run anyway.**

> An MSIX installer exists in the build scripts but is not published: it needs a
> signing certificate this project does not have. The portable zip is the
> supported Windows route today.

### macOS

Open `PeerBeam-<ver>.dmg` and drag PeerBeam to Applications. The build is
**universal** — it runs natively on both Intel and Apple Silicon.

It is not notarized, so Gatekeeper blocks the first launch. Either:

```bash
xattr -dr com.apple.quarantine /Applications/PeerBeam.app
```

…or right-click the app → **Open** → **Open**. You only need this once.

### Android

```bash
adb install -r peerbeam-<ver>-android.apk
```

or copy the APK to the phone and open it (allow installs from your file manager
when prompted). `-r` upgrades in place and **keeps your identity, trust list and
history**; uninstalling first throws them away.

Grant notification and nearby-device permissions on first run — transfers use a
foreground service so they survive the screen locking.

---

## 3. Install the CLI

The CLI is a single static binary. There is nothing to install beyond putting it
on your `PATH`.

**Linux**
```bash
install -Dm755 peerbeam-linux-x64 ~/.local/bin/peerbeam
peerbeam --version
```

**macOS**
```bash
install -Dm755 peerbeam-macos-arm64 /usr/local/bin/peerbeam
xattr -d com.apple.quarantine /usr/local/bin/peerbeam   # unsigned download
peerbeam --version
```

**Windows** (PowerShell)
```powershell
mkdir $HOME\bin -Force
Move-Item peerbeam-windows-x64.exe $HOME\bin\peerbeam.exe
$env:Path += ";$HOME\bin"        # add permanently via System → Environment Variables
peerbeam --version
```

### Shell completions

```bash
peerbeam completions bash > ~/.local/share/bash-completion/completions/peerbeam
peerbeam completions zsh  > ~/.zfunc/_peerbeam        # ensure ~/.zfunc is in $fpath
peerbeam completions fish > ~/.config/fish/completions/peerbeam.fish
```

Prebuilt copies (`peerbeam.bash`, `_peerbeam`, `peerbeam.fish`) are attached to
the release if you would rather not run the binary first.

---

## 4. First run

```bash
peerbeam doctor      # check the environment: ports, providers, permissions
peerbeam status      # this device's identity, providers, and what it shares
peerbeam discover    # find nearby devices (Ctrl-C to stop)
```

`doctor` is the right first command on a new machine — it reports what is
missing rather than failing later during a transfer.

---

## 5. Everyday CLI

### Send and receive

```bash
peerbeam receive                        # accept incoming transfers
peerbeam send movie.mkv --to laptop     # resolve a peer by name/id/prefix
peerbeam send ./project/ --to laptop    # a whole folder, structure preserved
peerbeam send file.bin --addr 192.168.1.9:49600   # skip discovery entirely
```

`--to` accepts a device id, a name, or an unambiguous prefix; ambiguous input is
an error listing the candidates rather than a guess. `--addr` dials directly,
which is what you want on a headless box or across a tailnet.

### Interrupted transfers

```bash
peerbeam transfers list          # what was interrupted, newest first
peerbeam transfers resume <ID>   # continue an outgoing transfer from its offset
peerbeam transfers discard <ID>  # forget it and delete the partial file
```

An interrupted **receive** cannot be pulled — the protocol is sender-driven — so
those show as waiting for the sender.

### Chat

```bash
peerbeam chat send --to laptop "build is green"
peerbeam chat send --to laptop --file report.pdf
peerbeam chat history laptop
peerbeam chat watch                       # print messages as they arrive
peerbeam chat search "invoice"            # search your own history
peerbeam chat cancel laptop <MSG-ID>
```

Messages and files queue for an offline peer and are delivered when it returns.
`chat search` reads only this device's stored history — nothing goes on the wire.

### Trust and permissions

```bash
peerbeam trust list                              # pinned vs approved, per device
peerbeam trust approve laptop                    # shows the fingerprint, asks first
peerbeam trust approve laptop --yes              # scriptable
peerbeam trust revoke-permission laptop clipboard
peerbeam trust permit laptop clipboard
peerbeam trust revoke laptop                     # forget it entirely
```

**Pinned is not approved.** Every device that completes a handshake is pinned so
a later key change is detectable; approval is a decision you make. Presence,
clipboard sync and `pipe --listen` require an approved device.

### Clipboard

```bash
peerbeam clipboard send --to laptop     # push this machine's clipboard
peerbeam clipboard get                  # print what was last received
```

### Pipes

```bash
tar cz ./project | peerbeam pipe --to laptop     # sender
peerbeam pipe --listen > project.tgz             # receiver
peerbeam pipe --listen --from laptop > out.bin   # restrict to one device
```

A pipe is accepted **only** by a process you started with `--listen`, from an
approved device, once — a running `receive` or `daemon` refuses them. `stdout`
carries the piped bytes and nothing else, so redirecting it is safe.

### Where received files land

```bash
peerbeam rules list
peerbeam rules add --ext mp4 ~/Videos
peerbeam rules add --from laptop --min-bytes 104857600 ~/Big
peerbeam rules remove 2
```

First match wins, so order matters. With no matching rule, files land in the
configured save directory. Rules are desktop/headless only — Android receives
into the folder you granted through the system picker and cannot write
elsewhere.

### Background service

```bash
peerbeam daemon         # keep discovery + receive running
peerbeam history        # completed transfers
peerbeam config show    # inspect configuration
peerbeam config set device.name "Workshop PC"
```

### Diagnostics

```bash
peerbeam doctor
peerbeam session list          # live PeerSessions
peerbeam channels              # channels on those sessions
peerbeam diagnostics           # sessions + transport + recovery, aggregated
peerbeam benchmark loopback --size 512
```

### Scripting

Every command takes `--json` and emits one object per line, so output is
pipeable into `jq` without parsing human text:

```bash
peerbeam --json discover | jq -r '.name'
peerbeam --json trust list | jq -r 'select(.approved | not) | .id'
```

---

## 6. Where your data lives

| Platform | Path |
|---|---|
| Linux | `~/.local/share/peerbeam` (data) · `~/.config/peerbeam` (config) |
| macOS | `~/Library/Application Support/peerbeam` |
| Windows | `%APPDATA%\peerbeam` |
| Android | app-private storage; received files go to the folder you grant |

This holds your device identity, trust store, chat history, settings and the
staging area for queued files. It **survives upgrade and uninstall** — delete
the directory to reset a device completely.

Your identity keypair lives here. Copying it to another machine gives that
machine this device's identity; back it up if that matters to you, and do not
share it.

---

## 7. Troubleshooting

**Devices do not appear.** Run `peerbeam doctor`. Discovery uses UDP broadcast on
port `49500`; check a firewall is not blocking it, and that both devices are on
the same network. Tailscale peers are found through Tailscale itself — on
Android that is not possible (the app exposes no reachable API to a sandboxed
app), so add those by address instead.

**"Not approved" on a device that used to work.** Since v0.7.0, presence,
clipboard sync and pipes require an approved device rather than a merely pinned
one. Run `peerbeam trust approve <device>` — or approve it in the app under
Settings → Trusted devices.

**A transfer stopped and did not resume.** `peerbeam transfers list`. Outgoing
transfers can be resumed; incoming ones wait for the sender to offer again.

**macOS says the app is damaged.** That is Gatekeeper on an unsigned build, not
a corrupt download: `xattr -dr com.apple.quarantine /Applications/PeerBeam.app`.
