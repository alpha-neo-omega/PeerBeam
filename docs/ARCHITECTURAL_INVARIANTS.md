# Architectural Invariants

> **Status: CONSTITUTIONAL**
>
> This document is authoritative.
>
> Any feature, milestone, refactor, protocol change, or architectural decision must conform to this document.
>
> Changes require an explicit constitutional amendment.

---

This document defines the **hard constraints** and the **decision rules** that
govern PeerBeam's architecture. Invariants are non-negotiable: a change that
violates one is rejected until the invariant is amended. Decision rules guide
choices *among* conforming options.

The invariants are deliberately few and stable. They are derived from
[CLAUDE.md](../CLAUDE.md) and the current architecture
([ARCHITECTURE.md](ARCHITECTURE.md)); they describe what is already true and fence
it in for the next 5–10 years. They are not aspirational — every one holds in the
codebase today.

---

## Invariants

### I1 — Inward dependencies
`peerbeam-domain` depends on nothing internal and performs no IO and starts no
runtime. All internal dependencies point toward the domain. Adapters depend on
the domain only to implement its ports.
**Forbids:** a domain that imports tokio/sockets/Flutter/filesystem; a cycle back
into an adapter.

### I2 — Capability = port + adapter
Every capability is a **trait (port)** in `peerbeam-domain::port`, implemented by
an **adapter crate**, and wired at the composition root (`peerbeam-engine`).
Adding a capability does not edit the domain or engine internals beyond
registration.
**Forbids:** feature logic baked into the engine or a frontend; capabilities that
cannot be swapped or tested behind an interface.

### I3 — Peer-to-peer first
No feature requires a central hub or server for its common case. A relay, if
present, is optional, the lowest-priority route, and untrusted.
**Forbids:** designs that only work through a coordinating server (chat rooms via
a hub, cloud folders); making the relay mandatory or trusted.

### I4 — No mandatory cloud, no account, no tracking
PeerBeam never requires cloud infrastructure, an account, or a login, and never
ships analytics, telemetry, or tracking — in any build, ever.
**Forbids:** phone-home, usage beacons, remote feature flags, mandatory sign-in.

### I5 — End-to-end encrypted by default
Every peer channel is end-to-end encrypted and mutually authenticated
(`SecureLink`, X25519 + AES-256-GCM). Any relay or intermediary sees only
ciphertext.
**Forbids:** plaintext peer traffic; a relay or "helper" that can read content;
opt-in (rather than default) encryption.

### I6 — Trust-gated capabilities
Peer capabilities require TOFU-pinned trust (`TrustStore`). Sensitive actions
(auto-accept, remote commands, live clipboard, remote browse) require explicit,
revocable, per-capability consent — never inferred from "we connected once."
**Forbids:** acting on an unverified peer; treating connection or key-pinning as
authorization for a side effect.

### I7 — Frontend-agnostic, engine-first
Core logic lives in the engine and never depends on Flutter. Every capability is
reachable headless through the engine and the CLI. A GUI is one frontend among
several (CLI, future web, future iOS).
**Forbids:** business logic in Flutter/Dart; a capability that only exists in the
GUI; an engine that cannot run headless.

### I8 — One responsibility per crate
Each crate has a single responsibility; a new capability is a new crate, not a
new concern bolted onto an existing one. No God crates or God files.
**Forbids:** dumping unrelated logic into `peerbeam-engine` or `peerbeam-transfer`
because it is convenient.

### I9 — Versioned, negotiated wire protocol
All peer framing carries a protocol version negotiated at session start. Wire
changes are explicit and backward-compatible within a support window; there is no
silent wire drift.
**Forbids:** changing a frame layout without a version bump and negotiation;
assuming both peers run the same build.

### I10 — Streaming, bounded memory
No whole file or payload is loaded into RAM. Transfers, stores, and message
payloads stream with bounded buffers.
**Forbids:** `read_to_end` on user data paths; buffering an entire file/message to
send or persist it.

### I11 — Secure defaults, local-first
Every feature ships secure-by-default and functions offline. Data is local-first
and encrypted at rest; synchronization is opportunistic, never a precondition for
use.
**Forbids:** a feature that only works while online; insecure defaults "for
convenience"; unencrypted local stores of user content.

### I12 — Cross-platform parity is a merge gate
Windows, Linux, macOS, Android, and the CLI are first-class. A capability that
cannot meet a platform's constraint **documents the limitation** (as with Android
+ Tailscale) rather than weakening an invariant for everyone.
**Forbids:** desktop-only or mobile-only capabilities shipped as core; lowering a
security or architecture invariant to fit one platform.

---

## Decision Rules

