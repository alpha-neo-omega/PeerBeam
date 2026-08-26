# PeerBeam v0.10.0 — Beta

Trust you can put a clock on, devices you can group and wake, messages that
delete themselves — and a long list of fixes, several of which were quiet enough
to be worth reading about.

## Trust that runs out

```
peerbeam trust approve laptop --for 30m
```

Also `45s`, `2h`, `7d`. When the window closes the device is back to being merely
pinned — it may nothing — and `trust list` shows it as `expired` with how long
ago. Visibly expired, never silently missing.

**Nothing has to be running for that to happen.** There is no sweeper and no
daemon: the deadline is checked wherever trust is *read*, and every gate already
re-reads the store for each operation. A sweep that has not run yet would be a
device still trusted after its window closed, which is exactly the gap this must
not have. A fresh process, a reopened store, and a machine that slept through the
whole window all reach the same verdict.

The **pin survives the window**, so a key change is still caught as a possible
MITM. `trust revoke` is what forgets a device. Re-approving an expired one gives
back the permissions it was actually left with, not the five that a fresh
approval starts from.

## Spaces, and your own devices

Name a set of devices you already trust and send or message all of them at once:

```
peerbeam space create work
peerbeam space add work laptop
peerbeam space send work notes.pdf
```

A space is a label over existing trust — it grants nothing on its own, and a
device you revoke is gone from every space that named it.

`peerbeam trust mine <device>` marks a machine as yours. My Devices is then the
short list you actually reach for, separate from every printer and phone that has
ever handshaked with you.

## Wake a device that is asleep

```
peerbeam wake set desktop 3c:7c:3f:aa:bb:cc
peerbeam wake send desktop
```

A magic packet on the local network, to a MAC you recorded yourself. It reports
what it sent, not that the machine woke — nothing on the network can promise
that.

Two things must both hold: you approved the device, and you recorded its MAC.
Approval rather than the "my devices" mark on purpose — a device you marked as
yours but have not approved is not one this should be able to reach, and
recording a MAC by hand is itself the deliberate act that makes a wake possible.

## Disappearing messages, and replies

```
peerbeam chat retention laptop --after 30m
```

Off by default. Applied when a conversation is *read*, for the same reason trust
expiry is: a message that should be gone is gone whether or not anything has run
since.

A message can now answer another. A reply whose parent has expired shows as a
reply to a message that is no longer there, rather than inventing the quoted text
— keeping a copy would defeat the retention it was subject to.

## A ceiling on outbound speed

`transfer.max_send_bytes_per_sec` (`0`, the default, is unlimited). A token
bucket, so the number is an average rather than a cadence: a transfer that has
been idle gets credit for the time it did not use, up to one second's worth.

## Round-trip time in the device list

Taken from QUIC's own smoothed estimate, so it costs nothing extra to know which
route you actually got.

## Update checks — only when you ask

`peerbeam check-updates`, and a button in About. Nothing checks on its own, on a
timer, or at startup. Offline is not an error: it says it could not reach the
feed and exits 0.

This is the first amendment to the architectural invariants, which forbid
phoning home. It is narrowed to a user-initiated check under six binding
conditions rather than quietly reinterpreted — see
`docs/ARCHITECTURAL_INVARIANTS.md`.

## Shared folders and logs, in the app

Both were reachable only by editing a config file. The logs the engine has always
kept are now readable where you are.

## Things the app could not do, and now can

A sweep for the shape of two bugs reported from ordinary use — the app quietly
cannot do something reasonable, and nothing on screen says so — turned up
nineteen more. The ones you are most likely to have hit:

**You can copy a message.** Chat text was not selectable, because press-and-hold
is how messages are selected, so a message someone sent you could be read and not
kept. Select one or several and **Copy text** takes them in order.

**You can copy a device's fingerprint.** It is shown shortened to keep the row
readable and could not be selected, so the one thing a fingerprint is for —
comparing it with the other device over some other channel — was the one thing it
could not do. Tapping it copies the whole value.

