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

## Signing a macOS build locally
`scripts/package-macos.sh` does codesign → DMG → notarize → staple. Run it on a
Mac with three env vars set. Requires an **Apple Developer Program** membership.

One-time setup:

1. Create a **Developer ID Application** certificate (Xcode → Settings →
   Accounts → Manage Certificates → +, or developer.apple.com → Certificates).
   Find its identity string and your Team ID:
   ```
   security find-identity -v -p codesigning
   # → "Developer ID Application: Your Name (TEAMID)"
   ```
2. Store notarization credentials in the keychain under a name you choose:
   ```
   xcrun notarytool store-credentials "peerbeam-notary" \
     --apple-id "you@example.com" --team-id "TEAMID" \
     --password "APP-SPECIFIC-PASSWORD"   # appleid.apple.com → App-Specific Passwords
   ```

Build:
```
brew install create-dmg                # optional (nicer DMG; falls back to hdiutil)
export PB_SIGN_ID="Developer ID Application: Your Name (TEAMID)"
export PB_TEAM_ID="TEAMID"
export PB_NOTARY_PROFILE="peerbeam-notary"
bash scripts/package-macos.sh          # → dist/PeerBeam-<version>.dmg
```

Verify, then upload the DMG to the GitHub release:
```
codesign --verify --deep --strict --verbose=2 dist/PeerBeam-*.dmg
xcrun stapler validate dist/PeerBeam-*.dmg
spctl -a -t open --context context:primary-signature -v dist/PeerBeam-*.dmg
```

> CI (`release.yml`) does **not** sign macOS yet: a fresh runner has no cert in
> its keychain and no notary profile. Automating it requires importing a
> base64 `.p12` cert into a temp keychain and recreating the notary profile
> from App Store Connect API-key secrets. Until then, sign locally as above.

## Signing a Windows build
`scripts/package-windows.ps1` builds the MSIX via `dart run msix:create` and
signs it when `PB_CERT_PATH` / `PB_CERT_PASSWORD` are set. Run on Windows with
the Flutter + Rust toolchains.

**Config prerequisite:** `msix_config.publisher` in `flutter/pubspec.yaml` must
**exactly** match the signing certificate's Subject (`CN=…`), or the signed MSIX
fails to install. It ships as `CN=PeerBeam Contributors` — change it to match
your cert.

### Testing (self-signed — sideload only, not for public distribution)
```powershell
# 1. Code-signing cert whose CN matches msix_config.publisher
$c = New-SelfSignedCertificate -Type CodeSigningCert `
  -Subject "CN=PeerBeam Contributors" `
  -CertStoreLocation "Cert:\CurrentUser\My" -NotAfter (Get-Date).AddYears(3)
# 2. Export .pfx (to sign) and .cer (for testers to trust)
$pw = ConvertTo-SecureString "yourpass" -Force -AsPlainText
Export-PfxCertificate -Cert "Cert:\CurrentUser\My\$($c.Thumbprint)" -FilePath peerbeam.pfx -Password $pw
Export-Certificate  -Cert "Cert:\CurrentUser\My\$($c.Thumbprint)" -FilePath peerbeam.cer
# 3. Build a signed MSIX
$env:PB_CERT_PATH="peerbeam.pfx"; $env:PB_CERT_PASSWORD="yourpass"
powershell -File scripts/package-windows.ps1
# → flutter/build/windows/x64/runner/Release/*.msix
```
To install for testing, import `peerbeam.cer` into **Local Machine → Trusted
People** (or Trusted Root), then double-click the `.msix`.

### Distribution
A plain purchased `.pfx` is largely unavailable now — since the 2023 CA/B rules,
standard OV **and** EV code-signing certs ship on hardware tokens / cloud HSM,
so `--certificate-path` (a file) doesn't fit them. Practical options:
- **Azure Trusted Signing** — cloud signing (~$10/mo, no token), good SmartScreen
  reputation. Signs via `signtool` + the Trusted Signing dlib, not
  `--certificate-path`, so `package-windows.ps1` would need a signing-step tweak.
- **EV cert on a token** — instant SmartScreen trust; signing goes through the
  token provider, again not a file path.

> CI (`release.yml`) passes `WINDOWS_CERT_PATH`/`PASSWORD` but a runner has no
> cert file; automating requires injecting a base64 `.pfx` secret to disk and
> pointing the env at it — which only works for a **file-based** cert
> (self-signed / legacy .pfx), not token/cloud certs.
