# Vision

> **Status: CONSTITUTIONAL**
>
> This document is authoritative.
>
> Any feature, milestone, refactor, protocol change, or architectural decision must conform to this document.
>
> Changes require an explicit constitutional amendment.

---

## Why PeerBeam exists

Everyday collaboration between a person's own devices — and between people who
trust each other — has been quietly captured by the cloud. Sending a file to the
laptop across the room routes through a data center. Sharing a clipboard needs an
account. A "private" message is private only to the company that stores it.

PeerBeam exists to give that collaboration back to the devices that own it.
Trusted devices should talk **directly**, encrypted end-to-end, with **no
account, no cloud, and no tracking** — on any network, or none.

PeerBeam began as a zero-configuration file-transfer tool that works across LAN,
Ethernet, Wi-Fi, USB tethering, Tailscale, and headless servers without the user
configuring anything. That transport foundation — discovery, routing, mutual
authentication, an end-to-end-encrypted link — is the hard part, and it is built.
The vision is to grow **on top of that foundation**, not beside it.

## What PeerBeam is becoming

A **local-first, end-to-end-encrypted mesh of trusted devices** — a private
peer-to-peer productivity fabric. Files were the first message across the link.
Chat, clipboard, presence, synchronization, notes, and automation are more
messages across the **same** link. One identity, one trust model, one secure
session, many capabilities.

The organizing idea is a single abstraction:

```
Peer → PeerSession → Typed Message → Handler → Engine
```

A **PeerSession** is an authenticated, end-to-end-encrypted, route-managed channel
between two trusted devices. A file transfer is one **typed message** on that
channel. Chat, clipboard, presence, and sync are others. Every capability is a
message type and a handler; none invents its own parallel networking, trust, or
crypto. This is the spine of the whole platform, and it is **mandatory**: no
feature may bypass PeerSession.

## Problems PeerBeam solves

- **Cross-network reach without configuration.** Reach a device on the same LAN, a
  different subnet, a tailnet, or a tethered link — without the user knowing which
  path was used.
- **Trusted device-to-device collaboration** — files today; messages, clipboard,
  presence, and automation next — without a middleman.
- **Privacy by construction.** Nothing to leak because nothing is collected; no
  server to subpoena because there is no server.
- **Headless and scriptable.** Everything the GUI can do, the CLI can do — so
  servers, containers, and automation are first-class, not afterthoughts.

## Who it is for

Everyone with more than one device, and anyone who shares with people they trust —
served through a clean GUI. And, uniquely, the **power users, developers, and
sysadmins** for whom the engine-first, CLI-first design makes PeerBeam a scriptable
building block, not just an app.

## Principles

PeerBeam is: peer-to-peer first · privacy first · local first · end-to-end
encrypted · cross-platform · engine-first · CLI-first · modular · offline-capable ·
no mandatory cloud · no accounts · no tracking.

These are enforced as hard constraints in
[ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md). The vision says *why*;
the invariants say *what may never break*.

## What PeerBeam deliberately refuses to become

Non-goals are permanent unless amended. They are as much a part of the identity as
the features.

- **Not a cloud service.** No mandatory server, no hosted storage, no account, no
  login — ever. A self-hostable relay may exist, but only as an optional,
  untrusted, last-resort route (invariant I3/I5).
- **Not a surveillance surface.** No analytics, telemetry, tracking, or
  phone-home, in any build (I4) — narrowed once, by
  [A1](#a1--a-user-initiated-release-check-2026-08-20), for a release check that
  happens only when a person asks for it.
- **Not a social platform.** No hub-brokered group chat, feeds, discovery of
  strangers, or public rooms. Communication is between **trusted** peers (I3/I6).
- **Not a remote-control tool.** PeerBeam is not TeamViewer. Remote capabilities
  are limited to explicit, permissioned, narrowly-scoped actions — never arbitrary
  control of another machine (I6).
- **Not a general sync/backup company.** Opportunistic sync between *your* trusted
  devices, yes; a Dropbox competitor with cloud folders, no (I3/I11).
- **Not an enterprise suite.** No SSO, MDM, admin consoles, or mandatory policy
  servers. At most an *optional, local* policy file for those who want it.
- **Not a plugin free-for-all before it is earned.** Extension points are exposed
  only after the internal ports are proven and versioned (I2/I9).

## The measure of every decision

CLAUDE.md asks one question of every design: *"Would this still be the right
architecture if PeerBeam had 1 million users and 100 contributors?"* This document
adds a second: *"Does it extend PeerSession, honor the invariants, and keep a
promise we made about what PeerBeam refuses to be?"* If either answer is no,
redesign before writing code — or propose an amendment.

---

## Amendments

### A1 — A user-initiated release check (2026-08-20)

**Non-goal narrowed:** *"Not a surveillance surface."*

**Date:** 2026-08-20.

**Rationale.** A release check contacts a vendor-designated server, and read
plainly "phone-home" covers it. It is permitted only when a person asks for it —
`peerbeam check-updates`, or the button in About — never on a timer, at startup,
or in the background, and it sends no identifier of any kind. The full reasoning
and the six binding conditions are recorded once, in
[ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md#a1--a-user-initiated-release-check-narrowly-permitted-2026-08-20),
against invariant I4; this entry exists because the amendment narrows this
document too, and a non-goal that still read as absolute would be a published
claim the shipped build makes false.

**Approval:** granted with the feature it accompanies (v0.10.0).

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
