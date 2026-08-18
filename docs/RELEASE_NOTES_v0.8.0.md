# PeerBeam v0.8.0 — Beta

Search your own chat history, and the first release built for every platform
since v0.2.4.

## Highlights

### Chat search
Search every conversation on this device at once — message text and shared file
names, case-insensitively, newest first. Results group by conversation; tapping
one opens that thread.

It runs in the engine rather than filtering in the app, so a query does not drag
every message of every conversation across the FFI to answer. It is local by
construction: no wire message, nothing a peer can see, and nothing that needs a
peer online. A file's path on disk is deliberately **not** searched — that is
this machine's filesystem layout, not conversation content, and matching on it
would surface a thread because of where a file happens to sit.

Results are bounded, and truncation is reported rather than hidden: "the newest
50 of many" is a different answer from "that is all there is".

Available in the app's Chats destination and as `peerbeam chat search <query>`.

### Every platform is built again
The release workflow ended with `gh release create --notes-file
docs/RELEASE_NOTES_<tag>.md`, unconditionally. A tag without that file failed
the job *after* all six platforms had built, discarding every artifact — and
because the preceding step deletes any existing release before recreating it, a
re-pushed tag could destroy a good release and put nothing back. **v0.4.1,
v0.5.0 and v0.6.0 are tagged with no release at all for exactly this reason.**

Missing notes are now a warning and generated notes, not a lost release, and
`scripts/set-version.sh` scaffolds the notes file when the version is set — so
it is caught while someone is already thinking about the release rather than
minutes into CI.

This release therefore carries **Windows (portable zip), macOS (universal DMG)
and the Windows/macOS CLI builds** again, alongside Linux and Android.

## Also
- The README describes what PeerBeam has become — chat, clipboard sync,
  presence, pipes, auto-save rules, permissions — and its test counts are
  recomputed by `scripts/readme-test-counts.sh` instead of typed by hand. They
  had drifted to "377 Rust + 35 Flutter" against actual figures of 1135 and 310.

## Upgrade note
Nothing behaves differently after upgrading from 0.7.0. The permission and
approval changes that need attention arrived in 0.7.0 — see
[its notes](RELEASE_NOTES_v0.7.0.md) if you are coming from 0.6.0 or earlier.

## Downloads
Linux (`.tar.gz`, `.deb`), Windows (portable `.zip`), Android (`.apk`/`.aab`),
macOS (universal `.dmg`) and the standalone CLI for Linux, macOS and Windows are
attached below. Desktop and CLI artifacts are **unsigned** — signing secrets are
not configured — so Gatekeeper and SmartScreen warn on first open. On macOS:

```
xattr -dr com.apple.quarantine /Applications/PeerBeam.app
```

Full detail in [CHANGELOG.md](../CHANGELOG.md).
