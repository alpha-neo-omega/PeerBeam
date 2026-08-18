# Changelog

All notable changes to PeerBeam. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); the project is pre-1.0 and
versioned per [Supported Versions](SUPPORTED_VERSIONS.md).

## [Unreleased]

### Added
- **Browse and mirror a device's shared folders.** Point `device.shared_directories`
  at what you want to offer (**empty by default**), grant a device the new
  **Browse** permission, and it can list what is there — in the app from a
  device's menu, or with `peerbeam browse <peer> [path]`.

  `peerbeam sync <peer> <path> <into>` mirrors a shared folder locally. It is a
  **one-way pull**: nothing local is deleted, nothing is pushed back, and a file
  newer on your machine is never overwritten. Receiving files needs the `files`
  permission as well as `browse` — being allowed to see that a file exists is
  not being allowed to take it.

  An empty listing is the same answer whether the device shares nothing, has not
  granted you permission, or the folder is gone. That is deliberate: a caller
  able to tell them apart could map a filesystem it may never see.
- **Receive hooks.** Set `device.receive_hook` to a program and it runs after
  each received file, with the saved path and the sender's device id as
  arguments. **Empty by default**, executed directly and **never through a
  shell**, so a file named `; rm -rf ~` is an argument rather than a command —
  which is also why it is one program and not a command line. A hook that
  fails, hangs or does not exist never affects the transfer that already
  completed.
- **`peerbeam snippet`** — pipe a log or a command's output straight into a
  conversation. Oversized input is truncated, not refused.
- **`peerbeam send --at <when>`** — wait until a time, then send. A delay rather
  than a scheduler, and it says so: the process must stay running.
- **An activity view** — one chronological list of transfers, conversations and
  clips, from Home or `peerbeam timeline`. It carries no message bodies and no
  clip text.
- **`peerbeam watch <dir> --to <peer>`** — send whatever lands in a folder. A
  file is sent only once it has stopped growing, so a copy still in progress is
  never delivered half-written.
- **Clipboard history — off by default, and a separate switch from sync.**
  Remembers your last 50 clips **on this device only**; nothing about history is
  ever sent to a peer. View or erase it in Settings → Clipboard history, or with
  `peerbeam clipboard history [--clear]`.

  When clipboard sync shipped it said there would be no history, "because a
  durable log of everything you ever copied is precisely what this feature must
  not create". That was about a log created as a *side effect of syncing* —
  automatic, unbounded, unasked — and it still holds: with this switch off,
  sync stores nothing. Turning sync on is explicitly not consent to record.

  Turning history off stops new entries but does not erase what was saved, so
  the erase action is separate and always available. Listings show one
  abbreviated line per clip: rendering fifty in full would put every remembered
  password on one screen.
- **Find my device.** Ring a device you trust from Home, or with
  `peerbeam ring <peer>`. The device shows a banner naming who is looking,
  wherever you are in the app — the reason it is being rung is that you cannot
  find it, so it is not hiding the alert on one tab.

  It rings only if it has granted your device the **Device status** permission,
  and never tells you either way: a caller told "refused" could map which
  devices are listening, and one told "it rang" could map which are awake.
- **Notes, synced to the devices you choose.** Text with a title, kept encrypted
  on this device — in the app (from Home), and as `peerbeam notes
  list|add|edit|remove|sync`. They sync to any device you grant the new **Notes**
  permission, automatically when it connects and on demand.

  "Your own devices" is that permission, made explicit: PeerBeam has no notion
  of *owning* a device, only of trusting one, so a device receives your notes
  because you said it may. Granting it to a colleague's laptop is possible and is
  your decision.

  Deleting leaves a tombstone rather than removing the row, so a deletion
  actually reaches your other devices instead of the note reappearing from one
  that had not seen it. Conflicting edits resolve last-writer-wins, with
  deletion winning a tie so a note you deleted does not come back.
- **Read receipts, off by default.** A message you sent shows a distinct marker
  once the peer tells you it read it. What it discloses is when *you* looked, so
  the setting ("Read receipts", in Settings) starts off and gates **sending
  only** — receipts other people send are always applied, so opting out never
  costs you what they choose to tell you. `peerbeam chat history --mark-read`
  is the CLI equivalent, opt-in on top of the opt-in: printing a conversation is
  not consent to report having read it.

  It is a watermark rather than one message per read: ids are time-ordered, so
  one id names the prefix that has been read. A repeat marks nothing new, and a
  stale watermark arriving late cannot move a read message back to unread.
- **React to a message** — an emoji on any message in a conversation, in the app
  (a control on the bubble opens a quick picker) and from the CLI
  (`peerbeam chat react <peer> <id> <emoji> [--remove]`). Reactions group by
  emoji with a count, and a chip you reacted with is outlined so tapping to
  withdraw aims at something visible.

  Add and remove are one wire message with a flag rather than a toggle: a toggle
  derives the new state from what the receiver believes the old one was, so one
  dropped or duplicated frame leaves two devices disagreeing forever. Stating
  the end state makes it idempotent.

  A reaction is applied to your own history whether or not the peer can be
  reached, and delivery is reported separately — an offline peer, or one whose
  build predates reactions, is told to you rather than silently implied to have
  seen it.

## [0.8.2] - 2026-08-18 — Beta

### Added
- **A native `arm64` CLI for Linux** (`peerbeam-linux-arm64`) — Raspberry Pi 4/5
  on 64-bit Raspberry Pi OS, Ampere and Graviton servers, Asahi Linux. Built on
  a native arm64 runner rather than cross-compiled.

### Fixed
- **Linux packaging derives the architecture instead of hardcoding it.** Four
  ecosystems spell it four ways — Flutter `x64`/`arm64`, dpkg `amd64`/`arm64`,
  rpm and AppImage `x86_64`/`aarch64` — so it is mapped once and every filename,
  the deb's `Architecture`, the spec's `BuildArch` and appimagetool's `ARCH`
  follow. x86_64 output is unchanged; an unrecognised architecture now fails
  loudly rather than producing a mislabelled package.
- **A templated artifact name no longer breaks the whole workflow.**
  `with: { name: ${{ matrix.artifact }}, … }` is invalid YAML — the expression's
  braces close the flow mapping — so the release workflow silently stopped
  parsing and GitHub stopped seeing its triggers. It would have simply not run
  on the next tag.

### Known limits
- **The Linux desktop app remains x86_64 only.** Google publishes no Linux
  arm64 Flutter SDK: all 730 Linux releases in its manifest are `x64` or
  unspecified, and an arm64 build fails at SDK setup. The CLI is plain Rust and
  has no such constraint. See [GUIDE.md](docs/GUIDE.md).


## [0.8.1] - 2026-08-18 — Beta