Decision rules are not hard constraints; they break ties **among** implementations
that already satisfy every invariant. When two conforming designs exist, apply the
rules in order.

### DR1 — Constitutional Preservation
Prefer the implementation that best **preserves** the constitution (these
invariants and the [Vision](VISION.md)) over the long term, **even at higher
implementation effort**. Effort and speed never outweigh constitutional fit.
Extends CLAUDE.md: *"Never prioritize speed over architecture."*

### DR2 — Simplicity First
Among conforming, constitution-preserving options, choose the simplest: fewer
moving parts, fewer abstractions, less state. Do not add an abstraction until a
second real caller demands it. Clarity over cleverness; consistency over novelty.
Reject speculative generality.

### DR3 — Verification Over Assumption
Decisions rest on observed behavior, not assumption. Prefer the option you can
verify end-to-end (a test, a live transfer, a measured number) over the one that
merely *should* work. Claims of "done," "fixed," or "faster" require evidence.

---

## How to use this document

Before any feature, milestone, refactor, protocol change, or architectural
decision:

1. Check it against **I1–I12**. A violation stops the work until the invariant is
   amended (see below).
2. If several conforming designs remain, apply **DR1 → DR2 → DR3**.
3. Record non-obvious decisions where they belong: architecture in
   [FUTURE_ARCHITECTURE.md](FUTURE_ARCHITECTURE.md), scope in
   [FEATURE_CATALOG.md](FEATURE_CATALOG.md).

Amending an invariant is a deliberate, approved act — not a side effect of a
feature. To amend: stop the conflicting work, state the invariant and the
conflict, get explicit approval, then record it below with date, rationale, and
approval before implementation.

---

## Amendments

### A1 — A user-initiated release check, narrowly permitted (2026-08-20)

**Invariant amended:** I4 — *No mandatory cloud, no account, no tracking*, whose
Forbids line reads "phone-home, usage beacons, remote feature flags, mandatory
sign-in". Also narrows the corresponding permanent non-goal in
[VISION.md](VISION.md) — *"Not a surveillance surface."*

**The conflict.** A release check contacts a vendor-designated server. Read
plainly, "phone-home" covers it, and the request discloses an IP, an
approximate location, a version, and — by its timing — when the app is used.
That is a server-side record of a user, which is the thing this project exists
not to create.

**Rationale for amending rather than refusing.** A user running a build with a
known security fix missing is also a harm, and PeerBeam ships no auto-update.
The narrow reading — that I4's Forbids clause targets *unattended, ongoing*
disclosure (beacons, analytics, remote control of the client) rather than a
single request a person deliberately makes — is available and coherent. What
was not permissible was adopting that reading silently, which CLAUDE.md forbids.
This records it instead.

**What A1 permits:** exactly one HTTPS GET, made only when a person asks for it,
returning a version string that the app renders and acts on in no other way.

**Binding conditions.** All six hold together; a build that drops any one of
them is outside A1 and back in conflict with I4.

1. **Off by default, and opt-in per use.** No check on launch, on a timer, or as
   a side effect of anything else. (I11 — secure-by-default; "Forbids: insecure
   defaults 'for convenience'".)
2. **No identifiers.** No device id, install id, keypair-derived value, cookie,
   or persistent client state. Nothing beyond what a bare HTTPS GET
   unavoidably discloses. No custom headers, and no query string: the only
   header naming this product is the `User-Agent`, which the GitHub API requires
   and which is the bare word `PeerBeam` — no version, so the request does not
   disclose which build is asking either. *(Wording sharpened 2026-08-20: this
   condition first read "carries no PeerBeam-specific header", which a reader
   could take as forbidding the very `User-Agent` the API will not serve a
   request without. The intent — that nothing distinguishing this install or
   this build travels — is unchanged, and so is the shipped request.)*
3. **The response is inert.** A version string is displayed. No download, no
   install, no behaviour anywhere changes on the strength of what the server
   said. (I4 — "Forbids: … remote feature flags".)
4. **Never a precondition.** Offline is a normal state for this app, not an
   error. Failure is quiet, nothing is gated on the result, and nothing nags.
   (I11 — "functions offline … never a precondition for use".)
5. **Reachable from the CLI, not GUI-only.** (I7 — "Every capability is
   reachable headless through the engine and the CLI".)
6. **The security review says so.** [FINAL_SECURITY_REVIEW.md](FINAL_SECURITY_REVIEW.md)
   certified "no telemetry (confirmed by absence of any network telemetry
   client)". That sentence is amended in the same change; shipping a checker
   while it stood would leave a published security claim false.

**Approval:** granted by the repository owner, 2026-08-20.

**Scope.** A1 covers a release check and nothing else. It is not a precedent for
any other outbound request; a second one needs its own amendment.

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