**You can reach a device's shared folders without knowing its address.** Browsing
existed, and the only way in was the menu of a device you had typed an IP address
for. It is on the Devices menu now, beside Wake.

**A navigation button works while a screen is open over it.** Opening a chat from
a device and then pressing Home did nothing until you pressed back.

**Searching for a device follows discovery.** It froze at the instant it opened,
so a device found a second later never appeared — and opening it in the first
seconds after launch showed "No matches" with nothing typed.

**A device you can wake is not forgotten while it sleeps.** Devices you marked as
your own, or gave a hardware address, are kept in the list however long they have
been away; everything else still ages out. And the address is filled in for you
rather than asked for again — `peerbeam wake list` shows what is recorded.

**Failures say so.** A revoke that did not happen looked exactly like one that
did. A note the app could not save vanished with your text in it. Renaming this
device, or changing where files are saved, reverted in silence. A tap on a file
while browsing did nothing at all.

## Fixes worth naming

**Anyone who could reach the port could change your clipboard.** Inbound clips
were applied with no trust check at all — not approval, not the Clipboard
permission — while the permission the app offers to revoke governed only what
this device *sends*. Since first contact pins a stranger as known-but-not-
approved, completing a handshake was enough. Both directions are gated now.

**A reconnect kept its channels only if it won a coin flip.** One transport loss
is noticed twice — the control link fails, and every channel's stream ends — and
whichever arrived first decided whether the session resumed with its channels or
without them, for good. Presence and clipboard hold a channel for the life of a
session, so on the losing side of the flip they went quiet after any blip, with no
error, because the session really was up.

**A receiver could throw away the verdict it had just written.** A transfer ends
with the receiver sending a checksum verdict the sender is waiting to read. On a
connection owned by that one transfer, letting it go discarded anything not yet
on the wire — so the sender reported a failure for a file that had arrived
complete and verified. On a two-core machine that was two runs in three, which is
what a small VPS or a CPU-limited container looks like.

**A file sync could not delete stayed deleted anyway.** Every failed `remove_file`
counted as success, so the file was indexed as gone while still on disk: this
device stopped offering a file it had, and no later scan put it back.

**Sync could stop fetching, silently**, after a few hundred requests — each opened
a channel and none returned one, so a session hit its channel limit and simply
stopped.

Also: wire paths no longer carry the sending host's separator (a Windows device
put `photos\june.jpg` on Linux disks as one filename); a concurrent reader can no
longer lose a write on Windows; logs no longer record every dependency's trace; a
settings file with one bad byte is no longer replaced by defaults; "no devices
found" no longer hides an engine that never started; renaming this device no
longer kills discovery silently; a chat file that arrived no longer stays
"waiting" forever; a chat message that could not be sent no longer disappears;
and on Android, background receive is on by default, a timed-out service stops
itself, and the multicast lock follows discovery rather than the notification, so
turning the notification off no longer kills mDNS.

The full list is in
[CHANGELOG.md](https://github.com/alpha-neo-omega/PeerBeam/blob/main/CHANGELOG.md).

## Verified

1727 Rust tests, 572 Flutter tests and 18 Kotlin tests pass; `cargo fmt`, `cargo clippy -D
warnings` and `flutter analyze` are clean.

CI runs the Rust suite and the Flutter suite on every commit, and **builds** the
desktop app for Windows and macOS. The Linux GUI is tested but not built there —
its packaging runs on the release tag.

**Not verified:** Android has no automated test coverage beyond its unit tests —
it is built on a tag, not exercised on a device. iOS and Web are not supported
yet.

## Install

Downloads below. On Linux:

```
curl -LO https://github.com/alpha-neo-omega/PeerBeam/releases/latest/download/peerbeam-linux-x64
chmod +x peerbeam-linux-x64
```

macOS and Windows GUI builds are unsigned — your OS will warn you. Signing awaits
certificates.
