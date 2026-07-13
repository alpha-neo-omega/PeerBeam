# Installing PeerBeam

## Linux
- **tar.gz** (portable): extract and run, or install system-wide:
  ```
  tar xzf peerbeam-<ver>-linux-x64.tar.gz
  sudo cp -r opt/peerbeam /opt/ && sudo ln -sf /opt/peerbeam/peerbeam /usr/bin/peerbeam
  sudo cp usr/share/applications/peerbeam.desktop /usr/share/applications/
  sudo cp -r usr/share/icons/hicolor/* /usr/share/icons/hicolor/
  ```
- **.deb:** `sudo apt install ./peerbeam-<ver>-amd64.deb`
- **.rpm:** `sudo dnf install ./peerbeam-<ver>.x86_64.rpm`
- **AppImage:** `chmod +x PeerBeam-<ver>-x86_64.AppImage && ./PeerBeam-<ver>-x86_64.AppImage`

Uninstall: `sudo apt remove peerbeam` / `sudo dnf remove peerbeam` / delete
`/opt/peerbeam` + the desktop entry. Config/history persist under
`~/.local/share/peerbeam` and `~/.config/peerbeam` (untouched by uninstall).

## Windows
Install the **MSIX**: double-click, or `Add-AppxPackage peerbeam-<ver>.msix`.
An unsigned MSIX needs its test certificate trusted first (dev only). Start-menu
entry + optional desktop shortcut are created automatically. Uninstall from
*Settings → Apps*. Upgrading installs a higher `msix_version` in place.

## macOS
Open the **DMG** and drag PeerBeam to Applications. First launch on a notarized
build passes Gatekeeper; an un-notarized build needs *right-click → Open*.
Uninstall: move the app to Trash (config persists under
`~/Library/Application Support/peerbeam`).

## Android
Install the **APK** (`adb install app-release.apk`) or ship the **AAB** via Play.
Grant notification + nearby-devices permissions on first run. Background
transfers use a foreground service ([Android](ANDROID.md)).

## Configuration persistence
Settings/history/trust live in the OS data dir and survive upgrade + uninstall.
Reset by deleting the data directory (see per-platform paths above).
