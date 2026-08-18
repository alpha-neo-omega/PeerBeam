# PeerBeam v0.7.0 — Beta

Phase A closed; Phase C begun. Interrupted transfers can be resumed, first
contact can be verified, and a trusted device can be told what it may do.

## Highlights

### Interrupted transfers can be resumed
The engine has always been able to reconnect and resume — checkpoints, backoff,
retry, all tested. **Nothing called it.** A transfer killed by a dropped link or
a closed app was simply gone. Now the FFI persists a checkpoint, surviving
checkpoints reappear after a restart with their partial progress, and Resume
continues from the offset instead of starting over.

A checkpoint binds to its transfer — direction, consent, peer, file name and
both recorded sizes — and refuses to resume against anything else, because a
checkpoint that appended to a *different* file would corrupt it silently. And
consent cannot be laundered by a crash: a transfer you never accepted cannot be
resumed into an accepted one.

Not everything resumes. An interrupted **receive** cannot be pulled — the
protocol is sender-driven — so those offer Discard and say they are waiting for
the sender. Folders are not checkpointed either.

### First contact can be verified
Every session derives a 128-bit safety number from both devices' public keys,
and the app has always received it. It displayed none of it: a total stranger's
approval prompt looked exactly like a device you use daily.

Now a first-contact prompt says the device is new and shows the pairing code to
compare against the other screen. With **Require pairing confirmation** on
(Settings, default off), a first-contact transfer cannot be accepted until you
confirm the codes match. Refusing un-pins the device — and so does leaving the
prompt unanswered while the check is on, so a stranger connecting while nobody
is at the machine cannot quietly consume the one chance to verify it.

The gate lives in the engine, not the dialog, so it holds whoever asks.

### Per-device permissions
`approved` used to mean *everything*. A device can now be allowed **Files**,
**Messages**, **Clipboard**, **Device status** and **Pipes** independently —
this laptop may sync files without ever seeing your clipboard.

Existing trusted devices keep exactly the five permissions that existed when
their record was written, and any permission added in a later version is denied
until you grant it. Nothing you already trust stops working on upgrade, and
nothing silently gains ground.

Manage them in Settings → Trusted devices, or with `peerbeam trust list`,
`peerbeam trust permit` and `peerbeam trust revoke-permission`.

## Also
- **About** reports the engine's real version instead of a number typed into
  Dart, which had read `0.3.0` since 0.3.0.
- Trusted devices distinguishes a device you **approved** from one merely
  **pinned** by a handshake.

## Upgrade note
Presence, clipboard sync and `pipe --listen` require an **approved** device, not
merely one the handshake pinned. A device that was working on the strength of a
pin alone will stop until approved — in Trusted devices, or with
`peerbeam trust approve <device>`. That is the fix working. Devices approved
through an accept-and-trust are unaffected.

## Downloads
Linux (`.deb`, `.tar.gz`), Windows (portable `.zip`), Android (`.apk`/`.aab`),
macOS (universal `.dmg`) and the standalone CLI are attached below. Desktop and
CLI artifacts are **unsigned** — signing secrets are not configured — so
Gatekeeper and SmartScreen will warn on first open.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
