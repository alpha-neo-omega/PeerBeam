# Releasing PeerBeam

## Process
1. `scripts/set-version.sh X.Y.Z` — bump + sync versions; commit.
2. Tag: `git tag vX.Y.Z && git push --tags`.
3. CI (`release.yml`) builds all platforms and uploads artifacts.
4. Verify each artifact (checklist below), then publish a GitHub Release.

## Local packaging
Run the matching `scripts/package-*` on each host (see [BUILD.md](BUILD.md)).
Cross-building desktop installers is not supported — build each on its own OS.

## Required secrets (CI)
| Secret | Purpose |
|---|---|
| `WINDOWS_CERT_PATH`, `WINDOWS_CERT_PASSWORD` | MSIX code-signing cert (.pfx) |
| `MACOS_SIGN_ID` | "Developer ID Application: …" identity |
| `MACOS_TEAM_ID`, `MACOS_NOTARY_PROFILE` | notarytool credentials |
| `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEY_PROPERTIES` | release keystore + key.properties |

Never commit certs/keystores. Android `key.properties` + `*.jks` are git-ignored;
use `android/key.properties.example` as a template.

## Verification checklist (per platform)
- [ ] **Install** the package cleanly (no manual file copying).
- [ ] App launches; discovery finds a peer; a real transfer completes.
- [ ] **Upgrade** over a previous version in place; settings/history persist.
- [ ] **Uninstall** removes the app; user data remains (documented).
- [ ] Version shown matches the tag (`pb_version_json` / About).

## Signing status
Config reads certs/keys from env/secrets. Without them, builds still produce
**unsigned/test** artifacts (Linux tar.gz, unsigned MSIX, un-notarized DMG,
debug-signed APK) — usable for testing, not for distribution.
