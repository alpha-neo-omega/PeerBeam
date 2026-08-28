# PeerBeam v0.11.0 — Beta

Group conversations, and a long correctness sweep that found more than expected.
Several of these are worth reading about: files over 256 MiB had never synced,
a sender could write past the size you approved, and a peer could name any
location on a Windows disk.

If you sync folders, share folders with Windows devices, or have ever set a
preference you cared about, upgrading is worth it.

## Groups

A conversation a set of devices share. Everyone in it holds the same roster, so
a reply reaches everyone — and **everyone in it learns who everyone else is**.

```
peerbeam group create "Work Trip"
peerbeam group invite "Work Trip" alices-laptop
peerbeam group send "Work Trip" "six works for me"
```

There is no server and no host. A message is N ordinary one-to-one sends over
the same routes and the same encryption a hand-addressed message uses; there is
no group key, so nothing can be compromised once and expose the whole
conversation. Nobody is asked who is in a group, because every member already
holds the roster.

The cost is the disclosure, and it cannot be undone. Joining says so before you
answer, and names the devices rather than counting them — somebody who would
decline over one particular device cannot act on a number.

Permitted by amendment **A2** in `docs/ARCHITECTURAL_INVARIANTS.md`, on eight
binding conditions. Groups are documented in [GROUPS.md](GROUPS.md), which opens
with a table for choosing between a Group and a [Space](SPACES.md) — they trade
opposite things, and picking the wrong one discloses something you did not mean
to.

In the app: **Groups** in the navigation, with the conversation, the invite
action, and pending invitations shown above your groups.

## Folder sync: files over 256 MiB now actually sync

They never did. Both delta paths refused a file whose chunk map exceeded an
in-memory ceiling and handed it to a "whole file" fallback — which sent a request
whose only handler emitted an event nothing in the codebase consumes. The file
never arrived, nothing failed, and the sync counted it as fetched.

Chunks are now fetched a window at a time and written as they arrive, so a
file's size is the filesystem's business rather than memory's. Two other things
fall out of the same change:

- **A failed sync no longer destroys your copy.** It used to write straight to
  the destination, so a chunk that did not verify, a disconnect or a full disk
  left the file truncated — having already replaced one that was fine. It now
  stages beside the destination and renames only when the whole file is written.
- **What did not arrive is named.** `peerbeam sync` and the app report the files
  that failed instead of counting them among the fetched.

## Security

- **A peer could write outside the sync folder on Windows.** A `/`-separated
  path from a peer meant `..\..\x` was a *single* segment — neither `.` nor
  `..` — which Windows then expanded with its own separator. A rooted segment
  like `C:\evil` was worse: it discarded the destination entirely.
- **The size you approve is now a bound.** A sender could declare "photo.jpg,
  2 MB", stream ten gigabytes and finish with an honest checksum of what it
  actually sent — so integrity passed and the oversized file was published under
  the name and size you consented to.
- **A malformed message could kill a channel before approval.** Peer-supplied
  hex was indexed by byte, so one character wider than a byte panicked the
  handler. Reachable with a plain JSON escape.
- **The `files` permission is enforced by the CLI.** It never was: `peerbeam
  receive` and `peerbeam daemon` took files from any authenticated peer while
  the docs said otherwise. Only the refusal was added — a device you approved
  and then narrowed. A device you never decided about is unaffected, so a
  headless receiver keeps working.
- The trust store is written `0600`, like every other sensitive store.
- Two message types could be pushed by any handshaken peer into queues nothing
  drains, and an unauthenticated LAN announce could grow the device list without
  limit. Both are bounded now.

## Settings could silently reset

`ffi_settings.json` was written with a call that truncates first, so a crash or
a full disk between the truncate and the write left a file the app cannot parse
— which it quarantines, writing defaults in its place. Every preference you set
was gone, including `require_pairing_confirmation`, which defaults **off**: a
security gate you turned on came back off with nothing on screen. It is now
written to a temp file, flushed, and renamed.

## Also fixed

- **A receiver told the sender a file was safe before it was written.**
  `Verify { ok: true }` went out before the file was flushed, closed or renamed,
  so a full disk or a failed rename produced "received and verified" on the wire
  and no file on disk — after the sender had been told it could stop caring.
- **Two folder receives of the same filename shared one staging file** and
  published a mixture of both senders' bytes.
- **Two devices joining a group at once lost one of them**, unrecoverably: the
  inviter's copy is the only complete roster.
- **A CLI receiver could be parked by one silent peer.** Each connection is now
  served on its own task, so one peer's transfer no longer delays everyone else.
  Measured: 2 seconds against 118.
- **The app froze on anything that dialled a peer.** Seven calls ran on the UI
  isolate — including marking a conversation read, which happens every time you
  open one. They now run off it.
- Clearing history reported success over a write that failed. Notes sync from
  the app could never succeed. Android never reported its battery. Tapping a
  "message" in History read a peer-named file of any size into memory.

## Upgrade note

Nothing to do. Trust, history, settings and chats carry over.

A device whose **Files** permission you revoked will now actually be refused by
`peerbeam receive` and `peerbeam daemon`, which previously accepted from it. If
a headless box stops accepting from a device after upgrading, that is this
change, and `peerbeam trust permit <device> files` restores it.

## Under the hood

- Android's Kotlin unit tests now run in CI. They existed and ran nowhere, which
  is why every Android defect this project has found was found by reading code.
- 1817 Rust tests and 620 Flutter tests, with `clippy -D warnings` and
  `flutter analyze` clean on Linux, Windows, macOS and Android.

## Downloads

Linux (`.deb`, `.tar.gz`), Windows (portable `.zip`), Android (`.apk`/`.aab`),
macOS (universal `.dmg`) and the standalone CLI are attached below. Desktop and
CLI artifacts are **unsigned**.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
