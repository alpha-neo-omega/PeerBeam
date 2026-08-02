# PeerSession Versioning

> **Status: DERIVED specification** (stability-critical). Conforms to the
> constitutional documents; **specification only — no production code**. Companion to
> [PEERSESSION_SPEC.md](PEERSESSION_SPEC.md) and [MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md).

This document defines how the protocol changes **without breaking peers** — the
machinery that satisfies invariant **I9** (versioned, negotiated wire; no silent
drift). It closes the current gap: today's wire has no version byte and is governed
only by the release version ([TRANSFER_PROTOCOL.md](TRANSFER_PROTOCOL.md §Compatibility)).

---

## 1. Version number

Protocol version is **`major.minor`**, exchanged in the `SessionHello` (control
channel) during negotiation.

| Part | Meaning | Compatibility |
|---|---|---|
| **major** | Framing/semantics contract (frame layout, auth, sealing, id meanings) | Peers **must share a major** to communicate |
| **minor** | Additive extensions (new ChannelTypes, MessageTypes, optional flags, feature flags) | **Backward-compatible**; any minor talks to any minor of the same major |

The protocol version is **independent** of the app release version and of the FFI ABI
version (`pb_abi_version`). An app release states which protocol major/minor and which
ABI it speaks.

## 2. Minor compatibility (the common case)

Within one major, everything is additive and backward-compatible:

- New **ChannelTypes** — an older peer never advertises them, so they are simply never
  opened toward it (§4).
- New **MessageTypes** — shipped with the `OPTIONAL` flag so older peers ignore them
  ([MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md §6–7)).
- New **feature flags** — unknown flags are ignored (§5).

Result: a `1.7` peer and a `1.2` peer interoperate at the intersection of what they
both support, with no negotiation failure.

## 3. Major versions (rare, deliberate)

A **breaking** change — altering the frame layout, the auth handshake, the sealing
construction, or the *meaning* of an existing id — requires a new major. Rules:

- Both peers negotiate the **highest common major**. No common major →
  `VersionIncompatible` and the session closes cleanly (fail-closed).
- A breaking change is introduced only when it cannot be expressed additively.
  Renumbering or reusing an id is always breaking and always a major (ids are never
  reused — retired ids stay reserved).
- Implementations **support the prior major alongside the new one for a documented
  window** (dual-major), so the network can migrate before the old major is dropped.

## 4. Capability negotiation (the real compatibility mechanism)

Version numbers gate the *framing*; **capabilities** gate *behavior*. In
`SessionHello` each peer advertises:

- the ChannelTypes it supports, and
- per-capability **feature flags**.

The agreed set is the **intersection**; a peer only ever opens or emits what the other
advertised. This is why new capabilities never break old peers — behavior is chosen
from advertised capabilities, not inferred from a version number. (Sniffing behavior
from a version string is explicitly *not* how PeerSession works.)

## 5. Feature flags

Per-capability booleans negotiated within a channel type — e.g. Chat `{reactions,
receipts}`, Transfer `{compression, delta}`. Rules:

- **Additive and optional.** Absence means "not supported"; the feature is simply not
  used.
- **Unknown flags are ignored**, so a newer peer advertising a flag an older peer has
  never heard of degrades gracefully.
- Flags are the fine-grained tool; a whole new capability is a new ChannelType, a
  refinement of an existing one is a flag (DR2 — smallest change that works).

## 6. Migration strategy

The path for any non-trivial change, ordered to avoid flag days:

1. **Introduce additively** — ship the new MessageType/ChannelType/flag as `OPTIONAL`
   in a **minor** bump. Old peers ignore it; new peers use it when both advertise it.
2. **Adopt** — let the change propagate across releases; measure real interop
   (DR3), recorded in the compatibility matrix.
3. **Flip defaults** — once support is widespread, make the new path the default while
   still tolerating the old (still same major).
4. **Break only if unavoidable** — a change that cannot be additive bumps **major**,
   ships with dual-major support, and drops the old major only after the support
   window.

**Pre-1.0 latitude:** before `1.0` the wire may change freely, but **every** change
still goes through version + capability negotiation, so the compatibility machinery is
exercised from day one and the `1.0` freeze inherits a proven mechanism (DR3).

## 7. Compatibility strategy and support window

- Each release **declares** the protocol majors/minors and the FFI ABI it
  interoperates with, plus the oldest it still accepts.
- Compatibility is a **tested claim**, not an assumption (DR3): interop is verified
  across the supported window and recorded in the compatibility matrix.
- The **FFI ABI** (`pb_abi_version`) evolves independently so a Flutter build and an
  engine build verify compatibility at load, decoupled from wire changes.
- **Deprecation** retires ids and majors on an announced schedule; nothing is silently
  removed (I9).

## 8. What versioning may never do

- Never change an id's meaning without a major bump (I9).
- Never drop a supported major inside its window.
- Never require a central server or account to negotiate (I3/I4) — negotiation is
  peer-to-peer in `SessionHello`.
- Never weaken the auth/sealing contract to ease a migration (I5/I6); a security
  change is a deliberate major, not a convenience.

---

*Message ids and ranges: [MESSAGE_REGISTRY.md](MESSAGE_REGISTRY.md) · Session behavior:
[PEERSESSION_SPEC.md](PEERSESSION_SPEC.md).*
