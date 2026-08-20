# Waking your own devices

The commonest reason one of your machines is unreachable is not the network. It
is asleep.

```
peerbeam wake set pb-3f9a12cd48b1 aa:bb:cc:dd:ee:ff
peerbeam wake send pb-3f9a12cd48b1
peerbeam list                       # watch for it to appear
```

---

## What this can and cannot do

**It works on the local network only.** A Wake-on-LAN packet is a broadcast, and
a broadcast has no destination to be routed to. It does **not** travel over
Tailscale, a VPN, or the internet — not for want of plumbing, but because those
are point-to-point paths. No amount of work on PeerBeam's side changes that, so
nothing here implies otherwise.

**Nothing confirms a wake.** The protocol has no reply. `wake send` exiting
successfully means the packet left this machine; it is not a claim that anything
received it, and PeerBeam will not print one. The only real confirmation is the
device turning up in `peerbeam list` or on the device list in the app.

**It needs the device's hardware address**, which PeerBeam cannot discover for a
machine that is switched off. Read it off the device once:

| | |
|---|---|
| Linux | `ip link` — the `link/ether` line |
| macOS | System Settings → Network → Details → Hardware |
| Windows | `ipconfig /all` — "Physical Address" |

**The device must be configured to listen.** Wake-on-LAN is usually off by
default and is enabled in the firmware (often "Wake on LAN", "Power on by PCI-E"
or similar) and sometimes in the OS network adapter settings as well. PeerBeam
cannot turn it on remotely — that is the point of it being a firmware setting.

---

## Who may be woken

Only a device you have **approved**. Sending a magic packet is an action on
someone else's hardware, so invariant
[I6](ARCHITECTURAL_INVARIANTS.md) applies: a pinned-but-unapproved device — one
that has merely connected once — is refused by name.

```
$ peerbeam wake send pb-stranger
error: pb-stranger is not an approved device, so PeerBeam will not wake it
```

Recording an address grants nothing else and tells the device nothing. It is a
note this machine keeps.

---

## Repeating a wake

The packet is idempotent: a machine already awake ignores it, and a single
broadcast can be dropped with nothing to notice the loss. Running `wake send`
again is therefore safe and occasionally useful. PeerBeam does not repeat it for
you, because only you know whether someone is waiting.

---

## Privacy

The packet contains the target's hardware address and nothing else — no
identifier of this device, no version, no content. It never leaves the local
segment. Nothing about waking a device is reported anywhere ([I4](ARCHITECTURAL_INVARIANTS.md)).
