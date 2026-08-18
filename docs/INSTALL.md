# Installing PeerBeam

For a walkthrough including the standalone CLI and everyday commands, see
[GUIDE.md](GUIDE.md).

## Linux
- **tar.gz** (portable): extract and run, or install system-wide:
  ```
  tar xzf peerbeam-<ver>-linux-x64.tar.gz
  sudo cp -r opt/peerbeam /opt/ && sudo ln -sf /opt/peerbeam/peerbeam /usr/bin/peerbeam
  sudo cp usr/share/applications/peerbeam.desktop /usr/share/applications/
  sudo cp -r usr/share/icons/hicolor/* /usr/share/icons/hicolor/
  ```
- **.deb** (Ubuntu, Debian, Mint, Pop!_OS, elementary, Zorin):
  `sudo apt install ./peerbeam-<ver>-amd64.deb`
- **.rpm** (Fedora, RHEL, Rocky, Alma, CentOS Stream):
  `sudo dnf install ./peerbeam-<ver>-x86_64.rpm`
- **.rpm** (openSUSE):
  `sudo zypper install --allow-unsigned-rpm ./peerbeam-<ver>-x86_64.rpm`
- **Arch, Manjaro, EndeavourOS:** `packaging/arch/PKGBUILD` → `makepkg -si`
- **AppImage** (any distribution, installs nothing):
  `chmod +x PeerBeam-<ver>-x86_64.AppImage && ./PeerBeam-<ver>-x86_64.AppImage`
  (add `--appimage-extract-and-run` on systems without FUSE)

The GUI needs **GTK 3** at run time; the packaged formats declare it, the
tarball and AppImage do not. The CLI needs nothing.

Uninstall: `sudo apt remove peerbeam` / `sudo dnf remove peerbeam` / delete
`/opt/peerbeam` + the desktop entry. Config/history persist under
`~/.local/share/peerbeam` and `~/.config/peerbeam` (untouched by uninstall).

## Windows
Unzip **`peerbeam-<ver>-windows-x64-portable.zip`** and run `peerbeam.exe`. It is
portable: no installer, and nothing written outside the folder and your user
profile. SmartScreen warns because the binary is unsigned — *More info → Run
anyway*.

The MSIX packaging in `scripts/package-windows.ps1` is **not published**: it
needs a signing certificate this project does not have, so its CI job is
`continue-on-error` and only the portable zip ships.

## macOS
Open the **DMG** and drag PeerBeam to Applications. The published DMG is **not notarized**, so Gatekeeper
blocks the first launch: either `xattr -dr com.apple.quarantine
/Applications/PeerBeam.app`, or right-click the app → *Open* → *Open*. The build
is universal (Intel + Apple Silicon).
Uninstall: move the app to Trash (config persists under
`~/Library/Application Support/peerbeam`).

## Android
Install the **APK** (`adb install -r peerbeam-<ver>-android.apk`) or ship the
**AAB** via Play. `-r` upgrades in place and keeps identity, trust and history;
uninstalling first discards them.
Grant notification + nearby-devices permissions on first run. Background
transfers use a foreground service ([Android](ANDROID.md)).

## Configuration persistence
Settings/history/trust live in the OS data dir and survive upgrade + uninstall.
Reset by deleting the data directory (see per-platform paths above).
