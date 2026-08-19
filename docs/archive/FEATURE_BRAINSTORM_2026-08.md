> **Archived, 2026-08-19.** A feature brainstorm, kept for provenance.
>
> **Most of this is now built** — presence, chat with attachments, clipboard
> history, remote folder browsing, device identity and QR pairing, per-device
> permissions, route selection, and folder sync with delta transfer and rename
> detection. Read it as history, not as a plan: the unbuilt remainder has been
> lifted into [FEATURE_ROADMAP.md](../FEATURE_ROADMAP.md), and where this
> document disagrees with the constitutional set, the constitutional set governs.

## 🔥 Features I'd prioritize

### 1. Real-time presence

Make the peer list feel alive:

```text
🟢 Alice's Laptop       Online · LAN
🟢 My Server            Online · Tailscale
🟡 Pixel 9              Away · Tailscale
⚫ Office PC             Offline
```

Show:

* Online/offline
* Last seen
* Connection type
* Latency
* Transfer speed
* Device OS
* Device name
* Direct vs relay route

This would make the application feel much more like a **device communication platform**.

---

### 2. Chat → attachments → transfer

This is one of the biggest opportunities.

Instead of having:

**Chat**

and separately:

**File Transfer**

make them one system.

For example:

```text
┌─────────────────────────────────────┐
│  💻 My Laptop                       │
├─────────────────────────────────────┤
│                                     │
│  You: Check this file               │
│                                     │
│  📦 project.zip                     │
│  1.8 GB                             │
│  ████████████░░ 82%                 │
│                                     │
│  You: Almost done                   │
│                                     │
│  🖼 screenshot.png                   │
│  2.4 MB  ✓                          │
│                                     │
├─────────────────────────────────────┤
│  📎  Type a message...       ➤      │
└─────────────────────────────────────┘
```

That makes PeerBeam feel like **private device-to-device messaging**, rather than a collection of utilities.

---

### 3. Group conversations

This could be very interesting.

```text
Home Network
 ├── 💻 Laptop
 ├── 📱 Phone
 ├── 🖥 Desktop
 └── 🗄 Server
```

Create:

**"Home Devices"**

and send a message/file to everyone.

Or:

**"Development Team"**

with several trusted peers.

You could make groups completely P2P rather than requiring a central messaging server.

---

### 4. Offline message queue

This would make your chat much more useful.

Example:

You send:

> "I'll send the backup tonight."

The other device is offline.

PeerBeam stores the encrypted message locally.

When the device returns:

```text
🔄 Device connected

Sending 3 queued messages...
✓ 3 messages delivered
```

You could do the same for files.

---

### 5. Smart route selection

You already have the beginnings of something valuable here.

Don't expose networking complexity to the user.

PeerBeam could automatically choose:

```text
              Peer
                │
       ┌────────┼────────┐
       ↓        ↓        ↓
      LAN    Tailscale   IPv6
       │        │        │
       └────────┼────────┘
                ↓
          Best available
             route
```

Then show:

> **Connected directly · 2.1 ms**

or

> **Connected through Tailscale relay · 48 ms**

Tailscale itself supports direct connections, DERP relays, and peer relays, so understanding and exposing route quality could become a particularly useful part of PeerBeam's UX. ([Tailscale][2])

---

# 🚀 Features that could make PeerBeam genuinely different

### 6. Remote clipboard history

Not just:

> Current clipboard

but:

```text
Clipboard History

10:42  https://github.com/...
10:39  cargo build --release
10:31  SSH command...
10:22  "Hello..."
```

Then:

**Right click → Send to Phone**

or

**Pin → Sync across devices**

This could become surprisingly useful.

---

### 7. Remote folder access

Allow:

```text
My Laptop
 ├── Documents
 ├── Downloads
 ├── Pictures
 └── Projects
```

from another trusted PeerBeam device.

But don't make it a normal SMB-like network drive initially.

Instead:

> **Browse → Download → Upload**

through the PeerBeam protocol.

This gives you secure remote access without requiring users to configure Samba/NFS/SSH.

---

### 8. Remote shell

This would be **very powerful**, particularly because you already have a CLI/Rust core.

Example:

```bash
peerbeam connect server
peerbeam shell server
```

Then:

```text
$ peerbeam shell home-server

Connected to home-server
Authenticated ✓
Encrypted ✓

server $
```

You could eventually support:

* Shell
* File operations
* Process information
* System information
* Service status

But make this **explicitly opt-in and permission controlled**.

---

### 9. Remote commands

Think of it as a secure automation layer.

From phone:

```text
My PC

[ Lock ]
[ Sleep ]
[ Shutdown ]
[ Restart ]
[ Run command ]
```

This is an area where KDE Connect already provides customizable commands, so you would want PeerBeam's implementation to emphasize its P2P/VPN/security model rather than simply copying the feature. ([KDE Connect][3])

---

# 🔐 Security features I'd strongly consider

Your security story could become one of PeerBeam's biggest strengths.

### 10. Device identity

Give every installation a cryptographic identity:

```text
Alice's Laptop

Peer ID:
7f31...92ac

Public Key:
ed25519:...

Trust:
✓ Verified
```

Allow users to verify devices via:

**QR code**

```text
Scan this QR code
       ↓
📱 ─────────→ 💻
       ↓
   Trust device?
```