### Fixed
- **Fedora, RHEL, Rocky, Alma and openSUSE have a package for the first time.**
  `package-linux.sh`'s `.rpm` branch printed a message and built nothing, under
  a header claiming it produced one when `rpmbuild` was present. CI installs
  `rpm`, so it took that branch every release and shipped nothing.
- **An AppImage is built.** The branch was live but CI never installed
  `appimagetool`, so the artifact that runs on any distribution was never
  produced.
- **Linux packages carry the app icon.** CI installed `librsvg2-bin`, but the
  icon master is a PNG needing resizing and the script looks for
  `magick`/`convert`; every build warned "no rasterizer" and shipped a desktop
  entry pointing at a file that did not exist. Invisible because ImageMagick is
  present on a typical dev machine — local builds had all five sizes, CI's had
  none, and only CI's ship.
- **A manual release run no longer names the branch as the version.** The
  version came from `GITHUB_REF_NAME`, the *branch* under `workflow_dispatch`,
  so dpkg refused a control file reading `Version: main` and every manual run
  died there — before the rpm or AppImage steps were reached.

### Added
- `packaging/arch/PKGBUILD` for Arch and derivatives, building from the release
  tarball rather than from source.

## [0.8.0] - 2026-08-18 — Beta

### Added
- **Chat search: find a message in your own history.** Every surface can now
  search this device's stored conversations — the **text of messages** and the
  **names of shared files** — case-insensitively.

  **Local only, by construction.** It reads the same conversation namespaces
  history already reads and nothing else: nothing goes on the wire, no peer is
  dialled, and there is no way for a peer to observe that it happened. A thread
  whose device is long gone is searchable exactly like one sitting on the same
  desk; a conversation that was deleted is not searchable at all, because its
  rows are gone and there is nowhere for them to come back from.

  **A file's path on this device is never searched.** Its *name* is
  conversation content; where it happens to sit on disk is not, and matching
  that would surface a thread because of the name of a folder — every file
  anyone ever sent answering to `/home/<you>/Downloads`.

  **A truncated result set says so, everywhere.** The engine bounds the results
  by an explicit limit and reports when there were more. Every surface shows it:
  the app puts the notice *above* the first hit rather than under the last, the
  CLI prints a line, the FFI returns `truncated`. A bounded search that silently
  returns its first N reads as "that is all there is" — for a search over your
  own history that is a wrong answer, not a partial one.

  **The search is in the engine** (`ChatStore::search`), not a filter in the
  app. A filter in Dart would mean loading every message of every conversation
  across the FFI to answer one query, and writing it a third time for the CLI.
  It is a plain **substring** match, not a regex: a user-supplied pattern is an
  unbounded amount of work over an unbounded amount of history and nothing here
  needs one. Results are newest first, ties broken by peer id then message id so
  the order is stable between runs, and each hit names the conversation it was
  read from — the thread the message is actually in.

  Case-insensitivity is honest about its limits: Unicode per-character lowercase
  *mapping*, not full case folding (`ПРИВЕТ` finds `привет`; `ß` does not match
  `ss`). A row this build cannot decode is skipped rather than fatal, exactly as
  history skips it, so one row from a newer schema cannot make a conversation
  unsearchable or abort a search across all of them.

  **App:** a debounced search field on **Chats**, results grouped by
  conversation. Tapping one opens that thread — not that exact message, which
  the thread's lazily-built variable-height list cannot yet scroll to honestly.
  **CLI:** `peerbeam chat search <query> [--limit N]`, with `--json` returning a
  single object so `truncated` cannot be missed by a script reading the first N
  lines. Finding nothing exits `0`: an empty result set is a successful search.
  **FFI:** `pb_chat_search` — `{query, limit?}` → `{hits, truncated, limit}`.

  No wire change of any kind: no message type, no feature bit, nothing a peer
  can see.

## [0.7.0] - 2026-08-18 — Beta

