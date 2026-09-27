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
   *(Host changed 2026-09-08: the check now GETs
   `peerbeam.pages.dev/releases.json` instead of the GitHub API, so the clause
   "which the GitHub API requires" no longer states why the header is sent —
   Cloudflare Pages serves a request without one. The header is kept anyway,
   still the bare word `PeerBeam`, because dropping it would not reduce what
   travels and a request with no User-Agent is itself distinctive. Nothing this
   condition forbids has changed: no identifier, no query string, no other
   header.)*
3. **The response is inert.** A version string is displayed. No download, no
   install, no behaviour anywhere changes on the strength of what the server
   said. (I4 — "Forbids: … remote feature flags".)
   *(Amended 2026-09-27 by [A3](#a3--downloading-a-release-the-user-asked-for-2026-09-27),
   which permits one further request — a download of one release artifact, at a
   person's request, verified against a project-signed checksum. The rest of
   this condition stands unchanged and A3 restates it: nothing installs,
   nothing executes, and no behaviour changes on the strength of what a server
   said. In particular the artifact address stays compiled in; A3's condition 2
   forbids taking it from the response, which is what keeps a served document
   from choosing what lands on a user's disk.)*
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

### A2 — Peer-held group conversations, without a hub (2026-08-27)

**Invariant amended:** none outright. I3 — *Peer-to-peer first* — is **read**,
not narrowed: its Forbids line targets "chat rooms via a hub", and what follows
is defined to have no hub. Also narrows the permanent non-goal in
[VISION.md](VISION.md) — *"Not a social platform."*, whose first clause reads
"No hub-brokered group chat". And it supersedes the design statement in
[SPACES.md](SPACES.md) that a Space "is not a group" and has "no shared roster,
no group key, no group id".

**The conflict.** Three documents refuse this today, at three different
strengths:

1. **VISION.md non-goal** — "No hub-brokered group chat". Read against a mesh
   design this is *satisfiable*: nothing is brokered. Read as shorthand for "no
   group chat", it is not.
2. **I3's Forbids** — "chat rooms via a hub". Same reading, same answer.
3. **SPACES.md** closes even the mesh variant explicitly: "someone would have to
   hold the roster and answer questions about it — and a device that brokers
   membership on everyone else's behalf is a hub, whatever it is called."

(3) is the real obstacle, and it is the one this amendment overturns. It is a
derived document, so it does not require an amendment of its own — but it
records a deliberate refusal, and overturning it silently is exactly what
CLAUDE.md forbids.

**Rationale for amending rather than refusing.** SPACES.md already names the
cost it accepted: *"there are no group replies."* A user who sends a file to
five people and receives five separate, mutually invisible replies is doing
group work with a tool that refuses to admit groups exist. The privacy property
being defended — that no member learns who else is a member — is real and worth
keeping *by default*, but it is a property of **Spaces**, and it does not follow
that a second, explicitly-joined construct may not exist beside them.

The hub objection is answerable on its own terms. A roster that every member
holds in full, and that every member re-sends to every other member directly, has
no broker: there is no device that others must ask, none whose absence stops the
conversation, and none that learns anything the others do not. That is a
replicated set, not a server. What it costs is not centralisation but
**metadata**: every member learns every other member. That is the honest price,
and it must be stated to the user rather than buried.

**What A2 permits.** A **Group**: a named, explicitly-joined set of trusted
devices, distinct from a Space, in which every member holds the full roster and
every message is sent directly to each member.

**Binding conditions.** All hold together; a build that drops any one is outside
A2 and back in conflict with I3 and the VISION non-goal.

1. **No hub, no host, no creator privilege.** Every member holds the complete
   roster. No member is required for the group to function, and a member going
   offline degrades nothing but that member's own delivery. Any design in which
   one device answers membership questions for others is outside A2.
2. **No relay.** Group messages take the same routes as 1:1 messages and are
   never relayed through a third member. N members means N direct sends.
   (I3 — a relay stays "optional, the lowest-priority route, and untrusted".)
3. **Per-member permission still gates every send.** Membership grants nothing.
   A message to a group passes the same `may_exchange_chat` check per recipient
   that a hand-addressed message would, and a member who has revoked chat simply
   does not receive it — and is named, not silently dropped. (I6.)
4. **Joining is explicit, on both sides.** No device is added to a group without
   an action by its own user. An invitation is an offer, never an enrolment;
   there is no mechanism by which a third party's device joins anything on its
   owner's behalf.
5. **The metadata cost is stated in the UI, at the point of joining.** The user
   is told, in plain words, that every member will learn who every other member
   is. A group that leaked the membership silently would be the surveillance
   property this project exists to refuse, merely relocated.
6. **Spaces are unchanged and stay the default.** A Space remains local, roster
   -less and invisible to peers. A Group is a second construct with a different
   trade, chosen deliberately; neither is silently converted into the other, and
   the fan-out send that Spaces already perform is not renamed "group chat".
7. **Reachable from the CLI, not GUI-only.** (I7.)
8. **End-to-end encrypted per member.** Each direct send keeps the existing 1:1
   guarantees; no group key is introduced that would weaken them or create a
   shared secret whose compromise exposes the whole conversation. (I5.)

**Approval:** granted by the repository owner, 2026-08-27.

**Scope.** A2 covers peer-held group conversations and nothing else. It is not a
precedent for feeds, public rooms, discovery of strangers, or any
server-mediated feature; each of those remains refused by the same non-goal.

### A3 — Downloading a release the user asked for (2026-09-27)

**Invariant amended:** **A1's binding condition 3** — *"The response is inert. A
version string is displayed. No download, no install, no behaviour anywhere
changes on the strength of what the server said."* Because A1 states that all
six of its conditions hold together and that a build dropping any one is "outside
A1 and back in conflict with I4", amending condition 3 reopens **I4 — No
mandatory cloud, no account, no tracking**. Also engages **A1's Scope clause** —
*"It is not a precedent for any other outbound request; a second one needs its
own amendment"* — of which this is the first invocation. Also narrows the
permanent non-goal in [VISION.md](VISION.md) — *"Not a surveillance surface."* —
further than A1 narrowed it.

**The conflict.** Three documents refuse this, at three strengths:

1. **A1 condition 3** forbids it in as many words: "No download, no install."
   This is not a reading that can be argued around; it is the clause itself.
2. **A1's Scope clause** requires a fresh amendment for any second outbound
   request. Fetching an artifact is a second request, to a second origin
   (`github.com`, where the assets live) with a second set of disclosures.
3. **[SECURITY.md](SECURITY.md)** publishes, as a security claim about the
   shipped build: *"A `Release` is a version string and a URL; nothing
   downloads, installs or changes behaviour on the strength of what the server
   said."* Shipping a downloader while that sentence stands would leave a
   published security claim false — the same defect A1's own condition 6 was
   written to prevent.

There is a fourth consideration, which is not a refusal but is the real risk.
Today the update check cannot send anyone anywhere: the download URL is compiled
into the binary and the manifest's own URL field is deliberately ignored. A
downloader turns a network response into something that writes bytes to the
user's disk. That is a supply-chain path which does not currently exist in this
project at all, and creating one deserves more care than the feature's size
suggests.

**Rationale for amending rather than refusing.** The argument that granted A1
applies here with more force, not less. A1 reasoned that *"a user running a build
with a known security fix missing is also a harm, and PeerBeam ships no
auto-update."* A1 solved half of that: the user now learns a newer release
exists. It left the other half exactly where it was — the user is handed a link
to a page listing **twenty-four assets** and must choose correctly between
`peerbeam-0.12.0-amd64.deb`, `peerbeam-0.12.0-arm64.deb`,
`peerbeam-0.12.0-x86_64.rpm`, `peerbeam-0.12.0-aarch64.AppImage`,
`peerbeam-0.12.0-linux-x64.tar.gz` and nineteen others. Choosing wrong is not an
edge case; it is the ordinary outcome for anyone who does not already know their
own architecture and packaging format. A told-but-stranded user is the state A1
left behind, and it is not obviously better than an uninformed one.

The decisive argument is that **this is safer than the status quo, not riskier.**
A user who today follows the link and downloads in a browser gets *no integrity
check whatsoever*. Nobody verifies `SHA256SUMS` by hand. A download that refuses
to hand over bytes which do not match a signature the project made is strictly
stronger than the thing it replaces. If A3 is refused, the browser path remains —
and it remains unverified.

What must **not** follow from this is auto-update. `CLAUDE.md` lists "Automatic
updates (future)" among its user-experience goals, and that is a different
feature with a different threat model: it requires the app to replace its own
executable, which means elevation, silent execution of fetched code, and a
rollback story. A3 is deliberately much smaller, and condition 4 exists to keep
it that way. Approving A3 is not approving auto-update.

**What A3 permits.** **One HTTPS GET of one release artifact, for the platform
the app is running on, made only when a person asks for it, written to a
location that person chose, and verified against a project-signed checksum
before it is offered to them.** PeerBeam does not install it, execute it, or act
on it in any other way.

**Binding conditions.** All hold together; a build that drops any one of them is
outside A3 and back in conflict with I4 and with A1.

1. **Opt-in per use.** No download on launch, on a timer, on the heels of a
   check, or as a side effect of anything else — and specifically no
   "pre-fetching in the background so it is ready", which is the form this
   always tries to take. One human action, one download. (I11 — "Forbids:
   insecure defaults 'for convenience'".)

2. **The artifact URL is compiled in, never served.** The manifest may say
   *which version* is newest. It may not say *where to fetch anything*. The
   artifact address is constructed in the app from a compiled-in host, the
   version string, and the platform's known asset-name pattern. HTTP redirects
   that leave the compiled-in host are refused rather than followed. A served
   document must never be able to choose what gets written to a user's disk —
   this is the property A1 protected by ignoring the manifest's URL field, and
   it is the single most important line in this amendment.

3. **Integrity is verified against a project signature before the file is
   usable.** The release's `SHA256SUMS` must be signed with a key whose public
   half is compiled into the binary; PeerBeam verifies the signature, then
   verifies the artifact's digest against the signed list, before the file is
   presented to the user as a download. A missing signature, a bad signature,
   an absent entry, or a digest mismatch deletes the downloaded bytes and says
   what happened. **There is no "unverified but probably fine" path, no
   override, and no prompt offering one** — an integrity check a user can click
   past is decoration. An unsigned checksum does not satisfy this condition:
   fetched from the same origin as the artifact, it proves only that the file
   arrived intact, which TLS already proved.

4. **PeerBeam never installs, never executes, never elevates.** It writes one
   file and stops. No replacing the running binary, no invoking an installer, no
   privilege prompt, no restart-to-apply, no marking the file executable. The
   user installs it exactly as they do today. (VISION — *"Not a remote-control
   tool"*; and the boundary that separates A3 from auto-update.)

5. **Never a precondition, and never a nag.** Offline stays an ordinary state.
   A failed or refused download is quiet and costs nothing. Nothing in the app
   is gated on having updated, no badge accumulates, and no dialog returns
   uninvited. (I11 — "functions offline … never a precondition for use"; carries
   A1 condition 4 forward unchanged.)

6. **No identifiers, and the new disclosure is stated plainly.** The artifact
   request carries what A1 condition 2 permits and nothing more: no device id,
   no install id, no cookie, no query string, no header naming this build. It
   does unavoidably disclose **which platform and architecture the user is on**,
   because that is which file is being asked for, and it discloses it to a
   second origin (the release host) that the check alone never contacted. That
   is a real widening of what A1 permitted and it is recorded here rather than
   left to be discovered in a later audit.

7. **Reachable from the CLI, not GUI-only.** A `peerbeam download-update` or
   equivalent, with the same verification and the same refusals. (I7 — "Every
   capability is reachable headless through the engine and the CLI".)

8. **The published security claim is amended in the same change.**
   [SECURITY.md:738](SECURITY.md) states that "nothing downloads, installs or
   changes behaviour on the strength of what the server said". That sentence
   becomes false the moment this ships and is corrected in the commit that
   lands the feature, not afterwards. (Follows A1 condition 6, which exists for
   exactly this reason.)

   [FINAL_SECURITY_REVIEW.md:82,88](FINAL_SECURITY_REVIEW.md) needs **no**
   change: its claims are "no telemetry, no analytics" and "no telemetry
   client, no beacon, nothing reporting usage", and a download a user asked for
   is none of those. Checked rather than assumed, because A1 was granted partly
   on the strength of noticing that its own feature falsified a certification
   sentence there.

**Approval:** granted by the repository owner, 2026-09-27.

**Scope.** A3 covers fetching one release artifact at a user's request and
nothing else. It is **not** a precedent for: automatic or background updates;
installing, executing or replacing any binary; delta or patch downloads;
downloading anything other than a release of PeerBeam itself; or any third
outbound request, which would need its own amendment exactly as this one did.

<!--
Future amendments must include:
- Date
- Rationale
- Approval
-->
