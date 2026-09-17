# PeerBeam v0.12.0 — Beta

Chat with a Tailscale peer, chat from a typed address, and notifications when a
message arrives. Plus a correctness sweep across every chat surface that found
forty defects, and one in the transport underneath them that was older and
worse than any of them.

If you use Tailscale, or use chat at all, upgrading is worth it.

## A message sent just before a session closed could vanish

Read this one even if you skip the rest.

`peerbeam chat send`, `group invite`, clipboard push and `peerbeam identify` all
dial a peer, send, and close. The send reported success once the frame was
*queued*, and the close then tore down the connection — discarding anything not
yet transmitted. About one time in twelve, the message simply never arrived and
you were told it had.

The control stream had been protected against exactly this, with a comment
explaining why. The streams carrying the actual payload never were. Links can
now finish their own stream and wait for the peer's acknowledgement without
closing the connection their neighbours share, and a closing session waits for
that rather than only asking for it.

This was present in v0.11.0 and every release before it.

## Chat with a Tailscale peer

It did not work. The flow that resolves a Tailscale device to the identity a
conversation can be filed under shipped in v0.11.0, and the device row withheld
the chat button for exactly the ids that need it — so it could not be reached
from the only screen that lists Tailscale devices.

Tapping chat now asks the address who is actually there — one dial, one
handshake, nothing sent — and opens the thread under the identity that answered.
The answer is remembered, so the conversation reopens without dialling again,
and reads while the peer is offline.

Two things that came with it:

- **First contact shows the pairing code.** That handshake pins the peer's key.
  The CLI has always printed the code and told you to compare it; the app pinned
  in silence. It now shows it, says plainly that recorded is not approved, and
  offers to forget the device — the only action that undoes the pin.
- **The dial no longer freezes the app.** It ran on the UI thread: up to 8
  seconds per resolved address, and two minutes if a peer answers and then
  stalls. That is not a spinner, it is a frozen window and an ANR on Android. It
  now runs off the UI thread and can be left.

## Chat from a typed address

"Send to address" offered files, a folder and a one-off text — and no way to
start a conversation, which is the thing you would want with a machine you have
to type the address of. It asks the address who is there and opens the thread.

## Notifications

A message arriving while PeerBeam is not what you are looking at now raises an
OS notification, on desktop as well as Android. Desktop had no notification
backend at all.

It stays quiet for a thread that is open **and** focused, for your own messages,
and for a row that merely settled from pending to sent. It carries at most 120
characters, because a notification is drawn on a lock screen and the app cannot
take it back. One per conversation, replaced rather than stacked, withdrawn when
you open the thread, and clicking one opens that conversation.

On Android messages get their own channel, so transfer noise and conversations
can be silenced independently.

## Also new

- **Group messages are searchable**, under the group's name, opening the group's
  transcript. They had been filtered out of search by accident, and the screen
  answered "No messages match" as a fact about your own disk.
- **Chat is readable by a screen reader** — one node per message, read as who,
  what, how it went, when.
- **Groups update live.** An invitation arriving while the Groups screen was
  open never appeared until you navigated away and back.

## Fixed

The sweep behind this release checked every chat surface adversarially. The ones
worth naming:

- **"Always accept" granted five capabilities behind a label about files** —
  files, messages, clipboard, presence and pipe — and said so only in a tooltip,
  which a phone shows on long-press and nobody sees.
- **Auto-accept could not be turned off once a device was blocked.** Revoking a
  device's Files permission greyed out your own standing consent while the bit
  stayed set; granting Files back later revived it.
- **The composer stayed live after you revoked a device's Messages permission**,
  so the message was typed, sent, and answered by a red bubble printing the
  engine's raw refusal.
- **Every group member became a phantom private conversation.** One group
  message populated a thread for everyone in it, each of which opened empty.
- **Forwarding a received file always failed on Android**, blaming a missing
  file while tapping the same row opened it.
- **Forward reported success when the engine had refused every message.**
- **A read receipt painted "read" over a file that was declined or failed.**
- **Read receipts stopped after the first batch**, so in a live back-and-forth
  the other device never learned anything more had been read.
- **A staged file's bytes could stay on disk for good** after a decline —
  silently, on Windows, where a file cannot be deleted while a handle is open.
- **A five-line composer had no reachable line break**: the return key had been
  turned into Send, and on a phone that is the only key there is.
- **Several screens claimed a conversation was empty before reading it**, and a
  read that *failed* was reported the same way.
- **Every group refusal read "Something went wrong. Please try again."** The
  engine had said "Family has nobody this device may message". That uncovered a
  wider one: `permission_denied` had no mapping at all, so every permission
  refusal in the app read that way.

## Upgrade note

Nothing to do. Trust, history, settings and chats carry over.

The Android build is still debug-signed, so it will not install over v0.11.0 —
uninstall first, which loses that device's identity, trust store and chat
history. Setting a release keystore fixes this permanently and costs nothing;
see [RELEASE.md](RELEASE.md).

## Under the hood

- 1836 Rust tests and 797 Flutter tests, with `clippy -D warnings` and
  `flutter analyze` clean on Linux, Windows, macOS and Android.
- `chat_ffi`'s test peers now bind OS-assigned ports. The fixed-port habit had
  been named as a flakiness source in a neighbouring test file and never fixed
  where it lived.

## Not verified

Notification delivery and the new SAF file staging are **unverified on Windows
and macOS** — built and analyzed there, not exercised by hand. The same is true
of the Windows file-handle behaviour the staging fix targets.

## Downloads

Linux (`.deb`, `.tar.gz`), Windows (portable `.zip`), Android (`.apk`/`.aab`),
macOS (universal `.dmg`) and the standalone CLI are attached below. Desktop and
CLI artifacts are **unsigned**.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