### Added
- **Per-device permissions: what a trusted device may do, not just whether it is
  trusted.** Approval used to be one bit that meant *everything* — approving a
  laptop so it could receive files also handed it this machine's clipboard, its
  status heartbeat and an accepted pipe, with no way to say otherwise. There is
  now a permission per feature that exists today — **files**, **chat**,
  **clipboard**, **presence**, **pipe** — so "this laptop may sync files but must
  never read my clipboard" is expressible.

  **Settings → Trusted devices** shows one switch per permission under each
  approved device, each stating what granting it actually allows. A
  pinned-but-unapproved device is offered none: permissions narrow a standing the
  user granted and never create one.

  **CLI:** `peerbeam trust list` gains a `PERMISSIONS` column (and an explicit
  array under `--json`), plus `peerbeam trust permit <device> <permission>…` and
  `peerbeam trust revoke-permission <device> <permission>…`. Several names may be
  given at once and every one is parsed before anything is written, so a typo
  applies nothing instead of leaving a half-applied change. Both directions are
  idempotent, so provisioning scripts are safe to re-run.

  **Revoking applies to the next operation, not the next reconnect.** Every gate
  re-reads the trust store per message, clip, heartbeat and accept, so turning a
  switch off refuses the very next thing that device tries. Revoking `presence`
  also drops the status the dashboard already holds for it.

  **One predicate.** `TrustStore::may(device, permission)` is the only thing that
  reads the set, called as one more named leg by each of the five gates rather
  than checked at call sites. It implies approval — an unapproved device may
  nothing, whatever its bits say — and fails closed on a store error, exactly as
  `is_approved` does.

  **Upgrading an existing `trust.json` changes nothing about what works.** A
  record written before this field means *the permissions that existed when it
  was written*, so every device that worked before the upgrade keeps working;
  reading it as "none" would have silently cut off chat and transfers with no
  reason given. It is equally not read as "all": permissions are stored as fixed
  slots and the default enumerates the five that existed, so **a permission added
  in a later release is denied by default** — for legacy and newly approved
  devices alike — until explicitly granted. See
  [Security](docs/SECURITY.md#the-upgrade-rule).

  New engine export `pb_trust_set_permission`; `pb_trust_list` rows carry a
  `permissions` array. No wire change: permissions are local policy about a peer,
  never negotiated with it.
- **The app can tell a stranger from a familiar device.** An approval prompt for
  a device this one has never spoken to now says so, and shows the session's
  **pairing code** — a 128-bit safety number both devices derive from the keys
  they actually negotiated. Comparing it against the other device's screen is
  what detects a man-in-the-middle on first contact.

  The engine has derived that number on every session, and carried it to the app
  alongside `newly_trusted`, for several releases. Only the CLI ever acted on
  it: accepting a file from a total stranger took the same one tap as accepting
  one from a laptop used daily, with the one field that would have caught an
  interception sitting unread in the event payload. The number itself has not
  changed and nothing new crosses the wire.

  Both approval surfaces carry it — the Transfers card and a file offered in a
  conversation — and the code is shown **in full**, because all 128 bits are
  what make it expensive to forge.

  **Settings → Transfers → Verify new devices with a pairing code**
  (`require_pairing_confirmation`, off by default, the same setting the CLI
  reads) makes the check mandatory: a first-contact transfer cannot be accepted
  until the user confirms both screens show the same code. Nothing is
  pre-selected and dismissing the dialog counts as *not* confirmed. Cancelling
  accepts nothing and declines nothing — the transfer stays waiting, so going to
  read the other screen costs the user nothing.

  **The gate is in the engine, not in the UI.** `pb_transfer_accept` takes a
  `confirmed` flag and refuses an unconfirmed first-contact accept whoever asks,
  so the rule holds for every frontend rather than for one. Only a literal
  `true` satisfies it, so a caller that has never heard of this prompt cannot
  satisfy it by accident.

  **Declining a first-contact transfer un-pins the device**, matching what the
  CLI has always done. A removal that fails is reported as a failure rather than
  assumed: a pin left behind while the app claimed otherwise would mean the next
  connection was not "new" at all, skipping the check silently.

  What this proves is bounded, and [SECURITY.md](docs/SECURITY.md#what-a-confirmed-code-proves-and-what-it-does-not)
  says so: the app displays a code and records what the user said, it cannot see
  the other screen, and an unanswered prompt un-pins nothing.

  With the setting off — the default — every accept behaves exactly as it did
  before.

- **Resumable transfers, surfaced.** A transfer interrupted by a dropped link or
  by the app closing is no longer lost. It comes back as an **interrupted**
  transfer — in the Transfers screen, in `peerbeam transfers`, and over the FFI —
  showing how far it got, and it can be resumed from there or discarded.

  The recovery engine itself is not new: `send_file_recover`, the
  `ReliabilityStore` port and its filesystem implementation have been in the
  engine, tested, for several releases. Nothing called them. Sends now run
  through the recovery driver and receives persist their own checkpoint, so the
  capability finally reaches a user.

  ```bash
  peerbeam transfers list             # ID, DIR, PEER, FILE, PROGRESS, AGE
  peerbeam transfers resume tx-4131-0
  peerbeam transfers discard fileref-7
  ```

  **A checkpoint binds to its transfer.** Resuming appends to a partial file, so
  before a byte moves the checkpoint is checked against direction, the persisted
  consent flag, the peer id, the file name and the total size — the last of
  those against the source file *as it is on disk now*, not against what the
  checkpoint remembers. A source that has been replaced or resized is refused: a
  resume that appended its bytes to a receiver's prefix of the old contents
  would build a file that never existed anywhere. The end-of-transfer checksum
  still runs on a resumed transfer, so one that does not verify fails loudly
  rather than landing.

  **Consent is remembered, and does not spread.** An inbound transfer the user
  already accepted resumes without a second prompt — being asked twice for one
  file because the Wi-Fi dropped is exactly the friction this removes. It grants
  nothing on its own: a transfer that was declined, that timed out, or that
  nobody answered leaves no checkpoint at all, so a later offer of the same id
  meets the ordinary approval prompt. An interruption never turns an unanswered
  prompt into an approval (I6). A checkpoint missing the consent field reads as
  "not accepted".

  **An interrupted receive resumes when its sender offers it again**, not on
  demand. Resume is the existing transfer protocol's own mechanism — no new
  frame, no new channel — and that protocol is sender-driven, so the receiving
  side has no way to pull. Such a transfer keeps its partial file, its progress
  and its consent, and the surfaces say "waiting for sender" rather than
  offering a Resume that would do nothing.

  **A resumed receive lands where it was already landing**, read from its own
  checkpoint rather than re-derived from the current save directory and rules. A
  user who changed their save folder while a transfer was interrupted would
  otherwise get the second half in the new folder, the first half stranded in
  the old one, and a transfer that silently restarted from zero.

  **Checkpoints are disposable, two ways.** Explicitly — discarding one drops the
  record and the partial file together (never the *source* of a send: giving up
  on a send is not permission to delete your file) — and by age: 14 days from
  the last attempt, swept at startup along with the partial file. Long enough
  for a laptop shut over a holiday, short enough that an abandoned 40 GB
  transfer is not still pinning its bytes a year later. A resume restarts the
  clock, so a transfer you keep retrying never ages out.

  New FFI exports, all additive (ABI stays 1): `pb_transfers_interrupted`,
  `pb_transfer_resume_interrupted`, `pb_transfer_discard_interrupted`, plus the
  `transfer_interrupted` and `transfer_discarded` events. **`pb_transfer_resume`
  is unchanged and still means "un-pause a live transfer"** — the two verbs are
  deliberately two different calls, because one name for both is how a surface
  ends up calling the wrong one.

  `peerbeam transfers` keeps its existing `sessions`/`transport` output and gains
  an `interrupted` array beside it; `peerbeam send` deliberately writes no
  checkpoint of its own, since re-running a foreground send already resumes from
  the receiver's partial file.

  Completes the last substantive gap in Phase A (`ROADMAP.md`: "Resumable
  transfers surfaced in the UI").


## [0.6.0] - 2026-08-18 — Beta

### Added
- **Rules-based auto-save for received items.** An ordered list of rules that
  choose **where** an accepted file is written:

  ```bash
  peerbeam rules add /srv/papers --ext pdf
  peerbeam rules add /mnt/big --from laptop --min-bytes 1073741824 --at 0
  peerbeam rules list
  ```

  **A rule decides where a file is saved. A rule never decides whether it is
  accepted.** Rules are consulted *after* a transfer has been accepted and is
  on its way to disk. Nothing here is reachable from the approval path, no
  rule field influences it, and the existing `device.auto_accept_trusted`
  setting is untouched (I6). If you want to change what is accepted, that is
  still the approval prompt and auto-accept.

  A rule is a **match** — any combination of sending device, file extension
  and a size range — plus an absolute **destination** directory. Every
  criterion is optional and an omitted one matches everything, so a rule with
  none is a legitimate catch-all. **The first rule that matches wins**, and
  there is deliberately no specificity score: a list you can reorder is a list
  whose outcome you can predict, whereas a ranking nobody can see is a support
  question waiting to happen. Reordering (`rules add --at`, or dragging in the
  app) is the supported way to change which of two overlapping rules applies.

  **Nothing changes for anyone who defines no rules.** An empty list — which is
  what every existing config file and settings document deserializes to —
  means every received file goes to `storage.save_directory` exactly as
  before.

  The sender criterion matches the **authenticated device id**, never the name
  a peer presents: a name is peer-supplied, so matching on one would hand a
  stranger calling itself "laptop" the laptop's destination. The CLI's
  `--from` resolves a name through the same resolver `send --to` uses and
  stores the resolved id; the app offers a dropdown of known devices rather
  than a text field. Extension matching is on the **sanitised** name, and the
  peer contributes no part of the destination.

  **A rule is validated when it is added**, not when a file arrives: absolute
  path, no `..` component, and a parent that already exists — reported then,
  while the person can still fix it. **A destination that fails anyway does not
  lose the file**: it falls back to `storage.save_directory` *and says so*, on
  the same channel every other transfer failure uses (`rule_fallback` in the
  CLI's `--json`, `transfer_save_fallback` on the app's event stream). A file
  quietly landing somewhere other than where the rules claimed is worse than
  having no rules at all.

  Surfaces: `peerbeam rules list|add|remove` (with `--json`, the usual exit
  codes, and `--at` for position); an *Auto-save rules* section in desktop
  Settings that views, adds, **reorders** and removes them; `pb_rules_set` on
  the FFI, with reads through `pb_settings_get` (`save_rules`, plus a managed
  `rules_supported`). `receive --dir DIR` names a destination for that run
  explicitly and turns rules off for it.

  **Desktop and headless only.** Android receives into a SAF-granted location
  and cannot write to arbitrary absolute paths, so rules do not apply there;
  the app says why instead of offering a list that would silently do nothing,
  and `pb_rules_set` refuses with a new `unsupported` error code —
  deliberately distinct from `unimplemented`, which would tell someone to wait
  for a build that is never coming (I12).

  Storage is additive: `storage.rules` in `peerbeam-config`, absent-tolerant,
  so every config file and settings document already on disk keeps working.

## [0.5.0] - 2026-08-18 — Beta

### Added
- **`peerbeam pipe` — an encrypted byte pipe between two devices.** stdin on one
  side, stdout on the other, over a new negotiated Pipe channel (`0x0107`):

  ```bash
  tar cz ./project | peerbeam pipe --to laptop
  peerbeam pipe --listen > project.tgz
  ```

  No temp file, no filename, no length. The stream is **binary-safe** —
  nothing inspects, decodes or line-buffers the bytes, so `tar`, `gzip`, `dd`
  and arbitrary binary survive intact — and **never held whole**: one chunk
  per direction at a time, so a 40 GB pipe runs at flat memory on both ends.
  EOF is the terminator: closing stdin ends the stream, the receiver flushes
  stdout and exits `0`.

  **stdout carries the piped bytes and nothing else.** Every human-facing
  line — the listening address, the peer, refusals, `--json` events — goes to
  stderr, in both directions, or `peerbeam pipe --listen > project.tgz` would
  write status text into the archive.

  **The consent model is deliberately not file transfer's.** A pipe is
  accepted **only** by a process the user explicitly started with `peerbeam
  pipe --listen` — a running `receive`, `daemon start` or `chat watch`, and
  the desktop app, all refuse every pipe offered to them. Running the command
  *is* the approval, which is why there is no prompt and must not be one: a
  prompt reads stdin, stdin is the payload, and it would break the scripted
  headless use the feature exists for. The second gate is the familiar one —
  **trusted peers only, not configurable** — narrowable to a single device
  with `--from`, matched against the authenticated device id and never
  against the name a peer presents. **One stream, then exit**; a refused
  attempt does not count, so a stranger cannot end a listener by dialling it.
  See `docs/SECURITY.md` for why this differs from the transfer prompt.

  A peer whose build predates the feature is refused **before a byte of stdin
  is read**, naming the peer and the reason, rather than being streamed bytes
  it would drop. A truncated or corrupt stream is an **error, not a clean
  end** — the bytes are already on stdout by then, so the non-zero exit code
  is the only report, and a script that ignores it will trust a bad file.

  CLI-first: there is no pipe UI in the app.
- **Automatic clipboard sync between trusted devices.** Copy on one desktop and
  it is on your other devices, over a new negotiated Clipboard channel
  (`0x0102`, `Clip`). No pairing step, no button — the desktop watcher notices
  the change and pushes it.

  **Opt-in and off by default**, and a clip is **never sent to a device that is
  not trusted** — that second gate is not configurable. Turning the setting off,
  or revoking trust, stops the *next* clip rather than the next reconnect. A
  device with sync off still receives and applies what its peers send, which is
  what makes it an opt-in rather than a mutual requirement.

  **Everything you copy is sent, passwords included, and the toggle says so.**
  There is deliberately no password detection: a clipboard read carries no
  sensitivity signal on any platform PeerBeam supports — Flutter's API has none
  and X11/Wayland define none — so a heuristic would be a guess, and wrong in
  either direction is bad. Guessing "secret" drops clips you expected; guessing
  "safe" ships a credential while the UI implies something was checked, which is
  worse than never claiming to check. The honest warning is the feature. See
  `docs/SECURITY.md`.

  **Desktop sends, every platform receives.** Android 10+ forbids reading the
  clipboard from the background, so a phone can never auto-send; it says so
  rather than offering a toggle that mysteriously works one way. The watcher
  refuses to run off desktop.

  A clip received from a peer is **never sent back** — without that guard two
  devices ping-pong a single copy forever — and unchanged content is never
  re-sent. Whatever is already on the clipboard when you flip the toggle is not
  synced: that is not something you just copied, and may well be a password you
  pasted earlier. Sync starts with the next copy.

  A received clip is written to the clipboard and announced with a toast naming
  the device that sent it, never repeating the content. Nothing is persisted:
  there is no clipboard history, because a durable log of everything you ever
  copied is precisely what this feature must not create.

  `Clip.text` is capped at 64 KiB, a frozen wire constant. An over-cap clip is
  **skipped and reported, never truncated** — a shortened clipboard silently
  corrupts what you believe you copied. An empty clip is refused too: applying
  one would erase the peer's clipboard.

  The CLI is deliberately unchanged: `send --clipboard` remains its manual path
  and it gains no watcher, because watching needs a system-clipboard adapter the
  Rust workspace does not have (and a headless server has no clipboard to sync).
  It still receives, printing one line naming the sender and the size — never
  the contents. See `docs/CLI.md`.
- **Device presence and a Devices dashboard.** Trusted devices can share a live
  status — battery, charging, free storage, network kind, app version — over a
  new negotiated Presence channel (`0x0103`, `Status`), sent when the channel
  opens and every 60s while it stays open. A new **Devices** destination lists
  every discovered device with whatever it chose to share.

  **Sharing is opt-in and off by default**, and status is **never sent to a
  device that is not trusted** — that second gate is not configurable. Battery
  level, free disk and network kind are a device fingerprint, so they go to the
  user's own pinned devices or nowhere. Turning the setting off, or revoking
  trust, stops the next heartbeat rather than the next reconnect. A device with
  sharing off still receives and displays everyone else's status, which is what
  makes it an opt-in rather than a mutual requirement.

  Absence is rendered as absence. A device that shared nothing, a field it
  could not measure, and a reading that has not arrived yet are three different
  things and none of them is zero — a desktop has no battery, and the
  Windows/macOS battery collector is deliberately not implemented rather than
  pulling in a dependency for it. Nothing is persisted: presence is live state,
  so a cold start says "status not shared" instead of presenting yesterday's
  battery level as current.

  Peer-supplied values are validated, not trusted: a `battery_percent` over 100
  discards the whole message rather than clamping it, and a `network` word
  outside the closed vocabulary is dropped rather than reaching the UI verbatim.
- **`peerbeam trust list|approve|revoke` — approving a device from a shell.**

  ```
  STATUS    DEVICE           NAME          FINGERPRINT          PINNED
  pinned    pb-91ab33cd1122  Unknown Peer  77b2ccddeeff0011…    2026-08-18 02:11
  approved  pb-f4e4d56fce98  laptop        3f9a1b2c4d5e6f70…    2026-08-17 10:30
  ```

  The listing's first column is the point. A device is **pinned** by the
  handshake the first time it connects — that records its key so a later change
  is detectable, and nothing more — while **approved** is set only by a person,
  and is what lets a device receive this machine's presence status, clipboard,
  or a `pipe --listen`. Approving had been reachable only from the desktop app,
  so a headless server or a container could not use any of the three at all.

  `approve` prints the full fingerprint it is approving and asks; `--yes` (or
  `--json`) proceeds without prompting, and without either — with no terminal
  to ask at — it refuses rather than approving unasked. Approving something
  already approved is a no-op that says so and exits `0`, so a provisioning
  script is safe to re-run. `revoke` removes the whole record, not just the
  flag, so the next connection is a fresh first contact.

  `<device>` resolves the way `send --to` does (exact id, exact name, unique
  name prefix); an ambiguous prefix lists the candidates and exits `2` instead
  of guessing. `--json` emits one object per line, `approved` an explicit bool.

### Security
- **Presence, clipboard sync and `pipe --listen` now require an *approved*
  device, not merely a pinned one.** All three asked `TrustStore::is_trusted`,
  which is "is there a record" — and PeerBeam's handshake is TOFU, so it
  records *every* never-seen peer as it connects, with `approved: false`, purely
  so that a later key change is detectable. `is_trusted` was therefore already
  true for any stranger on the LAN by the time the gate was asked, which made
  "trusted devices only" — the leg that is deliberately not configurable on any
  of the three — very nearly vacuous.

  In practice a stranger who completed one handshake was sent this device's
  battery level and free disk on every heartbeat, was sent the contents of
  every clipboard copy, and could write arbitrary bytes onto a listening
  terminal's stdout. Only `pipe`'s "you must be running `--listen`" gate and
  `--from` carried any weight against a first-contact peer; presence and
  clipboard had no second leg at all.

  The three gates now ask a new `TrustStore::is_approved`, which only the user's
  explicit accept-and-trust sets, and which fails **closed**. `is_trusted` keeps
  its meaning and its callers — "is this the key I saw before?" is a real and
  separate question. Each gate has a test in which the peer is pinned and the
  gate is still shut, and `peerbeam-cli/tests/pipe_e2e.rs` proves it across two
  real processes.

  **Upgrade note:** a device that was working over presence, clipboard or pipe
  purely on the strength of a pin will stop until it is approved — in the app's
  Trusted Devices, or with `peerbeam trust approve <device>`. That is the fix
  working. Devices already approved through an accept-and-trust are unaffected.

### Fixed
- **A sandboxed FFI test no longer fights other PeerBeam processes for the
  discovery port.** `peerbeam-discovery-udp`'s socket binds with
  `SO_REUSEPORT`, so more than one PeerBeam process on the same machine can
  bind the well-known discovery port (`49500`) at once; for a **unicast**
  datagram the kernel then load-balances by hash across every bound socket,
  delivering it to whichever process the hash happens to pick. Three tests in
  `rust/crates/peerbeam-ffi/tests/chat_ffi.rs` simulate a peer coming online
  by firing such a datagram straight at that port (no real LAN broadcast in a
  sandbox), so on a machine already running a PeerBeam GUI or CLI it landed on
  the wrong process roughly half the time and the drain never fired — a
  genuinely broken test and this flake were indistinguishable. `UdpDiscovery`
  already supported a configurable port (`Config::port` / `bound_port()`);
  the gap was `peerbeam-ffi`, which hardcoded the default. `peerbeam-config`'s
  `DiscoveryConfig` gains a `port` field (default unchanged), the FFI runtime
  now constructs `UdpDiscovery` with it via `with_config`, and
  `pb_discovery_start`'s result additively carries the port actually bound.
  The affected tests now request an OS-assigned port and send their announce
  there instead, making them immune to whatever else is running on the host.
- **Android: picking a second batch of files no longer destroys the first
  batch's bytes while they were still in use.** The native picker streams
  every pick into `cacheDir/picked`, and `preparePickedDir` wiped that whole
  directory at the start of each new pick on the (false) assumption that a
  prior batch was always already handed off by then. It wasn't: the Send
  flow only reads a staged file's path back when the user finally taps
  Send, which they may put off indefinitely, and a chat attachment's staging
  copy keeps reading its source for as long as the copy takes — minutes, for
  a large file — unawaited in the background. Picking again from inside the
  staged sheet, or attaching again mid-copy, deleted the earlier batch out
  from under whichever of those was still reading it. `preparePickedDir` now
  gives each pick batch its own subdirectory and prunes only by age (a day,
  not the hour share-in uses, since a pick can sit staged far longer than a
  share ever does), and the Dart side additionally tells it which paths are
  still staged (`keep`) so a batch left in the sheet past that cutoff is
  never pruned regardless of age.

## [0.4.1] - 2026-08-17 — Beta

### Fixed
- **A conversation you had navigated away from no longer owns drag & drop.**
  Two drop targets contend for every drop on desktop — `desktop_drop` delivers
  one drop to *every* mounted target, and the Send flow's `DropZone` wraps the
  whole shell — which is arbitrated by a claim register the open conversation
  holds while the Send zone stands down. That claim was scoped to the chat
  screen being *mounted*, but the shell keeps every navigation branch mounted
  and merely takes the inactive ones offstage. So opening a conversation and
  then tapping **Home** left the conversation owning drops: a file dropped on
  Home was sent straight to that peer instead of being staged for the Send
  flow, with no prompt and no route to the Send sheet. The claim is now scoped
  to **visible**, and the conversation's own drop target is disabled and its
  handler refuses independently of the claim.
- **Resizing the window across 600px no longer strands the drop claim.** The
  shell puts its `DropZone` in a different slot on either side of that width,
  so crossing it destroys the claim register while the open conversation
  survives. The conversation went on holding a register nobody read: a drop was
  then answered twice — sent to the peer *and* staged for the Send flow — and
  closing the conversation afterwards threw. The register is now re-resolved on
  every reconcile, and one that has been replaced is dropped rather than
  released.
- **A selection that partly went stale no longer resurrects on an id reuse.**
  The chat screen narrowed its selection for rendering but never pruned it, so
  a set that lost only *some* of its messages kept naming the dead ids. An
  inbound file's message id is the sender's own `FileRef` id — a value the peer
  chooses — so a peer reusing one had its message rendered as already selected,
  counted, and forwarded to a third device. Stale ids are now pruned.

## [0.4.0] - 2026-08-17 — Beta

### Added
- **Select messages in a conversation, then forward or delete them.** A
  long-press on a bubble starts a selection (right-click on desktop, where a
  long-press with a mouse is nobody's idiom); a tap toggles; the app bar becomes
  × / `N selected` / **Forward** / **Delete**. Back leaves the selection before
  it leaves the conversation, and the selection survives the rebuilds that
  incoming messages and staging progress cause constantly.
  **Delete** is local only and reports the engine's own answer rather than the
  request — `Deleted 2 messages · 1 kept because it is still being sent` — so a
  message the engine refused to take (its record is what will deliver a queued
  file) is explained rather than left mysteriously on screen.
  **Forward** opens the same device picker the Send flow uses and re-sends each
  message in thread order through the existing send paths, text as text and a
  file as a file. A file whose bytes are no longer on this device is excluded
  *before* anything is sent and named ("`invoice.pdf` isn't on this device any
  more"), rather than handed to the engine to fail one message at a time.
- **Delete individual chat messages** (`pb_chat_delete_messages`,
  `{peer_id, message_ids:[…]}` → `{removed, kept:[…]}`). Local only, like
  deleting a whole conversation: nothing goes on the wire and the peer keeps its
  own copy — this is not "unsend". Anything still waiting to be sent survives,
  queue entry and staged bytes included, because the engine's drain reads a
  *missing* record as "nothing will ever settle this" and would throw the queued
  file away; the reply **names** the ids it kept, so a surface can say which of
  the picked messages are still on their way out instead of claiming to have
  deleted them. Deleting a conversation and deleting a selection now answer to
  one shared implementation of that keep rule, rather than two that could drift
  apart. An unreadable outbox entry refuses the whole call with
  `queue_unreadable`, exactly as a conversation delete does.
- **A typed attach menu in chat.** The composer's attach button now opens a
  menu of **Document**, **Photos & videos**, or **Audio** instead of one
  undifferentiated picker — matching the shape WhatsApp and every other chat
  app use. Choosing a kind filters what the picker even offers: a file the
  filter excludes is never shown, rather than picked and then rejected.
  Desktop filters by both MIME type and extension (Windows reads extensions
  only, and macOS drops a wildcard MIME, so either list alone offers nothing on
  one of them — Linux honours both); Android sets
  `EXTRA_MIME_TYPES` on the picker intent, with the argument optional so
  nothing else that calls the native picker needs to change. Everything past
  the choice is unchanged: the picker is still multi-select, and every picked
  file still gets its own message.
- **Drag & drop into a conversation.** Dropping files onto an open chat sends
  them straight to that peer, the same per-file fan-out attach uses, with the
  same dashed drop overlay as the Send flow. Desktop only — a transparent
  passthrough on mobile. A dropped folder is refused rather than silently
  dropped or left to surface as a later engine error: a chat file message
  carries one file, so the folder is named in a message pointing at "Send
  folder" on Home. A drop mixing files and folders still sends the files and
  reports every folder it had to skip, in one message.
- **Approve several waiting transfers at once** — when two or more inbound
  transfers are awaiting approval, the Transfers screen shows a banner with
  **Accept all** / **Decline all**, so a batch takes one tap instead of one per
  card. Below two it stays hidden (a single card's own Accept is already the
  shortest path), and every card keeps its own Decline / Accept / Trust.
  The bulk action is **accept-once and never trusts** — granting a device
  persistent auto-accept stays a deliberate per-device choice, so there is no
  "Trust all" — and it reaches only inbound transfers still awaiting a
  decision, never an outbound send or one already in flight. It reports a
  verified tally rather than an assumed one ("Accepted 3 items", "Accepted 3 of
  5 — 2 were no longer waiting", "None were still waiting"), because transfers
  can time out or be answered from their own card between the render and the
  tap. The banner counts **items**, not files: a waiting transfer can be a
  whole folder.
- **Answer just the transfers you pick** — the same banner's **Select** hands
  you a checkbox per waiting card and switches to `N of M selected` with
  **Decline** / **Accept** / **Cancel**, for the common case where a batch is
  not all-or-nothing. It is the same accept-once decision as **Accept all**:
  `accept`, never `acceptTrust`, and **Trust** stays a single per-device action
  on the card — there is no "Trust selected". A checked transfer that stops
  waiting drops out of the count and is never handed to the engine; selection
  mode ends when the batch lands, is cancelled, or when nothing is left waiting.

### Fixed
- **Deleting a conversation no longer destroys a file that is still being
  staged.** Attaching a file writes its row immediately and copies the bytes in
  the background — minutes, for a multi-GB file. A delete landing in that window
  removed the row, and the finished copy then queued an entry with nothing
  behind it: the next delivery attempt offered the file to the peer and only
  then threw the entry and the staged bytes away. The attachment vanished from
  the thread and the peer was left with an approval prompt for a file that never
  arrived. A file still being staged is now kept, exactly as the confirmation
  promises, and the residual race leaves nothing behind rather than an orphan.
- **A file you declined no longer keeps its conversation undeletable.** A
  refusal queued for a peer that had already gone offline held the declined
  message's row alive, so deleting the thread reported it as "1 queued message
  was kept and will still be sent" and left the thread listed until that peer
  came back. The refusal itself is still delivered when they do.
- **The delete report counts every row it kept**, including one written by a
  newer version this build cannot read — previously reported as nothing kept
  while the thread stayed listed.
- **A conversation that refuses to delete now says why.** The shared send
  queue is checked as a whole before any delete proceeds, and if even one
  entry in it — belonging to any peer, not necessarily the conversation being
  deleted — cannot be read, the delete is refused rather than risk destroying
  the record behind someone's still-queued file. That refusal used to surface
  as "Something went wrong. Please try again," advice that could never be
  followed to a fix, since retrying never touches the unreadable entry. It now
  carries its own error and its own message, explaining that something still
  queued to send can't be read right now, so deleting is on hold.

## [0.3.0] - 2026-08-08 — Beta

The **PeerSession** release. The transfer stack was rebuilt on a single
multiplexed, authenticated session abstraction
(`Peer → PeerSession → Typed Message → Handler → Engine`). Every transfer now
runs as a cryptographically isolated channel over one QUIC session, with
reconnect/resume built in. The old direct-link transport and its
migration/compatibility layer are gone — PeerSession is the only transport.

### Added
- **PeerSession foundation** — a multiplexed session over QUIC: N independent
  channels per session, each a typed control or stream channel (M1–M3).
- **Per-channel encryption** — every channel derives its own key via HKDF, with
  independent counters and replay protection, so concurrent transfers never
  share a nonce (M4).
- **Transfer as a channel** — file and folder transfers run on a dedicated
  sealed transfer channel, reusing the transfer engine unchanged; a failed or
  cancelled transfer can no longer stall the session or its siblings (M5).
- **Reconnect + resume** — a session survives a dropped connection: it
  reconnects, bumps its epoch, and reattaches its channels with message
  continuity (M6).
- **PeerSession diagnostics** — live session/channel/transport/recovery
  snapshots exposed over the FFI and the CLI (`session`, `channels`,
  `transfers`, `recovery`, `diagnostics`) (M8–M9).

### Changed
- **CLI and FFI transfers execute over PeerSession** — both frontends dial /
  accept a PeerSession and run transfers as channels; the Flutter app follows
  via the FFI (M7–M9).
- **Diagnostics report the permanent runtime** — the former migration metrics
  endpoint now reports the live transport summary
  (`transport`/`active_sessions`/`recovering`); the API surface (FFI symbol +
  CLI command) is unchanged.
- **Unified monochrome brand mark** across launcher icons, packaging, README
  banner, and GitHub social preview.

### Removed
- **Migration / cutover / compatibility layer** — the dual-path selector,
  `CompatMode`, fallback machinery, and migration-only metrics were deleted.
  PeerSession is the sole send/receive/recovery path (M9).
- FFI migration event kinds (`FALLBACK_TRIGGERED`, `MIGRATION_STATS_UPDATED`).

## [0.2.4] - 2026-07-16 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.4.md).

### Fixed
- **macOS GUI now launches** — the app loaded the engine
  (`libpeerbeam_ffi.dylib`) by bare name, which macOS `dlopen` never resolves
  from an app bundle, and nothing built or embedded the dylib in the `.app`.
  The engine is now built universal (x86_64 + arm64), embedded in
  `Contents/Frameworks` with an `@rpath` install id, and loaded by an
  executable-relative path. One DMG runs on Intel and Apple Silicon. The DMG is
  attached to the release (unsigned/un-notarized until signing secrets exist).

## [0.2.3] - 2026-07-16 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.3.md). The stability release: two
adversarial audits (42 confirmed bugs fixed), an Android storage overhaul,
cooperative pause, and stacking selection.

### Added
- **Stacking selection** (LocalSend-style): build one selection from files,
  folders, text, and clipboard, review/edit it, then send the whole batch to a
  device in one go. Tapping a device now sends the current selection (empty
  selection → pick files, as before). A persistent bar on Home shows the count
  + total with a one-tap Send.
- **Cooperative pause**: either side pauses and both stop and show paused, with
  correct speed/ETA after resume.
- **Explicit Trust button** — accepting a transfer no longer auto-trusts the
  sender; auto-accept requires explicit approval, not just a pinned key.
- **Android notifications** for received files and send complete/failed, with an
  animated-while-transferring status-bar icon and a static idle brand glyph.

### Fixed
- **Received transfers show the sender's name, not a raw id** — the auth
  handshake dropped the peer's human name, so History/Transfers read
  "Received from app-12345" (the internal device id). The name now flows through
  the session and into history + events.
- **Android: received files are visible again** — they previously landed in an
  app-private dir hidden by scoped storage (invisible in Files/Gallery, and the
  engine data dir wasn't durable). Now received files default to public
  **Downloads/PeerBeam** (via MediaStore — visible in Files, images in Gallery,
  no permission prompt), or a **folder you pick** (Storage Access Framework);
  engine data (trust/history/settings) moved to the persistent app support dir.
- **Changing "Save to" takes effect immediately** — the receive directory was
  captured at startup, so a new folder (and the auto-accept toggle) only applied
  after a restart, and History reported a path the file wasn't at. Both now apply
  live to the running engine.
- **Large files no longer crash the app** — picked and shared files are streamed
  to cache instead of loaded into memory.
- **Transfer correctness**: cancel interrupts a parked receive and fires exactly
  one terminal event; abandoned incoming transfers are time-bounded (no stuck
  "1 transfer in progress"); a corrupt `.part` heals on checksum failure; folder
  receive overwrites instead of blind-appending; folder send skips unreadable
  files; receiver-side pause actually pauses (file + folder) with no lost wakeups.
- **The device list no longer freezes** under a broadcast-lag burst — the engine
  emits a resync hint and the app re-pulls the authoritative device list. Offline
  devices are pruned, DNS is non-blocking, the trust store merges instead of
  clobbering, config loads missing fields as defaults, init is idempotent, and
  Tailscale peers are dialable (transfer port stamped on discovery).
- **CLI**: `chunk_size` clamped (no u32 truncation), `watch` shows all device
  events, honest daemon hints, `benchmark` cleans up temp files on error.
- **UI**: stable keys on transfer rows (no backward-animating progress bars),
  inline validation on the address dialog, leaked text controllers disposed,
  partial-batch sends report what failed, the Nearby picker only lists reachable
  devices, Android back returns to the Home tab before exiting, re-shares
  coalesce into the open sheet, and the brand mark is announced once by screen
  readers.
- **Removed the non-functional "Compression" toggle** — it was never wired to the
  transfer path, so it did nothing even after a restart.

### Changed
- In-app logo is a monochrome brand glyph tinted to the app's primary colour at
  runtime — matches the theme and stays visible in both light (deep purple) and
  dark (light purple); the earlier transparent mark's white paper-plane washed
  out on light surfaces. Window title reads "PeerBeam" (proper case) on Linux,
  macOS, and Windows.
- Removed the duplicate "PeerBeam" heading on Home — the nav rail (or, on
  phones, the app bar) carries the brand once. Dropped the example
  name/IP placeholders in the add-device and send-to-address dialogs.
- Android `versionCode` is now monotonic (time-based), so any build installs
  over any previous one without a downgrade block.

## [0.2.2] - 2026-07-15 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.2.md).

### Added
- **CLI folder transfer**: `peerbeam send <dir>` streams whole folders, and
  `receive` dispatches folder transfers (previously file-only on both sides).
- **CLI clipboard + history**: `peerbeam clipboard send` (argument, stdin, or
  system clipboard; same wire convention as the app, so receivers offer Copy),
  `clipboard get` (prints the newest received text), and `peerbeam history`
  (persisted, `--limit`/`--clear`, human or NDJSON). Both were gated stubs.
- **Settings persist** and reach the engine: device name, save directory,
  auto-accept, theme, and toggles survive restarts and apply at init.
- **Transfer history persists** across restarts (bounded to the most recent
  500); Clear now clears engine-side too.
- **Auto-retry**: transient connect failures retry twice with backoff before
  failing.
- **Trusted devices**: Settings lists every pinned device with its key
  fingerprint; revoke to require fresh approval on the next connection.
- **Edit saved devices**: rename or re-address a saved device from its menu
  (share QR / edit / remove).
- **Open the save folder** from Settings (desktop).
- **Open from History**: history entries record the item's local path; tap to
  open the file (or the save folder for folder receives) with the OS handler.
- **Clipboard receive**: a received clipboard payload shows a snackbar with the
  sender, a preview, and one-tap **Copy** — clipboard to clipboard.
- **Android share sheet**: sharing files/text to PeerBeam now completes the
  flow — files open the staged sheet, text offers one-tap send.
- **Send folders**: desktop folder picker + folder drag-and-drop; staged
  batches split into file and folder transfers automatically.
- **Receiver-confirmed progress**: the sender's bar tracks the receiver's real
  byte count over a dedicated QUIC back-channel (falls back to bytes-sent for
  old/non-QUIC peers); 64 KiB chunks + throttled emission for smooth movement,
  a 1s heartbeat so speed/ETA keep ticking, and speed/ETA shown on transfer
  cards.
- **Android file picking**: the Send files action uses the native picker on
  every platform (no more desktop-only gate).
- **Unified destination picker**: one sheet with Nearby + Saved sections for
  file and clipboard sends — saved (Tailscale/by-address) peers are now
  reachable from the phone flows.
- **Send clipboard**: sends the OS text clipboard to a chosen device as a
  `.txt`.
- **QR**: share a saved device as a `peerbeam://` QR; scan one to add it
  (mobile camera).
- **Device search** on Home.

### Changed
- **UI overhaul**: stock Material 3 look (baseline seed), Google Sans Flex
  typeface (bundled, OFL), flat tonal components, one hero send action,
  sentence-case terse copy, seamless app bars, responsive polish. See
  `docs/UI_REDESIGN_REPORT.md`.
- **Nearby devices show live peers only** — offline devices disappear instead
  of lingering greyed out; the Scan/Stop control reflects the engine state
  from boot.

### Fixed
- Tailscale-discovered peers are now reachable: they were stamped with port 0
  ('not reachable right now' on send) because `tailscale status` reports only
  tailnet IPs. Both frontends now stamp the configured transfer port on
  Tailscale peers. Live-verified desktop -> phone over Tailscale.
- Windows GUI no longer flashes a console window on every discovery tick
  (the Tailscale status probe now spawns with CREATE_NO_WINDOW).
- Folder transfers no longer silently drop zero-byte files (both the send-side
  resume skip and the receiver's completed-count treated `0 >= 0` as "already
  transferred").
- A config file from an older or newer version now loads (missing fields fall
  back to defaults) instead of failing to parse; corrupt values still error.
- Cancelling a transfer takes effect immediately, even mid-chunk on a slow
  link.
- Dialing an unreachable peer fails in ~8s with a clear error instead of a
  silent 30s hang.
- A dead peer no longer stays "online" via a stale mDNS cache claim after an
  unclean exit; stopping discovery marks all devices offline instead of
  freezing stale presence.
- Fast transfers always emit a final progress update (fixed a flaky FFI test).

## [0.2.1] - 2026-07-14 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.1.md).

### Added
- The standalone `peerbeam` **CLI now ships in releases** for Linux, macOS
  (arm64), and Windows, with Linux shell completions. Dedicated `cli-*` CI jobs
  build them; the release attaches them alongside the Linux app + Android.
- Local signing how-tos for macOS (Developer ID + notarytool) and Windows
  (MSIX) in [RELEASE](docs/RELEASE.md); `msix_config.publisher` field.

### Note
- Signed macOS/Windows desktop apps (DMG/MSIX) are still not attached to
  releases until host signing secrets are configured.

## [0.2.0] - 2026-07-14 — Beta

First tagged release. See [Release Notes](docs/RELEASE_NOTES_v0.2.0.md).

### Added
- Branding: PeerBeam logo across all platform icons + README banner.
- `LICENSE` (full AGPL-3.0-or-later), Code of Conduct, security policy,
  supported-versions policy, issue/PR templates, dependabot.
- Continuous integration (`.github/workflows/ci.yml`): fmt, clippy
  (`-D warnings`), `cargo test`, examples build, `flutter analyze`/`test`.
- Tag-triggered release workflow that builds artifacts and publishes a GitHub
  Release (Linux + Android without secrets; macOS/Windows when signing secrets
  are set).
- `Cargo.lock` committed for reproducible application builds.
- Docs: Developer Guide, Transfer Protocol, runnable `quic_transfer` example,
  and the M7/M8 readiness + audit reports.

### Fixed
- Security/correctness hardening from a full-project audit: clipboard alloc DoS,
  Windows path-traversal in folder transfer, secure-link nonce-counter desync on
  retry, atomic (unique-temp) writes for trust/checkpoint/config, FFI shutdown
  stopping the daemon, cancel unblocking a pending transfer, poison-tolerant FFI
  locks, device-identity flapping across providers, and Flutter
  notify-after-dispose guards.

### Verified
- 217 Rust tests + 35 Flutter tests pass; clippy/fmt clean; examples compile and
  run byte-exact; Linux release build; live Android→Linux transfer. See
  [Stable Readiness](docs/STABLE_READINESS.md).

## M7 — Documentation, DX & open-source readiness
- README drift fixed; added Developer Guide and Transfer Protocol docs; a
  runnable `quic_transfer` example; CODE_OF_CONDUCT, SECURITY policy,
  SUPPORTED_VERSIONS, issue/PR templates, dependabot; four readiness reports.

## M6 — UI/UX polish
- Friendly, actionable error text (no internal detail leaks); screen-reader
  announcements on transfer cards; UX docs.

## M5 — Validation & hardening
- Full quality gate clean; folder edge-case tests; security review (no critical
  issues); benchmarks; Beta-readiness report; live Android→Linux transfer.

## M1–M4
- Rust engine, QUIC transport, RouteManager, discovery, FFI (M1–M3), Dart SDK +
  repositories, live-only Flutter, packaging. See [Migration](docs/MIGRATION.md).
