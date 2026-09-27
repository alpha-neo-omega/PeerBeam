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
| `MINISIGN_SECRET_KEY` | signs `SHA256SUMS`, so the app can verify a download ([below](#signing-the-checksums-minisign)) |

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

**The debug-signed APK is worse than it sounds.** An Android debug keystore is
generated per machine, and CI runners are ephemeral, so each release is signed
with a *different* throwaway key: `v0.11.0`'s certificate is
`CN=Android Debug` with `notBefore` set to the minute that build ran. Android
refuses to install over a package whose signature differs, so every release
fails to upgrade the last one — users get "App not installed as package
conflicts with an existing package" and have to uninstall, losing that device's
identity, trust store and chat history each time. Setting
`ANDROID_KEYSTORE_BASE64` + `ANDROID_KEY_PROPERTIES` once fixes this
permanently, and costs nothing: `keytool` generates the keystore and Android has
no certificate authority.

## Checksums
Every release attaches **`SHA256SUMS`**, generated in the `release` job over
exactly the files being uploaded and attached in the same `gh release create`
call — a separate upload step could fail *after* the release exists and leave
artifacts with no checksums and no visible error.

Names in it are basenames, so `sha256sum -c SHA256SUMS` works in whatever
directory somebody downloaded into. `docs/GUIDE.md` documents the user side.

On its own it is **integrity, not authenticity**: the file sits beside the
artifacts it describes, so whoever could swap one could swap both. It catches
truncation and corruption, and lets two people confirm they hold the same
bytes. Proving *origin* is what the signature below adds.

## Signing the checksums (minisign)

`SHA256SUMS.minisig` is a [minisign](https://jedisct1.github.io/minisign/)
signature over `SHA256SUMS`, written by `scripts/sign-release.sh` and attached
in the same `gh release create` call for the same reason the checksums are.

**Why it exists.** Amendment A3 in
[ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md#a3--downloading-a-release-the-user-asked-for-2026-09-27)
permits PeerBeam to download a release a user asked for, and its third binding
condition requires the bytes to be checked against a checksum list **this
project signed**. `peerbeam-update` refuses any download it cannot verify, with
no override and no "proceed anyway" — so an unsigned release is simply one the
app will not fetch. The website download still works by hand, which is the
situation today.

A release with no key configured still publishes, unsigned. A missing secret is
not a reason to withhold six platforms' artifacts.

### Generating the key (once)

```bash
# -W: no password. See the custody note below for why.
minisign -G -W -p peerbeam.pub -s peerbeam.key
```

Then:

1. Put the **contents of `peerbeam.key`** in the `MINISIGN_SECRET_KEY`
   repository *secret*.
2. Put the **second line of `peerbeam.pub`** (the base64, not the comment) in
   the `MINISIGN_PUBLIC_KEY` repository *variable* — the release checks its own
   signature back against it before publishing, so a mis-pasted key fails the
   release rather than every user's download.
3. Paste the same base64 into `SIGNING_PUBLIC_KEY` in
   `rust/crates/peerbeam-update/src/verify.rs` and commit it. **It belongs in
   version control**: it is public, and committing it is what pins it. A key
   read from configuration at runtime could be replaced by anyone who can write
   that configuration, which defeats the point.
4. Delete `peerbeam.key` from the machine that generated it, or keep it offline
   — see below.

### Custody

The key is unencrypted, because a password-protected one cannot be used
non-interactively and CI has nowhere to type it. The protection is therefore the
secret store, not a passphrase. Two consequences worth accepting deliberately:

- Anyone who can run a workflow on this repository can sign a release. Limit who
  can, and prefer a protected environment on the release job.
- If you would rather CI never hold the key, do not set the secret. Sign
  locally instead — `MINISIGN_SECRET_KEY_FILE=/path/to/peerbeam.key
  scripts/sign-release.sh dist/SHA256SUMS v0.13.0` — and upload the `.minisig`
  to the release by hand. The script takes a password-protected key on that
  path, since a person is there to type it.

### Rotation

`SIGNING_PUBLIC_KEY` is compiled in, so **every build trusts the key it shipped
with**. Replacing the key does not reach installed builds: they go on trusting
the old one and will refuse releases signed with the new one. That shows up as
"this download could not be verified" and never as accepting something it
should not — the failure is in the safe direction, but it is a failure.

So rotation means: ship a release signed with the **old** key that carries the
**new** public key compiled in, let it propagate, and only then switch signing
to the new key. Anyone who skips that intermediate release must re-download by
hand from the website, which still works.

If the secret key is lost or compromised, there is no revocation mechanism.
Publish the new public key on the website and in the release notes, and treat
the hand-download path as the recovery route.

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
