# DRAFT — Amendment A3

> **Status: NOT ADOPTED. This file is a proposal, not an authority.**
>
> Nothing in this document is in force. It is written to be read, argued with,
> and either approved or refused by the repository owner. If approved, the
> `### A3` section below moves verbatim into
> [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md#amendments), the
> mirror moves into [VISION.md](VISION.md#amendments), the
> [SECURITY.md](SECURITY.md) paragraph named in condition 8 is corrected in the
> same commit, and this file is deleted.
>
> **No code may be written against this draft while it is a draft.**

There is a hard prerequisite, stated here so it is not discovered late:
**condition 3 requires a signed checksum, and PeerBeam does not sign one today.**
`SHA256SUMS` ships with every release, but it is unsigned, and a checksum served
from the same origin as the file it describes proves only that the file arrived
intact — not that it is the file the project built. A3 cannot ship before that
signing exists. See *Prerequisites* at the end.

---

### A3 — Downloading a release the user asked for (proposed)

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

**Approval:** *pending — not granted.*

**Scope.** A3 would cover fetching one release artifact at a user's request and
nothing else. It is **not** a precedent for: automatic or background updates;
installing, executing or replacing any binary; delta or patch downloads;
downloading anything other than a release of PeerBeam itself; or any third
outbound request, which would need its own amendment exactly as this one did.

---

## Prerequisites (not part of the amendment text)

A3 cannot be implemented the day it is approved. Condition 3 requires release
engineering that does not exist yet:

1. **A signing key.** Minisign or `age`-style signing is enough and is free —
   this is *not* the same problem as the Authenticode and Apple Developer ID
   certificates the project currently lacks, and it does not depend on them. The
   secret key must live somewhere with a custody story; the public key is
   compiled into the binary and is therefore pinned for the life of every build
   that ships with it.
2. **A signing step in `release.yml`.** `SHA256SUMS` is already generated over
   exactly the uploaded files (`.github/workflows/release.yml:341-352`); it needs
   signing and the signature uploading beside it.
3. **A key-rotation answer.** A pinned public key means a compromised or lost
   secret key strands every already-shipped build. The design needs to say what
   happens then, before the first key ships rather than after.

## Open questions for the owner

These are decisions the amendment deliberately does not make:

- **Custody of the signing key** — where the secret half lives, who can use it,
  and whether CI may hold it at all.
- **Unsigned platforms.** The Windows MSIX and the macOS DMG currently ship
  unsigned and un-notarized, because the signing secrets are not configured. A
  verified *download* is achievable without them, but it delivers a file the OS
  will still refuse to run without the user clearing quarantine or a SmartScreen
  warning. Whether the download is offered on those platforms before code
  signing exists is a judgement call.
- **Default download location**, and whether the user is asked every time or
  once.
- **Whether A3 is worth it at all** given the prerequisites. Refusing is a
  coherent answer: the browser path keeps working, and the effort could go to
  code signing instead — which would improve the status quo for every user,
  including those who never update in-app.