---

### 11. Device permissions

This is extremely important if PeerBeam becomes more powerful.

Instead of:

> Trusted device = everything

use:

```text
Alice's Laptop

☑ Chat
☑ Send files
☑ Receive files
☑ Clipboard
☐ Remote shell
☐ Remote commands
☐ Browse folders
☐ Execute applications
```

This gives you a clean security model.

---

### 12. Time-limited trust

For example:

> Trust this device for **30 minutes**

or:

> Allow this device to receive files once.

Useful when connecting to someone else's laptop.

---

### 13. Security audit/log

Have:

```text
Security

11:32  Device authenticated
11:33  File received
11:35  Clipboard synchronized
11:40  Device disconnected
```

And perhaps:

```text
No server
No account
No telemetry
No central message store
```

That would be an excellent selling point.

---

# 🌍 Make the VPN functionality much more visible

I think this is one of your biggest opportunities.

Most people understand:

> AirDrop = nearby.

PeerBeam could become:

> **AirDrop for anywhere.**

For example:

```text
📱 Phone
       │
       │ Tailscale
       │
       ▼
🏠 Home PC
```

The devices can be physically anywhere while still behaving like peers.

Tailscale provides encrypted peer-to-peer networking over WireGuard and can fall back to relays when direct connectivity isn't possible. ([Tailscale][4])

### Even better: don't make PeerBeam depend on Tailscale

Support:

* LAN
* IPv6
* Tailscale
* Standard WireGuard
* Headscale
* Maybe ZeroTier
* Maybe direct public IPv6
* Eventually your own relay

Then PeerBeam becomes a **transport-agnostic application protocol**.

That's much more interesting architecturally.

---

# 🧠 A really interesting feature: PeerBeam Spaces

Imagine:

```text
PeerBeam

Spaces
─────────────────────
🏠 Home
💻 Development
📱 Personal
👥 Friends
🖥 Servers
```

A Space contains trusted peers.

For example:

### Home

```text
💻 Desktop
📱 Phone
💻 Laptop
🗄 NAS
```

You can:

* Chat
* Transfer files
* Sync clipboard
* Share folders
* Send commands
* See online status

This gives PeerBeam a **product identity**, rather than being a collection of networking features.

---

# 📦 Another big one: Sync

Eventually:

### Selective folder synchronization

```text
~/Documents/PeerBeam

Laptop  ←──────→  Desktop
              ↕
             Server
```

Features:

* Incremental sync
* Resumable
* Hash verification
* Conflict detection
* Version history
* Offline changes

This would start moving PeerBeam toward **Syncthing territory**, though you should avoid trying to reproduce all of Syncthing's functionality immediately.

---

# 📱 Mobile-specific features

For Android/iOS:

### Quick Share

Android quick action:

```text
Share → PeerBeam
          ↓
    Select peer
          ↓
       Send
```

### QR pairing

```text
Laptop:
[ QR CODE ]

Phone:
Scan → Authenticate → Connected
```

### Notification actions

```text
💻 Desktop sent photo.jpg

[View] [Save] [Reply]
```

---

# ⭐ My top 10 roadmap

If this were my project, I'd prioritize:

| Priority | Feature                       | Why                                      |
| -------- | ----------------------------- | ---------------------------------------- |
| 🔥 1     | Chat + file attachments       | Makes chat and transfer one product      |
| 🔥 2     | Presence/status               | Makes peers feel alive                   |
| 🔥 3     | QR device pairing             | Excellent UX + security                  |
| 🔥 4     | Device permissions            | Essential as capabilities grow           |
| 🔥 5     | Offline message/file queue    | Makes P2P chat reliable                  |
| 🔥 6     | Smart LAN/VPN route selection | Your strongest networking differentiator |
| ⭐ 7      | Clipboard history             | Very useful daily                        |
| ⭐ 8      | Remote folder browsing        | Powerful practical feature               |
| ⭐ 9      | Remote shell                  | Excellent for developers                 |
| ⭐ 10     | Group conversations           | Moves beyond 1-to-1 transfer             |

## And one feature I'd **not** prioritize yet

Don't immediately build:

> ❌ Cloud accounts
> ❌ Central messaging server
> ❌ Social profiles
> ❌ Public user discovery
> ❌ Cloud storage

Those would dilute what makes PeerBeam interesting.

Your strongest direction is almost the opposite:

**No account → no central server → trusted devices → direct connection → LAN/VPN → encrypted communication.**

That is a very coherent product philosophy.

### The bigger vision

I can see PeerBeam evolving into:

```text
                    PEERBEAM
                       │
        ┌──────────────┼──────────────┐
        │              │              │
      CHAT           DATA          CONTROL
        │              │              │
   Conversations    Files         Remote shell
   Groups           Clipboard     Commands
   Presence         Folders       Device info
   Offline queue    Sync          Automation
        │              │              │
        └──────────────┼──────────────┘
                       │
                 PEER PROTOCOL
                       │
          ┌────────────┼────────────┐
          │            │            │
         LAN       WireGuard    Tailscale
          │            │            │
          └────────────┼────────────┘
                       │
                Direct / Relay
                       │
                Encrypted QUIC
```

**That is a much more ambitious and differentiated project than "P2P file transfer."**
